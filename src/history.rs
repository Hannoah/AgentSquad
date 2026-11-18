//! Automatic work history: squad writes one entry per finished task itself, so
//! the record exists whatever the agent does or forgets. Entries are markdown
//! files named like SITREPs, filed in the project's `.squad/log/` where the
//! agent reads its history at the start of its next session.

use crate::ledger::{changed_files, git, utc_stamp};
use crate::ui::uptime_label;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Shortest task worth an entry; anything quicker is typing, not work.
pub const MIN_TASK: Duration = Duration::from_secs(5);

/// What squad saw of one task.
pub struct Task {
    pub project: String,
    pub path: PathBuf,
    pub command: String,
    pub started: SystemTime,
    pub took: Duration,
    /// How it ended: "task complete", or why the session ended mid-task.
    pub outcome: String,
    /// The last lines on the agent's screen.
    pub tail: String,
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The entry as markdown. Pure, so it tests without git or a disk.
pub fn render(t: &Task, files: &[String], commits: &[String]) -> String {
    let list = |items: &[String]| {
        if items.is_empty() {
            "(none seen)\n".to_string()
        } else {
            items.iter().map(|i| format!("- {i}\n")).collect()
        }
    };
    let tail = t.tail.trim_end();
    // Use a fence longer than any backtick run in `tail` so embedded code blocks stay intact.
    let mut max_run = 0usize;
    let mut cur = 0usize;
    for b in tail.bytes() {
        if b == b'`' {
            cur += 1;
            max_run = max_run.max(cur);
        } else {
            cur = 0;
        }
    }
    let fence = "`".repeat(max_run.max(2) + 1);
    format!(
        "# {} - {}\n\n- Ended: {}\n- Took: {}\n- Folder: {}\n- Command: {}\n\n## Files changed during the task\n\n{}\n## Commits during the task\n\n{}\n## Last output\n\n{fence}\n{}\n{fence}\n",
        t.project,
        t.outcome,
        utc_stamp(secs(t.started + t.took)),
        uptime_label(t.took.as_secs()),
        t.path.display(),
        t.command,
        list(files),
        list(commits),
        tail,
    )
}

/// Write the entry into `dir`, named by its end time; a second entry in the
/// same second gets a counter. Never overwrites.
pub fn write_entry(dir: &Path, t: &Task, files: &[String], commits: &[String]) -> Option<PathBuf> {
    fs::create_dir_all(dir).ok()?;
    let stamp = utc_stamp(secs(t.started + t.took)).replace(['-', ':'], "");
    let text = render(t, files, commits);
    for n in 0.. {
        let name = if n == 0 {
            format!("{stamp}-auto.md")
        } else {
            format!("{stamp}-auto-{n}.md")
        };
        let path = dir.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => return f.write_all(text.as_bytes()).ok().map(|_| path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
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

/// Auto entries kept per project; older ones go as new ones land. Without a
/// cap a busy project gathers thousands, and every listing of the log slows.
const KEEP_AUTO: usize = 200;

/// Is `name` an entry squad filed itself (`<stamp>-auto.md`, `<stamp>-auto-<n>.md`)?
pub fn is_auto(name: &str) -> bool {
    let rest = name.strip_suffix(".md").and_then(|stem| stem.get(16..));
    rest.is_some_and(|r| {
        r == "-auto"
            || r.strip_prefix("-auto-")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Delete the oldest auto entries in `dir` beyond the newest `keep`. SITREPs
/// an agent wrote are never touched.
pub fn prune(dir: &Path, keep: usize) {
    let mut autos: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| is_auto(n))
        .collect();
    autos.sort_unstable_by(|a, b| b.cmp(a)); // names start with a UTC stamp
    for name in autos.into_iter().skip(keep) {
        let _ = fs::remove_file(dir.join(name));
    }
}

/// Entries still being written by a worker.
static PENDING: AtomicUsize = AtomicUsize::new(0);

/// File an entry for `t`. The git lookups run on a worker so the UI never
/// waits for them. Best effort: a failure costs the entry, never the session.
/// `flush` before exit, or a worker still running then loses its entry.
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
    let (files, commits) = gather(&t.path, t.started);
    let dir = t.path.join(crate::ledger::DIR).join("log");
    write_entry(&dir, t, &files, &commits);
    prune(&dir, KEEP_AUTO);
}

/// Wait (up to `limit`) for entries still being written. Call before exit.
pub fn flush(limit: Duration) {
    let end = Instant::now() + limit;
    while PENDING.load(Ordering::SeqCst) > 0 && Instant::now() < end {
        thread::sleep(Duration::from_millis(20));
    }
}
