//! Shared lessons: one `memory/` folder next to `config.toml` that every
//! agent squad manages reads, and promotes truly reusable knowledge into
//! from its project's `.squad/`.
//!
//! Plain markdown, one lesson per file (so several writers never conflict).
//! `README.md` holds the promotion rules; squad writes it once and never
//! overwrites it, so a person can tighten the rules in place.

use crate::config::config_path;
use anyhow::Result;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

pub const README: &str = "\
# Shared lessons

The central knowledge every agent squad manages reads, across all projects.
Each project keeps its own knowledge in `.squad/` (`CONTEXT.md` and the
SITREPs in `log/`); only what passes the rules below is promoted here.

## Before you start

Run `squad lessons` for the index (one line per lesson) and open the
lessons that touch your task. `squad lessons <word>` searches them; use it
whenever a problem looks like one someone may have hit before.

## Promotion rules

Rules 1-5 are MUST: fail one and the lesson stays in the project's
`.squad/`. Rules 6-8 are SHOULD: they raise the quality.

Always, before any rule: no secrets, no customer data, no internal project
names, paths or people. A lesson must be safe to show to anyone.

1. **Generality** (must) - not tied to one project or customer. It helps
   someone else in a different situation.
2. **Non-obviousness** (must) - the official docs or a quick search do not
   give it. Pitfalls found by trial and error, hidden constraints, and how
   things actually behave belong here.
3. **Verified** (must) - actually run or checked. Write down the date it
   was checked and the environment (versions and the like).
4. **Actionability** (must) - a reader can act on it right away. It has at
   least one of: a procedure, a checklist, a command, a decision rule.
5. **Self-containedness** (must) - understood without the original
   project's context. It states when it applies and when it must not.
6. **Atomicity** (should) - one topic per lesson, so it is easy to search,
   link and update.
7. **Freshness** (should) - every lesson has a review date. Duplicates are
   merged into one; a lesson confirmed wrong is retired (see Upkeep).
8. **Provenance** (should) - record which project and agent learned it,
   and check the existing lessons before adding one.

## Format

- name: `<UTC date and time>-<short-slug>.md`, e.g. `20261127T061200Z-pty-resize-race.md`
- content:
  - `# <one-line lesson>`
  - **Applies when**: the situation
  - **Does not apply when**: where it would mislead
  - **Lesson**: the procedure, checklist, command or decision rule
  - **Verified**: date and environment (versions)
  - **Source**: which project and agent learned it
  - **Review by**: a date to check it again (e.g. six months out)
- write in the language the user uses with you.

## Upkeep

- **Past its review date**: check it again. Still true: update
  **Verified** and **Review by**. Not checkable: leave it for a person.
- **Duplicates**: merge them into one lesson (keep the best wording and
  every source), then retire the others.
- **Confirmed wrong**: retire it. Only with evidence, not a hunch.
- **Retire** = move the file into `retired/` here and add one line at the
  top saying why and when. Never delete outright: a person can restore it.
";

/// The shared lessons folder: `AGENTSQUAD_MEMORY`, else `memory/` next to
/// the config file.
pub fn dir() -> Result<PathBuf> {
    if let Some(p) = env::var_os("AGENTSQUAD_MEMORY").filter(|s| !s.is_empty()) {
        return Ok(p.into());
    }
    Ok(config_path()?.with_file_name("memory"))
}

/// Create `dir` and its `README.md` when missing; an existing README is the
/// person's to edit and is never overwritten.
pub fn ensure_at(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let readme = dir.join("README.md");
    if !readme.exists() {
        fs::write(readme, README)?;
    }
    Ok(())
}

/// [`ensure_at`] the shared folder and return its path.
pub fn ensure() -> Result<PathBuf> {
    let d = dir()?;
    ensure_at(&d)?;
    Ok(d)
}

/// The table of contents an agent gets instead of the lessons themselves:
/// one line per lesson (file name and its `# ` title) matching `query` in
/// the file name, title or body (case-insensitive; empty matches all).
/// Bodies stay on disk, so the context cost grows by a line per lesson.
pub fn index(dir: &Path, query: &str) -> String {
    let q = query.trim().to_lowercase();
    let lessons: Vec<(String, String, String)> = crate::ledger::list_entries(dir)
        .into_iter()
        .filter(|(name, _)| name.ends_with(".md") && name != "README.md")
        .map(|(name, path)| {
            let body = fs::read_to_string(&path).unwrap_or_default();
            let title = body
                .lines()
                // PowerShell 5.1 and Notepad start UTF-8 files with a BOM.
                .find_map(|l| l.trim_start_matches('\u{feff}').strip_prefix("# "))
                .unwrap_or("")
                .trim()
                .to_string();
            (name, title, body)
        })
        .collect();
    let total = lessons.len();
    let hits: Vec<String> = lessons
        .into_iter()
        .filter(|(n, _, b)| {
            q.is_empty() || n.to_lowercase().contains(&q) || b.to_lowercase().contains(&q)
        })
        .map(|(n, t, _)| crate::config::sanitize_chrome_line(&format!("- {n}: {t}")))
        .collect();
    let mut out = if q.is_empty() {
        format!(
            "Shared lessons from every agent: {total} in {}. Open a lesson when its title touches your task; search them any time with `squad lessons <word>`. Promotion rules: README.md there.\n",
            dir.display()
        )
    } else {
        format!(
            "{} of {total} shared lessons in {} match \"{}\":\n",
            hits.len(),
            dir.display(),
            query.trim()
        )
    };
    if hits.is_empty() {
        out.push_str("(none)\n");
    }
    for h in hits {
        out.push_str(&h);
        out.push('\n');
    }
    out
}

/// What the Claude Code SessionStart hook prints: the index as added
/// context. Non-ASCII is written as `\u` escapes, so a Korean title survives
/// PowerShell re-encoding the hook's output with the console code page.
pub fn session_start_json(dir: &Path) -> String {
    let mut ctx = crate::json::JsonValue::empty_object();
    ctx.set(
        "hookEventName",
        crate::json::JsonValue::String("SessionStart".into()),
    );
    ctx.set(
        "additionalContext",
        crate::json::JsonValue::String(index(dir, "")),
    );
    let mut out = crate::json::JsonValue::empty_object();
    out.set("hookSpecificOutput", ctx);
    let mut ascii = String::new();
    for c in out.to_pretty().chars() {
        if c.is_ascii() {
            ascii.push(c);
        } else {
            let mut units = [0u16; 2];
            for u in c.encode_utf16(&mut units) {
                ascii.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    ascii
}

/// What the Stop hook adds when it sends an agent back to file a SITREP:
/// the moment to judge whether this work taught something worth promoting.
/// `none` is the expected answer, so the nudge does not breed filler.
pub fn promote_note(dir: &Path) -> String {
    format!(
        " End it with a line **Promote**: if this work taught something that passes every must rule in {}, file that lesson there as the README describes and name the file; otherwise write none (most work teaches nothing worth promoting).",
        dir.join("README.md").display()
    )
}

/// The sentence the brief appends so an agent reads and feeds the lessons.
pub fn brief_note(dir: &Path) -> String {
    format!(
        " Shared lessons from every agent live in {}: run `squad lessons` for their index (`squad lessons <word>` searches) and open those that touch your task; README.md there has the promotion rules. When something in your .squad/ (CONTEXT.md, SITREPs) passes every must rule, promote it there as README.md describes.",
        dir.display()
    )
}
