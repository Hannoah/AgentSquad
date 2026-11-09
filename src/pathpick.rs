//! Path helpers for the add form: Tab completion, folder suggestions and a
//! live check. Pure functions over the typed text, so they test without a UI.

use crate::config::{clean_path, Project};
use std::{fs, path::MAIN_SEPARATOR};

/// A pickable path: (short label shown in the form, full text it fills in).
pub type Pick = (String, String);
/// Most candidates the form lists; `Alt+1`..`Alt+9` pick them.
const MAX_PICKS: usize = 9;
/// Most suggested parent folders shown for an empty Path field.
const MAX_ROOTS: usize = 6;

/// What the Path field currently points at.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PathState {
    #[default]
    Blank,
    Folder,
    Missing,
    NotFolder,
}

/// Look at the disk once. Call on edits, not per frame: a dead network share
/// can block for seconds.
pub fn check(raw: &str) -> PathState {
    let p = clean_path(raw);
    if p.is_empty() {
        return PathState::Blank;
    }
    match fs::metadata(p) {
        Ok(m) if m.is_dir() => PathState::Folder,
        Ok(_) => PathState::NotFolder,
        Err(_) => PathState::Missing,
    }
}

/// Parent folders of the registered projects, most used first: projects tend
/// to live side by side, so this is usually where the next one goes.
pub fn roots(projects: &[Project]) -> Vec<Pick> {
    let mut found: Vec<(String, usize)> = Vec::new();
    for dir in projects.iter().filter_map(|p| p.path.parent()) {
        let mut text = dir.display().to_string();
        if !text.ends_with(['\\', '/']) {
            text.push(MAIN_SEPARATOR); // a drive root already has one
        }
        match found
            .iter_mut()
            .find(|(t, _)| t.eq_ignore_ascii_case(&text))
        {
            Some((_, n)) => *n += 1,
            None => found.push((text, 1)),
        }
    }
    found.sort_by_key(|&(_, n)| std::cmp::Reverse(n)); // stable: ties keep registration order
    found
        .into_iter()
        .take(MAX_ROOTS)
        .map(|(t, _)| (t.clone(), t))
        .collect()
}

/// Result of one Tab press.
pub struct Completion {
    /// The new field text: a single match, or the longest shared prefix.
    pub text: String,
    /// Every match when there are several (each full text ends with a separator).
    pub picks: Vec<Pick>,
}

/// Complete the last segment of `text` against the folders on disk. None
/// when nothing matches. Matching ignores case (Windows paths do); dot
/// folders only show once the prefix starts with a dot.
pub fn complete(text: &str) -> Option<Completion> {
    let (quote, rest) = match text.strip_prefix('"') {
        Some(r) => ("\"", r),
        None => ("", text),
    };
    let sep = if rest.contains('/') && !rest.contains('\\') {
        '/'
    } else {
        MAIN_SEPARATOR
    };
    // "C:" has no separator yet; treat it as the drive root.
    let rest = match rest.as_bytes() {
        [d, b':'] if d.is_ascii_alphabetic() => format!("{rest}{sep}"),
        _ => rest.to_string(),
    };
    let (dir, prefix) = rest.split_at(rest.rfind(['\\', '/']).map_or(0, |i| i + 1));
    let want = prefix.to_lowercase();
    let mut names: Vec<String> = fs::read_dir(if dir.is_empty() { "." } else { dir })
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            n.to_lowercase().starts_with(&want) && (prefix.starts_with('.') || !n.starts_with('.'))
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    let first: Vec<char> = names.first()?.chars().collect();
    if names.len() == 1 {
        return Some(Completion {
            text: format!("{quote}{dir}{}{sep}", names[0]),
            picks: Vec::new(),
        });
    }
    let shared: String = (0..first.len())
        .take_while(|&i| {
            names.iter().all(|n| {
                n.chars()
                    .nth(i)
                    .is_some_and(|c| c.to_lowercase().eq(first[i].to_lowercase()))
            })
        })
        .map(|i| first[i])
        .collect();
    Some(Completion {
        text: format!("{quote}{dir}{shared}"),
        picks: names
            .iter()
            .take(MAX_PICKS)
            .map(|n| (format!("{n}{sep}"), format!("{quote}{dir}{n}{sep}")))
            .collect(),
    })
}
