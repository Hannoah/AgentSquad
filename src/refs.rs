use crate::config::{
    atomic_write_bytes, config_path, load_toml_or_default, save_toml_to, SyncState,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

pub const START_MARKER: &str = "<!-- agentsquad:references:start -->";
pub const END_MARKER: &str = "<!-- agentsquad:references:end -->";

/// Primary instruction markdown targets read across claude, agy, muse, and codex.
const PRIMARY_DOC_FILES: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ReferenceEntry {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ReferenceVault {
    #[serde(default)]
    pub references: Vec<ReferenceEntry>,
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct ProjectReferences {
    pub vault_state: BTreeMap<String, SyncState>,
    pub local_only: Vec<String>,
}

pub fn refs_path() -> Result<PathBuf> {
    if let Some(p) = env::var_os("AGENTSQUAD_REFS").filter(|s| !s.is_empty()) {
        return Ok(p.into());
    }
    Ok(config_path()?.with_file_name("references.toml"))
}

pub fn load_refs_from(p: &Path) -> Result<ReferenceVault> {
    load_toml_or_default(p)
}

pub fn load_refs() -> Result<ReferenceVault> {
    load_refs_from(&refs_path()?)
}

pub fn save_refs_to(vault: &ReferenceVault, p: &Path) -> Result<()> {
    save_toml_to(vault, p, false)
}

pub fn save_refs(vault: &ReferenceVault) -> Result<()> {
    save_refs_to(vault, &refs_path()?)
}

pub fn validate_ref_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("Reference name cannot be empty.".into());
    }
    if name.len() > 64 {
        return Err("Reference name must be 64 characters or fewer.".into());
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err("Reference name must start with A-Z, a-z, or 0-9.".into());
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Err("Reference name may only contain A-Z, a-z, 0-9, -, _, or .".into());
    }
    Ok(name.to_string())
}

pub fn normalize_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("URL cannot be empty.".into());
    }
    if trimmed.chars().any(char::is_whitespace) || trimmed.chars().any(char::is_control) {
        return Err("URL cannot contain spaces or control characters.".into());
    }
    let normalized = if trimmed
        .get(..11)
        .is_some_and(|p| p.eq_ignore_ascii_case("github.com/"))
    {
        format!("https://{trimmed}")
    } else {
        trimmed.to_string()
    };
    let rest = normalized
        .strip_prefix("https://")
        .or_else(|| normalized.strip_prefix("http://"))
        .ok_or_else(|| {
            "URL must start with https:// or http:// (or github.com/...).".to_string()
        })?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_name = host.split(':').next().unwrap_or("");
    if host_name.is_empty()
        || (!host_name.contains('.') && !host_name.eq_ignore_ascii_case("localhost"))
    {
        return Err("URL must include a valid host (e.g. https://github.com/owner/repo).".into());
    }
    Ok(normalized)
}

/// Derive a clean fallback slug from a URL (e.g. `https://github.com/org/repo` -> `repo`).
pub fn infer_name_from_url(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let after_scheme = without_query
        .split_once("://")
        .map_or(without_query, |(_, r)| r);
    let path_part = after_scheme.split_once('/').map_or("", |(_, p)| p);
    let last = path_part
        .split('/')
        .map(str::trim)
        .rfind(|s| !s.is_empty())
        .unwrap_or("ref");
    let candidate = last.strip_suffix(".git").unwrap_or(last);
    let mut slug: String = candidate
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    while slug.starts_with(['-', '_', '.']) {
        slug.remove(0);
    }
    if slug.is_empty() {
        "ref".to_string()
    } else {
        slug.chars().take(64).collect()
    }
}

pub fn sanitize_description(raw: &str) -> String {
    raw.chars()
        .map(|c| if matches!(c, '\r' | '\n') { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn upsert_ref(vault: &mut ReferenceVault, mut entry: ReferenceEntry) -> Result<bool, String> {
    entry.url = normalize_url(&entry.url)?;
    let raw_name = if entry.name.trim().is_empty() {
        infer_name_from_url(&entry.url)
    } else {
        entry.name.clone()
    };
    entry.name = validate_ref_name(&raw_name)?;
    entry.description = sanitize_description(&entry.description);
    if let Some(existing) = vault
        .references
        .iter_mut()
        .find(|r| r.name.eq_ignore_ascii_case(&entry.name))
    {
        *existing = entry;
        Ok(false)
    } else {
        vault.references.push(entry);
        Ok(true)
    }
}

pub fn remove_ref(vault: &mut ReferenceVault, name: &str) -> bool {
    let target = name.trim();
    let before = vault.references.len();
    vault
        .references
        .retain(|r| !r.name.eq_ignore_ascii_case(target));
    vault.references.len() != before
}

fn find_doc_path(dir: &Path, default_name: &str) -> PathBuf {
    let exact = dir.join(default_name);
    if exact.is_file() {
        return exact;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.eq_ignore_ascii_case(default_name) && entry.path().is_file() {
                    return entry.path();
                }
            }
        }
    }
    exact
}

fn target_doc_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = PRIMARY_DOC_FILES
        .iter()
        .map(|name| find_doc_path(dir, name))
        .collect();
    let gemini = find_doc_path(dir, "GEMINI.md");
    if gemini.is_file() {
        paths.push(gemini);
    }
    paths
}

fn render_block(entries: &[&ReferenceEntry], eol: &str) -> String {
    let mut lines = vec![
        START_MARKER.to_string(),
        "## References (managed by AgentSquad)".to_string(),
        String::new(),
    ];
    for e in entries {
        if e.description.is_empty() {
            lines.push(format!("- **{}** ({})", e.name, e.url));
        } else {
            lines.push(format!("- **{}** ({}): {}", e.name, e.url, e.description));
        }
    }
    lines.push(END_MARKER.to_string());
    lines.join(eol)
}

fn find_marker_span(text: &str) -> Option<(usize, usize)> {
    let start_idx = text.find(START_MARKER)?;
    let line_start = text[..start_idx].rfind('\n').map_or(0, |i| i + 1);
    let end_rel = text[start_idx..].find(END_MARKER)?;
    let end_marker_end = start_idx + end_rel + END_MARKER.len();
    let line_end = text[end_marker_end..]
        .find('\n')
        .map_or(text.len(), |i| end_marker_end + i + 1);
    Some((line_start, line_end))
}

/// Insert or replace the managed references block inside `existing`, preserving all other text.
pub fn upsert_managed_block(existing: &str, entries: &[&ReferenceEntry]) -> String {
    let eol = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let block = render_block(entries, eol);
    if let Some((start, end)) = find_marker_span(existing) {
        let before = existing[..start].trim_end_matches(['\r', '\n']);
        let after = existing[end..].trim_start_matches(['\r', '\n']);
        match (before.trim().is_empty(), after.trim().is_empty()) {
            (true, true) => format!("{block}{eol}"),
            (false, true) => format!("{before}{eol}{eol}{block}{eol}"),
            (true, false) => {
                format!("{block}{eol}{eol}{}", after.trim_end_matches(['\r', '\n'])) + eol
            }
            (false, false) => format!(
                "{before}{eol}{eol}{block}{eol}{eol}{}{eol}",
                after.trim_end_matches(['\r', '\n'])
            ),
        }
    } else if existing.trim().is_empty() {
        format!("{block}{eol}")
    } else {
        let head = existing.trim_end_matches(['\r', '\n']);
        format!("{head}{eol}{eol}{block}{eol}")
    }
}

/// Remove ONLY the managed references block from `existing`. Returns `None` when
/// no non-whitespace user content remains (indicating the file can be removed).
pub fn remove_managed_block(existing: &str) -> Option<String> {
    let Some((start, end)) = find_marker_span(existing) else {
        return (!existing.trim().is_empty()).then(|| existing.to_string());
    };
    let eol = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let before = existing[..start].trim_end_matches(['\r', '\n']);
    let after = existing[end..].trim_start_matches(['\r', '\n']);
    match (before.trim().is_empty(), after.trim().is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!("{before}{eol}")),
        (true, false) => Some(format!("{}{eol}", after.trim_end_matches(['\r', '\n']))),
        (false, false) => Some(format!(
            "{before}{eol}{eol}{}{eol}",
            after.trim_end_matches(['\r', '\n'])
        )),
    }
}

/// Parse entries inside the managed references block of `text` as `name -> (url, description)`.
pub fn parse_managed_refs(text: &str) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    let Some((start, end)) = find_marker_span(text) else {
        return out;
    };
    for line in text[start..end].lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("- **") else {
            continue;
        };
        let Some((name, after_name)) = rest.split_once("** (") else {
            continue;
        };
        let Some((url, after_url)) = after_name.split_once(')') else {
            continue;
        };
        let desc = after_url
            .trim()
            .strip_prefix(':')
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        out.insert(name.trim().to_string(), (url.trim().to_string(), desc));
    }
    out
}

/// Inspect `<dir>` instruction files (`CLAUDE.md`, `AGENTS.md`, and `GEMINI.md` if present)
/// against `vault`.
pub fn read_project_refs(dir: &Path, vault: &ReferenceVault) -> ProjectReferences {
    let mut state = ProjectReferences::default();
    if !dir.is_dir() {
        return state;
    }
    let targets = target_doc_paths(dir);
    let parsed: Vec<BTreeMap<String, (String, String)>> = targets
        .iter()
        .map(|p| {
            fs::read_to_string(p)
                .map(|t| parse_managed_refs(&t))
                .unwrap_or_default()
        })
        .collect();

    let vault_names: BTreeSet<&str> = vault.references.iter().map(|r| r.name.as_str()).collect();
    let mut local_set = BTreeSet::new();
    for map in &parsed {
        for name in map.keys() {
            if !vault_names.contains(name.as_str()) {
                local_set.insert(name.clone());
            }
        }
    }
    state.local_only = local_set.into_iter().collect();

    for entry in &vault.references {
        let hits = parsed
            .iter()
            .filter(|m| m.contains_key(&entry.name))
            .count();
        if hits == 0 {
            continue;
        }
        let all_match = hits == parsed.len()
            && parsed.iter().all(|m| {
                m.get(&entry.name)
                    .is_some_and(|(u, d)| u == entry.url.trim() && d == entry.description.trim())
            });
        state.vault_state.insert(
            entry.name.clone(),
            if all_match {
                SyncState::Installed
            } else {
                SyncState::Differs
            },
        );
    }
    state
}

fn write_refs_to_project(
    dir: &Path,
    vault: &ReferenceVault,
    desired_names: &BTreeSet<String>,
) -> Result<()> {
    let selected: Vec<&ReferenceEntry> = vault
        .references
        .iter()
        .filter(|r| desired_names.contains(&r.name))
        .collect();

    for path in target_doc_paths(dir) {
        let existing = if path.is_file() {
            Some(
                fs::read_to_string(&path)
                    .with_context(|| format!("Cannot read {}", path.display()))?,
            )
        } else {
            None
        };
        if selected.is_empty() {
            let Some(text) = existing else {
                continue;
            };
            if find_marker_span(&text).is_none() {
                continue;
            }
            match remove_managed_block(&text) {
                None => {
                    fs::remove_file(&path)
                        .with_context(|| format!("Cannot remove {}", path.display()))?;
                }
                Some(cleaned) => {
                    atomic_write_bytes(&path, cleaned.as_bytes(), false)?;
                }
            }
        } else {
            let text = existing.unwrap_or_default();
            let updated = upsert_managed_block(&text, &selected);
            if updated != text {
                atomic_write_bytes(&path, updated.as_bytes(), false)?;
            }
        }
    }
    Ok(())
}

/// Apply `names` from `vault` into `<dir>/CLAUDE.md` and `AGENTS.md` (preserving existing content
/// and existing assigned references).
pub fn apply_ref_names(
    dir: &Path,
    vault: &ReferenceVault,
    names: &[String],
) -> Result<(usize, usize)> {
    if names.is_empty() {
        return Ok((0, 0));
    }
    if !dir.is_dir() {
        bail!("Project folder does not exist: {}", dir.display());
    }
    let current = read_project_refs(dir, vault);
    let mut desired: BTreeSet<String> = current.vault_state.keys().cloned().collect();
    let mut added = 0usize;
    let mut updated = 0usize;
    for raw in names {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(entry) = vault
            .references
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(trimmed))
        else {
            bail!("Unknown shared reference: {trimmed}");
        };
        if desired.insert(entry.name.clone()) {
            added += 1;
        } else if current.vault_state.get(&entry.name) != Some(&SyncState::Installed) {
            updated += 1;
        }
    }
    write_refs_to_project(dir, vault, &desired)?;
    Ok((added, updated))
}

/// Remove `names` from the managed references block inside `<dir>/CLAUDE.md`, `AGENTS.md`, etc.
pub fn remove_refs(dir: &Path, vault: &ReferenceVault, names: &[String]) -> Result<usize> {
    if names.is_empty() || !dir.is_dir() {
        return Ok(0);
    }
    let current = read_project_refs(dir, vault);
    let mut desired: BTreeSet<String> = current.vault_state.keys().cloned().collect();
    let mut removed = 0usize;
    for raw in names {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(existing) = desired.iter().find(|n| n.eq_ignore_ascii_case(t)).cloned() {
            desired.remove(&existing);
            removed += 1;
        }
    }
    if removed > 0 {
        write_refs_to_project(dir, vault, &desired)?;
    }
    Ok(removed)
}

/// Sync checked references into `<dir>/CLAUDE.md` and `AGENTS.md` and remove unchecked ones,
/// preserving all unrelated markdown content.
pub fn sync_project_refs(
    dir: &Path,
    vault: &ReferenceVault,
    picked: &BTreeSet<String>,
    current: &ProjectReferences,
) -> Result<(usize, usize, usize)> {
    let mut added = 0usize;
    let mut updated = 0usize;
    for name in picked {
        match current.vault_state.get(name) {
            None => added += 1,
            Some(SyncState::Differs) => updated += 1,
            Some(SyncState::Installed) => {}
        }
    }
    let removed = vault
        .references
        .iter()
        .filter(|r| current.vault_state.contains_key(&r.name) && !picked.contains(&r.name))
        .count();
    if added == 0 && updated == 0 && removed == 0 {
        return Ok((0, 0, 0));
    }
    if !dir.is_dir() {
        bail!("Project folder does not exist: {}", dir.display());
    }
    write_refs_to_project(dir, vault, picked)?;
    Ok((added, updated, removed))
}
