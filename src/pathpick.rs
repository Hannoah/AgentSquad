//! Path helpers for the add form: Tab completion, the live subfolder list and a
//! live check. Pure functions over the typed text, so they test without a UI.

use crate::config::{clean_path, Project};
use std::{fs, path::MAIN_SEPARATOR};

/// A pickable path: (short label shown in the form, full text it fills in).
pub type Pick = (String, String);
/// Most subfolders the form lists (it shows a few at a time and scrolls).
const MAX_PICKS: usize = 50;
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

/// The folders matching the last segment of the typed text.
struct Scan {
    quote: &'static str,
    /// Typed text up to and including the last separator.
    dir: String,
    sep: char,
    /// Matching folder names, sorted case-insensitively. Never empty.
    names: Vec<String>,
}

/// Look the last segment of `text` up on disk. None when nothing matches.
/// Matching ignores case (Windows paths do); dot folders only show once the
/// prefix starts with a dot.
fn scan(text: &str) -> Option<Scan> {
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
    if names.is_empty() {
        return None;
    }
    names.sort_by_key(|n| n.to_lowercase());
    Some(Scan {
        quote,
        dir: dir.to_string(),
        sep,
        names,
    })
}

/// The folders to list under the Path field for the typed text; a trailing
/// separator lists that folder's subfolders. Empty when nothing matches.
pub fn suggest(text: &str) -> Vec<Pick> {
    scan(text).map_or_else(
        Vec::new,
        |Scan {
             quote,
             dir,
             sep,
             names,
         }| {
            names
                .iter()
                .take(MAX_PICKS)
                .map(|n| (format!("{n}{sep}"), format!("{quote}{dir}{n}{sep}")))
                .collect()
        },
    )
}

/// Tab: complete the last segment to the single match or the longest shared
/// prefix. None when nothing matches.
pub fn complete(text: &str) -> Option<String> {
    let Scan {
        quote,
        dir,
        sep,
        names,
    } = scan(text)?;
    if names.len() == 1 {
        return Some(format!("{quote}{dir}{}{sep}", names[0]));
    }
    let first: Vec<char> = names[0].chars().collect();
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
    Some(format!("{quote}{dir}{shared}"))
}
