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

Scan the file names here and open the lessons that touch your task.

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
6. **Durability** (should) - still true months from now. A version-specific
   note names the version.
7. **Conciseness** (should) - one lesson per file, read in under a minute.
8. **No duplicates, with source** (should) - check the existing lessons
   first. A correction is a new lesson that names the file it supersedes.
   Record the source project and date.

## Format

- name: `<UTC date and time>-<short-slug>.md`, e.g. `20261127T061200Z-pty-resize-race.md`
- content:
  - `# <one-line lesson>`
  - **Applies when**: the situation
  - **Does not apply when**: where it would mislead
  - **Lesson**: the procedure, checklist, command or decision rule
  - **Verified**: date and environment (versions)
  - **Source**: which project and agent learned it
  - **Supersedes**: the file it corrects, if any
- never edit or delete someone else's lesson.
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
        " Shared lessons from every agent live in {}: read README.md there (the promotion rules), scan the file names, and open those that touch your task. When something in your .squad/ (CONTEXT.md, SITREPs) passes every must rule, promote it there as README.md describes.",
        dir.display()
    )
}
