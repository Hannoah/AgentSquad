use crate::config::{atomic_write_bytes, config_path, load_toml_or_default, save_toml_to};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    path::{Path, PathBuf},
};

/// A single reusable environment variable stored in the shared vault.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
}

/// Shared environment variable registry persisted next to `config.toml`.
#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct EnvVault {
    #[serde(default)]
    pub vars: Vec<EnvEntry>,
}

/// Location of the shared env vault file (`env.toml`).
pub fn env_path() -> Result<PathBuf> {
    if let Some(p) = env::var_os("AGENTSQUAD_ENV").filter(|s| !s.is_empty()) {
        return Ok(p.into());
    }
    Ok(config_path()?.with_file_name("env.toml"))
}

/// Load the shared env vault from `p` (empty vault when the file does not exist).
pub fn load_env_from(p: &Path) -> Result<EnvVault> {
    load_toml_or_default(p)
}

pub fn load_env() -> Result<EnvVault> {
    load_env_from(&env_path()?)
}

/// Persist the shared env vault atomically via temp file + rename.
pub fn save_env_to(vault: &EnvVault, p: &Path) -> Result<()> {
    save_toml_to(vault, p, true)
}

pub fn save_env(vault: &EnvVault) -> Result<()> {
    save_env_to(vault, &env_path()?)
}

/// Validate and normalize an environment variable key (`[A-Za-z_][A-Za-z0-9_]*`).
pub fn validate_env_key(raw: &str) -> Result<String, String> {
    let key = raw.trim();
    if key.is_empty() {
        return Err("Key cannot be empty.".into());
    }
    let mut chars = key.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err("Key must start with A-Z, a-z, or _.".into());
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("Key may only contain A-Z, a-z, 0-9, or _.".into());
    }
    Ok(key.to_string())
}

/// Insert or update `key` in `vault`. Returns `Ok(true)` if newly added, `Ok(false)` if updated.
pub fn upsert_env(vault: &mut EnvVault, key: &str, value: &str) -> Result<bool, String> {
    let normalized = validate_env_key(key)?;
    if let Some(existing) = vault.vars.iter_mut().find(|v| v.key == normalized) {
        existing.value = value.to_string();
        Ok(false)
    } else {
        vault.vars.push(EnvEntry {
            key: normalized,
            value: value.to_string(),
        });
        Ok(true)
    }
}

/// Remove `key` from `vault`. Returns `true` if an entry was removed.
pub fn remove_env(vault: &mut EnvVault, key: &str) -> bool {
    let before = vault.vars.len();
    vault.vars.retain(|v| v.key != key);
    vault.vars.len() != before
}

/// Mask a secret value for safe display unless reveal mode is active.
pub fn mask_value(val: &str) -> String {
    let single: String = val
        .chars()
        .map(|c| if matches!(c, '\r' | '\n') { ' ' } else { c })
        .collect();
    let chars: Vec<char> = single.chars().collect();
    if chars.len() <= 4 {
        return "••••".to_string();
    }
    let head: String = chars[..2].iter().collect();
    let tail: String = chars[chars.len() - 2..].iter().collect();
    format!("{head}••••{tail}")
}

/// Parse a single `.env` assignment line into `(key, value)`.
pub fn parse_dotenv_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let body = trimmed
        .strip_prefix("export")
        .filter(|rest| rest.chars().next().is_some_and(char::is_whitespace))
        .map(str::trim_start)
        .unwrap_or(trimmed);
    let (raw_key, raw_val) = body.split_once('=')?;
    let key = validate_env_key(raw_key).ok()?;
    let val_trim = raw_val.trim();
    let value = if val_trim.len() >= 2 && val_trim.starts_with('"') && val_trim.ends_with('"') {
        unescape_double_quoted(&val_trim[1..val_trim.len() - 1])
    } else if val_trim.len() >= 2 && val_trim.starts_with('\'') && val_trim.ends_with('\'') {
        val_trim[1..val_trim.len() - 1].to_string()
    } else {
        // Strip inline `# comment` only when unquoted and preceded by whitespace.
        let mut cut = val_trim.len();
        let mut prev_ws = false;
        for (idx, ch) in val_trim.char_indices() {
            if ch == '#' && prev_ws {
                cut = idx;
                break;
            }
            prev_ws = ch.is_whitespace();
        }
        val_trim[..cut].trim_end().to_string()
    };
    Some((key, value))
}

fn unescape_double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Read all parsed `KEY=VALUE` entries from `<dir>/.env` (empty map if absent).
pub fn read_dotenv(dir: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(text) = fs::read_to_string(dir.join(".env")) else {
        return map;
    };
    for line in text.lines() {
        if let Some((k, v)) = parse_dotenv_line(line) {
            map.insert(k, v);
        }
    }
    map
}

/// Format a `KEY=VALUE` line for `.env`, quoting when special characters exist.
pub fn format_dotenv_line(key: &str, value: &str) -> String {
    let needs_quote = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '#' | '"' | '\'' | '\\' | '$'));
    if !needs_quote {
        return format!("{key}={value}");
    }
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    format!("{key}={escaped}")
}

/// Add or update `entries` inside `<dir>/.env`, preserving unrelated lines,
/// comments, and ordering. Returns `(added, updated)` counts.
pub fn apply_dotenv(dir: &Path, entries: &[EnvEntry]) -> Result<(usize, usize)> {
    if entries.is_empty() {
        return Ok((0, 0));
    }
    if !dir.is_dir() {
        bail!("Project folder does not exist: {}", dir.display());
    }
    let dotenv = dir.join(".env");
    let existing = if dotenv.exists() {
        fs::read_to_string(&dotenv).with_context(|| format!("Cannot read {}", dotenv.display()))?
    } else {
        String::new()
    };
    let line_ending = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let by_key: HashMap<&str, &EnvEntry> = entries.iter().map(|e| (e.key.as_str(), e)).collect();
    let mut updated_keys: BTreeMap<&str, bool> = BTreeMap::new();
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();

    let mut updated = 0usize;
    for line in &mut lines {
        if let Some((k, _)) = parse_dotenv_line(line) {
            if let Some(entry) = by_key.get(k.as_str()) {
                let had_export = line
                    .trim_start()
                    .strip_prefix("export")
                    .is_some_and(|r| r.chars().next().is_some_and(char::is_whitespace));
                let formatted = format_dotenv_line(&entry.key, &entry.value);
                *line = if had_export {
                    format!("export {formatted}")
                } else {
                    formatted
                };
                if updated_keys.insert(entry.key.as_str(), true).is_none() {
                    updated += 1;
                }
            }
        }
    }

    let mut added = 0usize;
    for entry in entries {
        if !updated_keys.contains_key(entry.key.as_str()) {
            lines.push(format_dotenv_line(&entry.key, &entry.value));
            updated_keys.insert(entry.key.as_str(), false);
            added += 1;
        }
    }

    write_dotenv(dir, &lines, line_ending)?;
    // These are secrets: unless the project already ignores `.env`, keep it
    // out of commits via the local exclude list (deleting the line opts in).
    if crate::ledger::git(dir, &["check-ignore", "-q", ".env"]).is_none() {
        let _ = crate::ledger::exclude_entry_from_git(dir, ".env");
    }
    Ok((added, updated))
}

/// Remove every assignment of `keys` from `<dir>/.env`, keeping all other
/// lines. Returns how many lines were dropped (0 when the file is absent).
pub fn remove_dotenv(dir: &Path, keys: &[String]) -> Result<usize> {
    let dotenv = dir.join(".env");
    if keys.is_empty() || !dotenv.exists() {
        return Ok(0);
    }
    let existing =
        fs::read_to_string(&dotenv).with_context(|| format!("Cannot read {}", dotenv.display()))?;
    let line_ending = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let before = existing.lines().count();
    let lines: Vec<String> = existing
        .lines()
        .filter(|l| !parse_dotenv_line(l).is_some_and(|(k, _)| keys.contains(&k)))
        .map(str::to_string)
        .collect();
    let removed = before - lines.len();
    if removed > 0 {
        write_dotenv(dir, &lines, line_ending)?;
    }
    Ok(removed)
}

/// Atomically replace `<dir>/.env` with `lines` (temp file + rename).
fn write_dotenv(dir: &Path, lines: &[String], line_ending: &str) -> Result<()> {
    let dotenv = dir.join(".env");
    let mut out = lines.join(line_ending);
    if !out.is_empty() {
        out.push_str(line_ending);
    }
    atomic_write_bytes(&dotenv, out.as_bytes(), true)
}

/// Resolve `keys` from `vault` and apply them to `<dir>/.env`.
pub fn apply_dotenv_keys(dir: &Path, vault: &EnvVault, keys: &[String]) -> Result<(usize, usize)> {
    let mut selected = Vec::new();
    for k in keys {
        let trimmed = k.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(entry) = vault.vars.iter().find(|e| e.key == trimmed) else {
            bail!("Unknown shared env key: {trimmed}");
        };
        if !selected.iter().any(|e: &EnvEntry| e.key == entry.key) {
            selected.push(entry.clone());
        }
    }
    apply_dotenv(dir, &selected)
}

/// Carry a vault change of `old` into every `<dir>/.env` that holds it:
/// `Some(entry)` rewrites the value (renaming when the key differs), `None`
/// deletes it. Returns how many `.env` files changed and `dir: error` lines.
pub fn propagate_env<'a>(
    dirs: impl IntoIterator<Item = &'a Path>,
    old: &str,
    new: Option<&EnvEntry>,
) -> (usize, Vec<String>) {
    let (mut changed, mut errors) = (0, Vec::new());
    for dir in dirs {
        let Some(cur) = read_dotenv(dir).remove(old) else {
            continue;
        };
        let res = match new {
            Some(e) if e.key == old && e.value == cur => continue,
            Some(e) if e.key == old => apply_dotenv(dir, std::slice::from_ref(e)).map(drop),
            Some(e) => remove_dotenv(dir, &[old.to_string()])
                .and_then(|_| apply_dotenv(dir, std::slice::from_ref(e)).map(drop)),
            None => remove_dotenv(dir, &[old.to_string()]).map(drop),
        };
        match res {
            Ok(()) => changed += 1,
            Err(e) => errors.push(format!("{}: {e:#}", dir.display())),
        }
    }
    (changed, errors)
}
