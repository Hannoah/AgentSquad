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
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
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
    format!(
        "# {} - {}\n\n- Ended: {}\n- Took: {}\n- Folder: {}\n- Command: {}\n\n## Files changed during the task\n\n{}\n## Commits during the task\n\n{}\n## Last output\n\n```\n{}\n```\n",
        t.project,
        t.outcome,
        utc_stamp(secs(t.started + t.took)),
        uptime_label(t.took.as_secs()),
        t.path.display(),
        t.command,
        list(files),
        list(commits),
        t.tail.trim_end(),
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

/// File an entry for `t`. The git lookups run on a worker so the UI never
/// waits for them. Best effort: a failure costs the entry, never the session.
// ponytail: a worker still running when squad quits loses its entry; join the
// workers in shutdown if that ever shows up.
pub fn record(t: Task) {
    thread::spawn(move || {
        // The ledger is made when the project is registered; this covers one
        // deleted since. A folder that is gone gets no entry.
        if !t.path.is_dir() || crate::ledger::ensure(&t.path).is_err() {
            return;
        }
        let (files, commits) = gather(&t.path, t.started);
        let dir = t.path.join(crate::ledger::DIR).join("log");
        write_entry(&dir, &t, &files, &commits);
    });
}
