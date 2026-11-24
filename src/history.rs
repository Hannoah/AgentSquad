//! Automatic work history: squad appends each finished task (including full
//! conversation content without duplicates) into a daily markdown file in
//! `.squad/log/` (`<YYYYMMDD>-auto.md`).

use crate::config::plain_path;
use crate::json::{self, JsonValue};
use crate::ledger::{changed_files, git, utc_stamp};
use crate::ui::uptime_label;
use std::{
    env, fs,
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Shortest task worth an entry; anything quicker is typing, not work.
pub const MIN_TASK: Duration = Duration::from_secs(2);

/// Serializes daily log appends and cursor updates across background workers.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// What squad saw of one task.
pub struct Task {
    pub project: String,
    pub path: PathBuf,
    pub command: String,
    pub started: SystemTime,
    pub took: Duration,
    /// How it ended: "task complete", or why the session ended mid-task.
    pub outcome: String,
    /// User prompts typed to the PTY during this task (fallback for non-Claude agents).
    pub prompts: Vec<String>,
    /// Full new PTY output captured since the previous recorded task (no duplicates).
    pub output: String,
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn local_date_time(t: SystemTime, offset_secs: i64) -> (String, String) {
    let shifted = (secs(t) as i64).saturating_add(offset_secs).max(0) as u64;
    let stamp = utc_stamp(shifted);
    (
        stamp.get(..10).unwrap_or("1970-01-01").to_string(),
        stamp.get(11..19).unwrap_or("00:00:00").to_string(),
    )
}

/// Daily auto log filename for `ended` in local time (`<YYYYMMDD>-auto.md`).
pub fn daily_name(ended: SystemTime, offset_secs: i64) -> String {
    let (date, _) = local_date_time(ended, offset_secs);
    format!("{}-auto.md", date.replace('-', ""))
}

/// File header written once at the top of a new daily log file.
pub fn daily_header(project: &str, ended: SystemTime, offset_secs: i64) -> String {
    let (date, _) = local_date_time(ended, offset_secs);
    format!("# {project} - {date}\n")
}

/// One finished task rendered as a markdown section for appending to the daily log.
pub fn render_section(
    t: &Task,
    files: &[String],
    commits: &[String],
    transcript: Option<&str>,
    offset_secs: i64,
) -> String {
    let ended = t.started + t.took;
    let (date, time) = local_date_time(ended, offset_secs);
    let took = uptime_label(t.took.as_secs());
    let mut out = format!(
        "\n---\n## {date} {time} · {} (`{}`) · {} ({took})\n\n- Date: {date}\n- Time: {time} (took {took})\n- Agent: {} (`{}`)\n",
        t.project,
        t.command,
        t.outcome,
        t.project,
        t.command,
    );
    if !files.is_empty() {
        out.push_str("\n### Files changed\n\n");
        for f in files {
            out.push_str(&format!("- {f}\n"));
        }
    }
    if !commits.is_empty() {
        out.push_str("\n### Commits\n\n");
        for c in commits {
            out.push_str(&format!("- {c}\n"));
        }
    }
    if let Some(md) = transcript.map(str::trim).filter(|s| !s.is_empty()) {
        out.push('\n');
        out.push_str(md);
        out.push('\n');
    } else {
        if !t.prompts.is_empty() {
            out.push_str("\n### User\n\n");
            out.push_str(&t.prompts.join("\n"));
            out.push('\n');
        }
        let trimmed = t.output.trim();
        if !trimmed.is_empty() {
            // Use a fence longer than any backtick run in `output` so embedded code blocks stay intact.
            let mut max_run = 0usize;
            let mut cur = 0usize;
            for b in trimmed.bytes() {
                if b == b'`' {
                    cur += 1;
                    max_run = max_run.max(cur);
                } else {
                    cur = 0;
                }
            }
            let fence = "`".repeat(max_run.max(2) + 1);
            out.push_str(&format!("\n### Output\n\n{fence}\n{trimmed}\n{fence}\n"));
        }
    }
    out
}

/// Append one task section to `dir/<YYYYMMDD>-auto.md`, creating the daily header once.
pub fn append_entry(
    dir: &Path,
    t: &Task,
    files: &[String],
    commits: &[String],
    transcript: Option<&str>,
    offset_secs: i64,
) -> Option<PathBuf> {
    fs::create_dir_all(dir).ok()?;
    let ended = t.started + t.took;
    let path = dir.join(daily_name(ended, offset_secs));
    let needs_header = fs::metadata(&path).map_or(true, |m| m.len() == 0);
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    if needs_header {
        f.write_all(daily_header(&t.project, ended, offset_secs).as_bytes())
            .ok()?;
    }
    f.write_all(render_section(t, files, commits, transcript, offset_secs).as_bytes())
        .ok()?;
    Some(path)
}

/// What git saw since `started`: files changed (still uncommitted) and the
/// commits made. Both empty outside a repository.
pub fn gather(path: &Path, started: SystemTime) -> (Vec<String>, Vec<String>) {
    if !path.ancestors().any(|a| a.join(".git").exists()) {
        return (Vec::new(), Vec::new());
    }
    let files = changed_files(path)
        .unwrap_or_default()
        .into_iter()
        .filter(|f| {
            fs::metadata(path.join(f))
                .and_then(|m| m.modified())
                .is_ok_and(|m| m >= started)
        })
        .take(50)
        .map(|f| f.display().to_string())
        .collect();
    let since = format!("--since=@{}", secs(started));
    let commits = git(path, &["log", &since, "-n", "30", "--pretty=format:%h %s"])
        .map_or_else(Vec::new, |s| s.lines().map(String::from).collect());
    (files, commits)
}

/// Encode a project path the same way Claude Code names folders under `~/.claude/projects/`.
pub(crate) fn encode_claude_project_dir(path: &Path) -> String {
    plain_path(path.to_path_buf())
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn claude_projects_dir() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(|h| PathBuf::from(h).join(".claude").join("projects"))
}

fn find_claude_transcript(project: &Path, started: SystemTime) -> Option<PathBuf> {
    let base = claude_projects_dir()?;
    let target = encode_claude_project_dir(project);
    let proj_dir = fs::read_dir(&base).ok()?.find_map(|e| {
        let e = e.ok()?;
        let name = e.file_name().into_string().ok()?;
        let matches = if cfg!(windows) {
            name.eq_ignore_ascii_case(&target)
        } else {
            name == target
        };
        (matches && e.path().is_dir()).then(|| e.path())
    })?;
    let min_mtime = started
        .checked_sub(Duration::from_secs(5))
        .unwrap_or(UNIX_EPOCH);
    fs::read_dir(proj_dir)
        .ok()?
        .filter_map(|e| {
            let e = e.ok()?;
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") || !path.is_file() {
                return None;
            }
            let mtime = e.metadata().ok()?.modified().ok()?;
            (mtime >= min_mtime).then_some((mtime, path))
        })
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, path)| path)
}

fn read_cursor(ledger_dir: &Path) -> Option<(PathBuf, u64)> {
    let text = fs::read_to_string(ledger_dir.join(".history-cursor")).ok()?;
    let mut lines = text.lines();
    let path = PathBuf::from(lines.next()?.trim());
    let offset = lines.next()?.trim().parse::<u64>().ok()?;
    Some((path, offset))
}

fn write_cursor(ledger_dir: &Path, jsonl: &Path, offset: u64) {
    let _ = fs::write(
        ledger_dir.join(".history-cursor"),
        format!("{}\n{offset}\n", jsonl.display()),
    );
}

fn parse_rfc3339_secs(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: i64 = s[5..7].parse().ok()?;
    let d: i64 = s[8..10].parse().ok()?;
    let hh: i64 = s[11..13].parse().ok()?;
    let mm: i64 = s[14..16].parse().ok()?;
    let ss: i64 = s[17..19].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    let (y, mp) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let yoe = y.rem_euclid(400);
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + (153 * mp + 2) / 5 + d - 1;
    let days = y.div_euclid(400) * 146_097 + doe - 719_468;
    let total = days * 86_400 + hh * 3600 + mm * 60 + ss;
    u64::try_from(total).ok()
}

fn strip_system_reminders(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<system-reminder>") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "<system-reminder>".len()..];
        if let Some(end) = after.find("</system-reminder>") {
            rest = &after[end + "</system-reminder>".len()..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn is_user_boilerplate(text: &str) -> bool {
    text.starts_with("<local-command-caveat>")
        || text.starts_with("<local-command-stdout>")
        || text.starts_with("<command-name>")
}

fn summarize_tool_use(item: &JsonValue) -> Option<String> {
    let name = match item.get("name") {
        Some(JsonValue::String(s)) if !s.trim().is_empty() => s.trim(),
        _ => return None,
    };
    const KEYS: [&str; 7] = [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
    ];
    let arg = item.get("input").and_then(|inp| {
        KEYS.iter().find_map(|k| match inp.get(k) {
            Some(JsonValue::String(s)) if !s.trim().is_empty() => {
                let single: String = s
                    .replace(['\r', '\n', '`'], " ")
                    .trim()
                    .chars()
                    .take(160)
                    .collect();
                Some(single)
            }
            _ => None,
        })
    });
    Some(match arg {
        Some(a) if !a.is_empty() => format!("- [Tool: {name}] `{a}`"),
        _ => format!("- [Tool: {name}]"),
    })
}

fn extract_tool_error(item: &JsonValue) -> Option<String> {
    if item.get("is_error") != Some(&JsonValue::Bool(true)) {
        return None;
    }
    let raw = match item.get("content") {
        Some(JsonValue::String(s)) => s.clone(),
        Some(JsonValue::Array(arr)) => arr
            .iter()
            .filter_map(|v| match (v.get("type"), v.get("text")) {
                (Some(JsonValue::String(k)), Some(JsonValue::String(t))) if k == "text" => {
                    Some(t.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    let preview: String = raw
        .replace(['\r', '\n'], " ")
        .trim()
        .chars()
        .take(200)
        .collect();
    if preview.is_empty() {
        Some("- [Tool error]".to_string())
    } else {
        Some(format!("- [Tool error] {preview}"))
    }
}

fn push_turn(turns: &mut Vec<(&'static str, String)>, role: &'static str, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some((last_role, last_text)) = turns.last_mut() {
        if *last_role == role {
            last_text.push_str("\n\n");
            last_text.push_str(&text);
            return;
        }
    }
    turns.push((role, text));
}

fn extract_turn_parts(content: &JsonValue, is_user: bool, turns: &mut Vec<(&'static str, String)>) {
    match content {
        JsonValue::String(s) => {
            let cleaned = if is_user {
                strip_system_reminders(s)
            } else {
                s.trim().to_string()
            };
            if !is_user || !is_user_boilerplate(&cleaned) {
                push_turn(turns, if is_user { "User" } else { "Assistant" }, cleaned);
            }
        }
        JsonValue::Array(items) => {
            let mut user_parts = Vec::new();
            let mut agent_parts = Vec::new();
            for item in items {
                let kind = match item.get("type") {
                    Some(JsonValue::String(k)) => k.as_str(),
                    _ => continue,
                };
                match (is_user, kind) {
                    (true, "text") => {
                        if let Some(JsonValue::String(t)) = item.get("text") {
                            let cleaned = strip_system_reminders(t);
                            if !cleaned.is_empty() && !is_user_boilerplate(&cleaned) {
                                user_parts.push(cleaned);
                            }
                        }
                    }
                    (true, "tool_result") => {
                        if let Some(err) = extract_tool_error(item) {
                            agent_parts.push(err);
                        }
                    }
                    (false, "text") => {
                        if let Some(JsonValue::String(t)) = item.get("text") {
                            let cleaned = t.trim().to_string();
                            if !cleaned.is_empty() {
                                agent_parts.push(cleaned);
                            }
                        }
                    }
                    (false, "tool_use") => {
                        if let Some(summary) = summarize_tool_use(item) {
                            agent_parts.push(summary);
                        }
                    }
                    _ => {}
                }
            }
            if !user_parts.is_empty() {
                push_turn(turns, "User", user_parts.join("\n\n"));
            }
            if !agent_parts.is_empty() {
                push_turn(turns, "Assistant", agent_parts.join("\n\n"));
            }
        }
        _ => {}
    }
}

/// Read new `User` / `Assistant` turns (questions, tool actions, and answers)
/// from a Claude Code JSONL transcript starting at `start_offset` (or the tail
/// filtered by `min_ts` when no offset is stored yet).
/// Returns `(markdown, new_byte_offset)`.
pub(crate) fn extract_claude_turns(
    jsonl_path: &Path,
    start_offset: Option<u64>,
    min_ts: Option<SystemTime>,
) -> Option<(String, u64)> {
    const INITIAL_TAIL_BYTES: u64 = 512 * 1024;
    let mut file = fs::File::open(jsonl_path).ok()?;
    let file_len = file.metadata().ok()?.len();
    let (seek_pos, mut filter_by_ts) = match start_offset {
        Some(off) if off <= file_len => (off, false),
        _ => (
            file_len.saturating_sub(INITIAL_TAIL_BYTES),
            min_ts.is_some(),
        ),
    };
    file.seek(SeekFrom::Start(seek_pos)).ok()?;
    let mut reader = BufReader::new(file);
    let mut pos = seek_pos;
    let mut line = String::new();
    if start_offset.is_none_or(|o| o > file_len) && seek_pos > 0 {
        let skipped = reader.read_line(&mut line).ok()?;
        pos += skipped as u64;
        line.clear();
    }
    let min_secs = min_ts.map(|t| secs(t).saturating_sub(60)).unwrap_or(0);
    let mut turns: Vec<(&'static str, String)> = Vec::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).ok()?;
        if n == 0 {
            break;
        }
        pos += n as u64;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = json::parse(trimmed) else {
            continue;
        };
        if v.get("isSidechain") == Some(&JsonValue::Bool(true))
            || v.get("isMeta") == Some(&JsonValue::Bool(true))
        {
            continue;
        }
        if filter_by_ts {
            if let Some(JsonValue::String(ts)) = v.get("timestamp") {
                if let Some(s) = parse_rfc3339_secs(ts) {
                    if s < min_secs {
                        continue;
                    }
                    filter_by_ts = false;
                }
            }
        }
        let kind = match v.get("type") {
            Some(JsonValue::String(k)) => k.as_str(),
            _ => continue,
        };
        let is_user = match kind {
            "user" => true,
            "assistant" => false,
            _ => continue,
        };
        let Some(content) = v.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        extract_turn_parts(content, is_user, &mut turns);
    }
    let mut md = String::new();
    for (idx, (role, text)) in turns.into_iter().enumerate() {
        if idx > 0 {
            md.push_str("\n\n");
        }
        md.push_str(&format!("### {role}\n\n{text}"));
    }
    Some((md, pos))
}

fn gather_claude_transcript(project: &Path, started: SystemTime) -> Option<String> {
    let jsonl = find_claude_transcript(project, started)?;
    let ledger_dir = project.join(crate::ledger::DIR);
    let stored = read_cursor(&ledger_dir).and_then(|(p, off)| {
        let same = if cfg!(windows) {
            p.to_string_lossy()
                .eq_ignore_ascii_case(&jsonl.to_string_lossy())
        } else {
            p == jsonl
        };
        same.then_some(off)
    });
    let (md, new_offset) =
        extract_claude_turns(&jsonl, stored, stored.is_none().then_some(started))?;
    write_cursor(&ledger_dir, &jsonl, new_offset);
    (!md.trim().is_empty()).then_some(md)
}

/// Is `name` an entry squad filed itself (daily `<YYYYMMDD>-auto.md` or
/// legacy `<YYYYMMDD>T<HHMMSS>Z-auto[-N].md`)?
pub fn is_auto(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    if stem.len() == 13 && stem.ends_with("-auto") && stem[..8].bytes().all(|b| b.is_ascii_digit())
    {
        return true;
    }
    let b = stem.as_bytes();
    let valid_stamp = b.len() >= 16
        && b[8] == b'T'
        && b[15] == b'Z'
        && stem[..8].bytes().all(|c| c.is_ascii_digit())
        && stem[9..15].bytes().all(|c| c.is_ascii_digit());
    if !valid_stamp {
        return false;
    }
    let rest = &stem[16..];
    rest == "-auto"
        || rest
            .strip_prefix("-auto-")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Entries still being written by a worker.
static PENDING: AtomicUsize = AtomicUsize::new(0);

/// File an entry for `t`. The git lookups and transcript reads run on a worker
/// so the UI never waits for them. Best effort: a failure costs the entry,
/// never the session. `flush` before exit, or a worker still running then loses its entry.
pub fn record(t: Task) {
    PENDING.fetch_add(1, Ordering::SeqCst);
    thread::spawn(move || {
        file(&t);
        PENDING.fetch_sub(1, Ordering::SeqCst);
    });
}

fn file(t: &Task) {
    // The ledger is made when the project is registered; this covers one
    // deleted since. A folder that is gone gets no entry.
    if !t.path.is_dir() || crate::ledger::ensure(&t.path).is_err() {
        return;
    }
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (files, commits) = gather(&t.path, t.started);
    let transcript = gather_claude_transcript(&t.path, t.started);
    let has_body = transcript.as_deref().is_some_and(|s| !s.trim().is_empty())
        || !t.prompts.is_empty()
        || !t.output.trim().is_empty();
    if files.is_empty() && commits.is_empty() && !has_body {
        return;
    }
    let dir = t.path.join(crate::ledger::DIR).join("log");
    append_entry(
        &dir,
        t,
        &files,
        &commits,
        transcript.as_deref(),
        crate::sys::utc_offset_secs(),
    );
}

/// Wait (up to `limit`) for entries still being written. Call before exit.
pub fn flush(limit: Duration) {
    let end = Instant::now() + limit;
    while PENDING.load(Ordering::SeqCst) > 0 && Instant::now() < end {
        thread::sleep(Duration::from_millis(20));
    }
}
