//! Shared lessons: one `memory/` folder next to `config.toml` that every
//! agent squad manages reads, and promotes truly reusable knowledge into.
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

Knowledge every agent squad manages reads, across all projects.
Sessions end and projects close; what is here keeps helping.

## Before you start

Scan the file names here and open the lessons that touch your task.

## Promotion rules

Promote a lesson here only when it passes ALL of these. When in doubt,
keep it in your project's `.squad/CONTEXT.md` instead.

1. **Universal** - not tied to one project. Someone on a different
   project, with a different agent, would be helped by it.
2. **Verified** - it actually happened: reproduced, fixed, or measured.
   No guesses, no \"should work\".
3. **Costly to rediscover** - it took real time or a failure to learn.
   Not something the official docs or a quick search answer.
4. **Actionable** - it says what to do or avoid, and when it applies.
5. **Durable** - still true months from now. A version-specific
   workaround names the version.
6. **Clean** - no secrets, no customer data, no private project names,
   paths or people. Write it so it could be shown to anyone.
7. **New** - check the existing lessons first. If one covers it, do not
   add a near-copy; a correction is a new lesson that names the old one.

## Format

- name: `<UTC date and time>-<short-slug>.md`, e.g. `20261127T061200Z-pty-resize-race.md`
- content:
  - `# <one-line lesson>`
  - **Applies when**: the situation (tools, platform, versions)
  - **Lesson**: what to do or avoid
  - **Evidence**: what happened that proved it
  - **Source**: which project and agent learned it, and when
  - **Supersedes**: the file it corrects, if any
- one lesson per file; never edit or delete someone else's lesson.
- write in the language the user uses with you.
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

/// The sentence the brief appends so an agent reads and feeds the lessons.
pub fn brief_note(dir: &Path) -> String {
    format!(
        " Shared lessons from every agent live in {}: read README.md there (the promotion rules), scan the file names, and open those that touch your task. When you learn something that passes every promotion rule, file it there as README.md describes.",
        dir.display()
    )
}
