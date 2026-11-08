//! The squad ledger: a `.squad/` folder inside a project where agents keep
//! their history and key facts, and squad records what it observed.
//!
//! Plain markdown files, readable by people and agents alike:
//! `README.md` (the protocol), `CONTEXT.md` (key facts), `log/` (one file
//! per entry, so several writers never conflict) and `events.log` (written
//! by squad only).

use anyhow::Result;
use std::{
    fs,
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub const DIR: &str = ".squad";

const README: &str = "\
# Squad ledger

This folder is the shared memory of the agents working on this project.
You (the agent) read it before working and write to it as you go. Sessions
end and your memory with them; this folder does not.

## Before you start

1. Read `CONTEXT.md` - the goal, decisions, conventions and known pitfalls.
2. Read the newest files in `log/` - what was done lately and what is next.

## While you work

After each meaningful unit of work (a feature, a fix, a decision, a dead
end worth remembering) add ONE new file to `log/`:

- name: `<UTC date and time>-<short-slug>.md`, e.g. `20261108T061200Z-add-auth.md`
- content: **What** you did, **Why**, **Result** (including what failed), **Next**.
- never edit or delete old entries; correct a mistake with a new entry.

Update `CONTEXT.md` only when a durable fact changes (a decision, a rule, a
pitfall). Keep it short: it is read at the start of every session.

## Rules

- `events.log` is written by squad. Do not edit it.
- Write in the language the user uses with you.
- Never put secrets (keys, tokens, passwords) in this folder.
";

const CONTEXT: &str = "\
# Project context

Keep this short. It is read at the start of every session.

## Goal

(what this project is for)

## Architecture and key decisions

(decisions that are costly to rediscover, with the reason)

## Conventions

(naming, structure, how to build and test)

## Known pitfalls

(things that wasted time before)

## Current state

(where things stand; the log has the details)
";

/// Seconds since the epoch as `2026-11-08T06:12:03Z`.
pub fn utc_stamp(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Days-to-calendar conversion (proleptic Gregorian, March-based years).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Create the ledger in `project` unless one exists. Returns whether it was
/// created. An existing folder is never touched, so a ledger people have
/// written to is safe.
pub fn ensure(project: &Path) -> Result<bool> {
    let dir = project.join(DIR);
    if dir.exists() {
        return Ok(false);
    }
    fs::create_dir_all(dir.join("log"))?;
    fs::write(dir.join("README.md"), README)?;
    fs::write(dir.join("CONTEXT.md"), CONTEXT)?;
    // Agent notes can hold sensitive detail: keep them out of commits by
    // default. Deleting the line from .git/info/exclude opts in.
    let _ = exclude_from_git(project);
    Ok(true)
}

/// Add `.squad/` to the repository's local exclude list (not `.gitignore`,
/// which is shared). A worktree or submodule, where `.git` is a file, is
/// left alone.
fn exclude_from_git(project: &Path) -> std::io::Result<()> {
    let git = project.join(".git");
    if !git.is_dir() {
        return Ok(());
    }
    let info = git.join("info");
    fs::create_dir_all(&info)?;
    let file = info.join("exclude");
    let mut text = fs::read_to_string(&file).unwrap_or_default();
    if text
        .lines()
        .any(|l| matches!(l.trim(), ".squad" | ".squad/"))
    {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(".squad/\n");
    fs::write(file, text)
}

/// Append one fact to `events.log`. Best effort and silent: recording must
/// never get in the way of running a session. Does nothing without a ledger.
pub fn event(project: &Path, text: &str) {
    let dir = project.join(DIR);
    if !dir.is_dir() {
        return;
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let line = format!("{} {text}\n", utc_stamp(secs));
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("events.log"))
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// What `b` types into an agent to point it at the ledger. No digits or
/// other keystrokes that could answer a menu by accident.
pub fn brief_line() -> &'static str {
    "Before you start, read .squad/README.md, then .squad/CONTEXT.md and the newest files in .squad/log/. Follow the protocol in README.md for the rest of this session."
}
