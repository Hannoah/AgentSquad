use crate::{
    config::{
        atomic_write_bytes, clean_path, config_path, home_dir_string, load_toml_or_default,
        save_toml_to, SyncState,
    },
    pathpick,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

/// Project-scoped skill directories discovered by agy, codex, muse, and claude.
pub const SKILL_ROOTS: [&str; 2] = [".agents/skills", ".claude/skills"];

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
}

#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct SkillVault {
    #[serde(default)]
    pub skills: Vec<SkillEntry>,
}

/// Snapshot of a project's installed skills relative to the shared vault.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct ProjectSkills {
    pub vault_state: BTreeMap<String, SyncState>,
    pub local_only: Vec<String>,
}

pub fn skills_path() -> Result<PathBuf> {
    if let Some(p) = env::var_os("AGENTSQUAD_SKILLS").filter(|s| !s.is_empty()) {
        return Ok(p.into());
    }
    Ok(config_path()?.with_file_name("skills.toml"))
}

pub fn load_skills_from(p: &Path) -> Result<SkillVault> {
    load_toml_or_default(p)
}

pub fn load_skills() -> Result<SkillVault> {
    load_skills_from(&skills_path()?)
}

pub fn save_skills_to(vault: &SkillVault, p: &Path) -> Result<()> {
    save_toml_to(vault, p, false)
}

pub fn save_skills(vault: &SkillVault) -> Result<()> {
    save_skills_to(vault, &skills_path()?)
}

/// Validate and normalize a skill slug (`[a-z0-9][a-z0-9_-]*`, max 64 chars).
pub fn validate_skill_name(raw: &str) -> Result<String, String> {
    let name = raw.trim().to_ascii_lowercase();
    if name.is_empty() {
        return Err("Skill name cannot be empty.".into());
    }
    if name.len() > 64 {
        return Err("Skill name must be 64 characters or fewer.".into());
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err("Skill name must start with a-z or 0-9.".into());
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
        return Err("Skill name may only contain a-z, 0-9, -, or _.".into());
    }
    Ok(name)
}

pub fn expand_user_path(raw: &str) -> PathBuf {
    let cleaned = clean_path(raw);
    if let Some(expanded) = home_dir_string().and_then(|h| pathpick::expand_home(cleaned, &h)) {
        PathBuf::from(expanded)
    } else {
        PathBuf::from(cleaned)
    }
}

fn looks_like_path(raw: &str) -> bool {
    let t = clean_path(raw);
    if t.is_empty() {
        return false;
    }
    let b = t.as_bytes();
    t.starts_with("~/")
        || t.starts_with("~\\")
        || t.starts_with("./")
        || t.starts_with(".\\")
        || t.starts_with("../")
        || t.starts_with("..\\")
        || t.starts_with('/')
        || t.starts_with("\\\\")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/'))
        || (!t.contains('\n') && t.to_ascii_lowercase().ends_with(".md") && !t.contains(' '))
}

/// Interpret the third editor field as either an existing skill folder/file path
/// or inline markdown instructions. Returns `(source_path, instructions)`.
pub fn classify_source_or_body(raw: &str) -> Result<(Option<String>, String), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Enter a skill folder/SKILL.md path or inline instructions.".into());
    }
    let expanded = expand_user_path(trimmed);
    if expanded.exists() {
        if expanded.is_dir() {
            if find_skill_md(&expanded).is_none() {
                return Err(format!(
                    "Folder {} must contain SKILL.md.",
                    expanded.display()
                ));
            }
            return Ok((Some(clean_path(trimmed).to_string()), String::new()));
        }
        if expanded.is_file() {
            return Ok((Some(clean_path(trimmed).to_string()), String::new()));
        }
        return Err(format!("Unsupported source path: {}", expanded.display()));
    }
    if looks_like_path(trimmed) {
        return Err(format!(
            "Source path does not exist: {}",
            expanded.display()
        ));
    }
    Ok((None, trimmed.to_string()))
}

pub fn find_skill_md(dir: &Path) -> Option<PathBuf> {
    let upper = dir.join("SKILL.md");
    if upper.is_file() {
        return Some(upper);
    }
    let lower = dir.join("skill.md");
    if lower.is_file() {
        return Some(lower);
    }
    None
}

/// Extract a human description from YAML frontmatter (`description: ...`) if present.
pub fn extract_description(markdown: &str) -> Option<String> {
    let text = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let mut lines = rest.lines();
    while let Some(line) = lines.next() {
        let t = line.trim();
        if t == "---" {
            break;
        }
        if let Some(after) = t.strip_prefix("description:") {
            let val = after.trim();
            if val == ">-" || val == ">" || val == "|" || val == "|-" {
                let mut collected = Vec::new();
                for sub in lines.by_ref() {
                    if sub.trim() == "---"
                        || (!sub.starts_with(' ')
                            && !sub.starts_with('\t')
                            && !sub.trim().is_empty())
                    {
                        break;
                    }
                    let st = sub.trim();
                    if !st.is_empty() {
                        collected.push(st);
                    }
                }
                let joined = collected.join(" ");
                if !joined.is_empty() {
                    return Some(joined);
                }
            } else {
                let unquoted = val.trim_matches('"').trim_matches('\'').trim().to_string();
                if !unquoted.is_empty() {
                    return Some(unquoted);
                }
            }
        }
    }
    None
}

/// Derive a fallback description from a source file or directory when none was entered.
pub fn infer_description(source_path: &str) -> Option<String> {
    let p = expand_user_path(source_path);
    let md_path = if p.is_dir() { find_skill_md(&p)? } else { p };
    let text = fs::read_to_string(md_path).ok()?;
    extract_description(&text)
}

pub fn upsert_skill(vault: &mut SkillVault, mut entry: SkillEntry) -> Result<bool, String> {
    entry.name = validate_skill_name(&entry.name)?;
    entry.description = entry.description.trim().to_string();
    if entry.description.is_empty() {
        if let Some(src) = &entry.source_path {
            if let Some(desc) = infer_description(src) {
                entry.description = desc;
            }
        }
    }
    if let Some(existing) = vault.skills.iter_mut().find(|s| s.name == entry.name) {
        *existing = entry;
        Ok(false)
    } else {
        vault.skills.push(entry);
        Ok(true)
    }
}

pub fn remove_skill(vault: &mut SkillVault, name: &str) -> bool {
    let target = name.trim().to_ascii_lowercase();
    let before = vault.skills.len();
    vault.skills.retain(|s| s.name != target);
    vault.skills.len() != before
}

fn has_frontmatter(text: &str) -> bool {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = t
        .strip_prefix("---\n")
        .or_else(|| t.strip_prefix("---\r\n"))
    else {
        return false;
    };
    rest.lines().any(|l| l.trim() == "---")
}

fn yaml_quote(s: &str) -> String {
    let clean: String = s
        .chars()
        .map(|c| if matches!(c, '\r' | '\n') { ' ' } else { c })
        .collect();
    let escaped = clean.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn ensure_frontmatter(name: &str, description: &str, body: &str) -> String {
    let normalized = body.replace("\r\n", "\n");
    if has_frontmatter(&normalized) {
        let mut out = normalized;
        if !out.ends_with('\n') {
            out.push('\n');
        }
        return out;
    }
    let desc = if description.trim().is_empty() {
        format!("Project skill '{name}' managed by AgentSquad")
    } else {
        description.trim().to_string()
    };
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        format!(
            "---\nname: {name}\ndescription: {}\n---\n\n# {name}\n",
            yaml_quote(&desc)
        )
    } else {
        format!(
            "---\nname: {name}\ndescription: {}\n---\n\n{trimmed}\n",
            yaml_quote(&desc)
        )
    }
}

/// Produce the exact `SKILL.md` contents that should be written for `entry`.
pub fn render_skill_md(entry: &SkillEntry) -> Result<String> {
    if let Some(src) = &entry.source_path {
        let p = expand_user_path(src);
        let md_path = if p.is_dir() {
            find_skill_md(&p)
                .with_context(|| format!("Folder {} must contain SKILL.md", p.display()))?
        } else if p.is_file() {
            p
        } else {
            bail!("Skill source path not found: {}", p.display());
        };
        let raw = fs::read_to_string(&md_path)
            .with_context(|| format!("Cannot read {}", md_path.display()))?;
        Ok(ensure_frontmatter(&entry.name, &entry.description, &raw))
    } else {
        Ok(ensure_frontmatter(
            &entry.name,
            &entry.description,
            &entry.instructions,
        ))
    }
}

fn collect_source_files(
    root: &Path,
    cur: &Path,
    out: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<()> {
    for entry in fs::read_dir(cur).with_context(|| format!("Cannot read {}", cur.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_source_files(root, &path, out)?;
        } else if file_type.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            // Skip root `skill.md` / `SKILL.md`: rendered separately with frontmatter guaranteed.
            if rel.components().count() == 1
                && rel.to_string_lossy().eq_ignore_ascii_case("SKILL.md")
            {
                continue;
            }
            let bytes =
                fs::read(&path).with_context(|| format!("Cannot read {}", path.display()))?;
            out.insert(rel, bytes);
        }
    }
    Ok(())
}

fn expected_skill_tree(entry: &SkillEntry) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let skill_md = render_skill_md(entry)?;
    files.insert(PathBuf::from("SKILL.md"), skill_md.into_bytes());
    if let Some(src) = &entry.source_path {
        let p = expand_user_path(src);
        if p.is_dir() {
            collect_source_files(&p, &p, &mut files)?;
        }
    }
    Ok(files)
}

fn read_dir_tree(root: &Path) -> Option<BTreeMap<PathBuf, Vec<u8>>> {
    if !root.is_dir() {
        return None;
    }
    let mut out = BTreeMap::new();
    fn walk(base: &Path, cur: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) -> Option<()> {
        for entry in fs::read_dir(cur).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            let ft = entry.file_type().ok()?;
            if ft.is_dir() {
                walk(base, &path, out)?;
            } else if ft.is_file() {
                let rel = path.strip_prefix(base).ok()?.to_path_buf();
                let bytes = fs::read(&path).ok()?;
                out.insert(rel, bytes);
            }
        }
        Some(())
    }
    walk(root, root, &mut out)?;
    Some(out)
}

/// Inspect `<dir>/.agents/skills` and `<dir>/.claude/skills` against `vault`.
pub fn read_project_skills(dir: &Path, vault: &SkillVault) -> ProjectSkills {
    let mut state = ProjectSkills::default();
    if !dir.is_dir() {
        return state;
    }
    let vault_names: BTreeSet<&str> = vault.skills.iter().map(|s| s.name.as_str()).collect();
    let mut local_set: BTreeSet<String> = BTreeSet::new();

    for root in SKILL_ROOTS {
        let skills_dir = dir.join(root);
        let Ok(entries) = fs::read_dir(&skills_dir) else {
            continue;
        };
        for item in entries.flatten() {
            let path = item.path();
            if !path.is_dir() || find_skill_md(&path).is_none() {
                continue;
            }
            let Some(name) = item.file_name().into_string().ok() else {
                continue;
            };
            if !vault_names.contains(name.as_str()) {
                local_set.insert(name);
            }
        }
    }
    state.local_only = local_set.into_iter().collect();

    for entry in &vault.skills {
        let trees: Vec<Option<BTreeMap<PathBuf, Vec<u8>>>> = SKILL_ROOTS
            .iter()
            .map(|r| read_dir_tree(&dir.join(r).join(&entry.name)))
            .collect();
        let present = trees.iter().filter(|t| t.is_some()).count();
        if present == 0 {
            continue;
        }
        let sync = match expected_skill_tree(entry) {
            Ok(want)
                if present == SKILL_ROOTS.len()
                    && trees.iter().all(|t| t.as_ref() == Some(&want)) =>
            {
                SyncState::Installed
            }
            _ => SyncState::Differs,
        };
        state.vault_state.insert(entry.name.clone(), sync);
    }
    state
}

fn sync_skill_dir(
    dest: &Path,
    files: &BTreeMap<PathBuf, Vec<u8>>,
    src_dir: Option<&Path>,
) -> Result<()> {
    // Guard against copying a skill folder onto itself when source == destination.
    if let (Some(src), Ok(dest_canon)) = (src_dir, fs::canonicalize(dest)) {
        if fs::canonicalize(src).ok().as_ref() == Some(&dest_canon) {
            if let Some(md_bytes) = files.get(Path::new("SKILL.md")) {
                atomic_write_bytes(&dest.join("SKILL.md"), md_bytes, false)?;
            }
            return Ok(());
        }
    }
    if dest.exists() {
        fs::remove_dir_all(dest).with_context(|| format!("Cannot replace {}", dest.display()))?;
    }
    fs::create_dir_all(dest).with_context(|| format!("Cannot create {}", dest.display()))?;
    for (rel, bytes) in files {
        let target = dest.join(rel);
        atomic_write_bytes(&target, bytes, false)?;
    }
    Ok(())
}

/// Install or update `entries` into `<dir>/.agents/skills/<name>` and `<dir>/.claude/skills/<name>`.
pub fn apply_skills(dir: &Path, entries: &[SkillEntry]) -> Result<(usize, usize)> {
    if entries.is_empty() {
        return Ok((0, 0));
    }
    if !dir.is_dir() {
        bail!("Project folder does not exist: {}", dir.display());
    }
    let mut added = 0usize;
    let mut updated = 0usize;
    for entry in entries {
        let files = expected_skill_tree(entry)?;
        let src_dir = entry
            .source_path
            .as_deref()
            .map(expand_user_path)
            .filter(|p| p.is_dir());
        let existed = SKILL_ROOTS
            .iter()
            .any(|r| dir.join(r).join(&entry.name).exists());
        for root in SKILL_ROOTS {
            let dest = dir.join(root).join(&entry.name);
            sync_skill_dir(&dest, &files, src_dir.as_deref())?;
        }
        if existed {
            updated += 1;
        } else {
            added += 1;
        }
    }
    Ok((added, updated))
}

pub fn apply_skill_names(
    dir: &Path,
    vault: &SkillVault,
    names: &[String],
) -> Result<(usize, usize)> {
    let mut selected = Vec::new();
    for raw in names {
        let name = validate_skill_name(raw).map_err(anyhow::Error::msg)?;
        let Some(entry) = vault.skills.iter().find(|s| s.name == name) else {
            bail!("Unknown shared skill: {name}");
        };
        if !selected.iter().any(|s: &SkillEntry| s.name == entry.name) {
            selected.push(entry.clone());
        }
    }
    apply_skills(dir, &selected)
}

/// Remove `names` from `<dir>/.agents/skills/` and `<dir>/.claude/skills/`.
pub fn remove_skills(dir: &Path, names: &[String]) -> Result<usize> {
    if names.is_empty() || !dir.is_dir() {
        return Ok(0);
    }
    let mut removed = 0usize;
    for raw in names {
        let name = raw.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let mut any = false;
        for root in SKILL_ROOTS {
            let target = dir.join(root).join(&name);
            if target.exists() {
                fs::remove_dir_all(&target)
                    .with_context(|| format!("Cannot remove {}", target.display()))?;
                any = true;
            }
            let root_dir = dir.join(root);
            if root_dir
                .read_dir()
                .ok()
                .is_some_and(|mut it| it.next().is_none())
            {
                let _ = fs::remove_dir(&root_dir);
            }
        }
        if any {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Sync checked skills into `<dir>` and remove unchecked vault skills, leaving local-only skills untouched.
pub fn sync_project_skills(
    dir: &Path,
    vault: &SkillVault,
    picked: &BTreeSet<String>,
    current: &ProjectSkills,
) -> Result<(usize, usize, usize)> {
    let write: Vec<String> = picked
        .iter()
        .filter(|name| current.vault_state.get(*name) != Some(&SyncState::Installed))
        .cloned()
        .collect();
    let remove: Vec<String> = vault
        .skills
        .iter()
        .filter(|s| current.vault_state.contains_key(&s.name) && !picked.contains(&s.name))
        .map(|s| s.name.clone())
        .collect();
    if write.is_empty() && remove.is_empty() {
        return Ok((0, 0, 0));
    }
    let (added, updated) = apply_skill_names(dir, vault, &write)?;
    let removed = remove_skills(dir, &remove)?;
    Ok((added, updated, removed))
}
