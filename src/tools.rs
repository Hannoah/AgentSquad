use crate::{
    config::{
        atomic_write_bytes, config_path, load_toml_or_default, save_toml_to, split_command,
        SyncState,
    },
    json::{self, JsonValue},
    ledger,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    #[default]
    Mcp,
    Cli,
}

impl ToolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolKind::Mcp => "mcp",
            ToolKind::Cli => "cli",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            ToolKind::Mcp => ToolKind::Cli,
            ToolKind::Cli => ToolKind::Mcp,
        }
    }
}

impl FromStr for ToolKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mcp" => Ok(ToolKind::Mcp),
            "cli" => Ok(ToolKind::Cli),
            other => bail!("Unknown tool kind: {other}. Use mcp or cli"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ToolEntry {
    pub name: String,
    #[serde(default)]
    pub kind: ToolKind,
    /// For MCP: executable + args (`npx -y ...`) or streamable-HTTP URL (`https://...`).
    /// For CLI: command or invocation (`gh pr list`, `cargo test --locked`).
    pub command: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ToolVault {
    #[serde(default)]
    pub tools: Vec<ToolEntry>,
}

/// Snapshot of a project's configured tools relative to the shared vault.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct ProjectTools {
    pub vault_state: BTreeMap<String, SyncState>,
    pub local_only: Vec<(String, ToolKind)>,
}

pub fn tools_path() -> Result<PathBuf> {
    if let Some(p) = env::var_os("AGENTSQUAD_TOOLS").filter(|s| !s.is_empty()) {
        return Ok(p.into());
    }
    Ok(config_path()?.with_file_name("tools.toml"))
}

pub fn load_tools_from(p: &Path) -> Result<ToolVault> {
    load_toml_or_default(p)
}

pub fn load_tools() -> Result<ToolVault> {
    load_tools_from(&tools_path()?)
}

pub fn save_tools_to(vault: &ToolVault, p: &Path) -> Result<()> {
    save_toml_to(vault, p, false)
}

pub fn save_tools(vault: &ToolVault) -> Result<()> {
    save_tools_to(vault, &tools_path()?)
}

/// Validate a tool identifier (`[A-Za-z0-9][A-Za-z0-9_-]*`, max 64 chars).
pub fn validate_tool_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("Tool name cannot be empty.".into());
    }
    if name.len() > 64 {
        return Err("Tool name must be 64 characters or fewer.".into());
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err("Tool name must start with A-Z, a-z, or 0-9.".into());
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
        return Err("Tool name may only contain A-Z, a-z, 0-9, -, or _.".into());
    }
    Ok(name.to_string())
}

pub fn is_http_url(cmd: &str) -> bool {
    let t = cmd.trim();
    (t.starts_with("http://") || t.starts_with("https://")) && !t.chars().any(char::is_whitespace)
}

pub fn validate_tool_command(kind: ToolKind, raw: &str) -> Result<String, String> {
    let cmd = raw.trim();
    if cmd.is_empty() {
        return Err("Command or URL cannot be empty.".into());
    }
    if kind == ToolKind::Mcp && !is_http_url(cmd) && split_command(cmd).is_empty() {
        return Err("Enter a command (e.g. npx -y ...) or an http(s) URL.".into());
    }
    Ok(cmd.to_string())
}

pub fn upsert_tool(vault: &mut ToolVault, mut entry: ToolEntry) -> Result<bool, String> {
    entry.name = validate_tool_name(&entry.name)?;
    entry.command = validate_tool_command(entry.kind, &entry.command)?;
    entry.description = entry.description.trim().to_string();
    if let Some(existing) = vault
        .tools
        .iter_mut()
        .find(|t| t.name.eq_ignore_ascii_case(&entry.name))
    {
        *existing = entry;
        Ok(false)
    } else {
        vault.tools.push(entry);
        Ok(true)
    }
}

pub fn remove_tool(vault: &mut ToolVault, name: &str) -> bool {
    let target = name.trim();
    let before = vault.tools.len();
    vault.tools.retain(|t| !t.name.eq_ignore_ascii_case(target));
    vault.tools.len() != before
}

fn mcp_json_entry(entry: &ToolEntry, include_http_type: bool) -> JsonValue {
    let mut obj = Vec::new();
    let cmd = entry.command.trim();
    if is_http_url(cmd) {
        if include_http_type {
            obj.push(("type".to_string(), JsonValue::String("http".to_string())));
        }
        obj.push(("url".to_string(), JsonValue::String(cmd.to_string())));
    } else {
        let parts = split_command(cmd);
        let program = parts.first().cloned().unwrap_or_default();
        let args: Vec<JsonValue> = parts.into_iter().skip(1).map(JsonValue::String).collect();
        obj.push(("command".to_string(), JsonValue::String(program)));
        if !args.is_empty() {
            obj.push(("args".to_string(), JsonValue::Array(args)));
        }
        if !entry.env.is_empty() {
            let env_obj = entry
                .env
                .iter()
                .map(|(k, v)| (k.clone(), JsonValue::String(v.clone())))
                .collect();
            obj.push(("env".to_string(), JsonValue::Object(env_obj)));
        }
    }
    JsonValue::Object(obj)
}

fn codex_mcp_table(entry: &ToolEntry) -> toml::map::Map<String, toml::Value> {
    let mut tbl = toml::map::Map::new();
    let cmd = entry.command.trim();
    if is_http_url(cmd) {
        tbl.insert("url".to_string(), toml::Value::String(cmd.to_string()));
    } else {
        let parts = split_command(cmd);
        let program = parts.first().cloned().unwrap_or_default();
        let args: Vec<toml::Value> = parts.into_iter().skip(1).map(toml::Value::String).collect();
        tbl.insert("command".to_string(), toml::Value::String(program));
        if !args.is_empty() {
            tbl.insert("args".to_string(), toml::Value::Array(args));
        }
        if !entry.env.is_empty() {
            let mut env_tbl = toml::map::Map::new();
            for (k, v) in &entry.env {
                env_tbl.insert(k.clone(), toml::Value::String(v.clone()));
            }
            tbl.insert("env".to_string(), toml::Value::Table(env_tbl));
        }
    }
    tbl
}

fn read_json_mcp_servers(path: &Path) -> Result<Option<JsonValue>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let val =
        json::parse(&text).map_err(|e| anyhow::anyhow!("Cannot parse {}: {e}", path.display()))?;
    if val.as_object().is_none() {
        bail!("{} must contain a JSON object", path.display());
    }
    Ok(Some(val))
}

fn write_json_mcp_servers(
    path: &Path,
    upsert: &[&ToolEntry],
    remove: &[String],
    include_http_type: bool,
) -> Result<()> {
    let existing = read_json_mcp_servers(path)?;
    if existing.is_none() && upsert.is_empty() {
        return Ok(());
    }
    let mut root = existing.unwrap_or_else(JsonValue::empty_object);
    let mut servers = match root.remove("mcpServers") {
        Some(JsonValue::Object(entries)) => JsonValue::Object(entries),
        Some(_) => bail!("{} has a non-object mcpServers field", path.display()),
        None => JsonValue::empty_object(),
    };
    for name in remove {
        servers.remove(name);
    }
    for entry in upsert {
        servers.set(entry.name.clone(), mcp_json_entry(entry, include_http_type));
    }
    let servers_empty = servers.as_object().is_some_and(|e| e.is_empty());
    let root_empty = root.as_object().is_some_and(|e| e.is_empty());
    if servers_empty && root_empty {
        if path.is_file() {
            fs::remove_file(path).with_context(|| format!("Cannot remove {}", path.display()))?;
            cleanup_empty_parent(path);
        }
        return Ok(());
    }
    root.set("mcpServers", servers);
    atomic_write_bytes(path, root.to_pretty().as_bytes(), false)
}

fn read_codex_toml(path: &Path) -> Result<Option<toml::map::Map<String, toml::Value>>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let tbl: toml::map::Map<String, toml::Value> =
        toml::from_str(&text).with_context(|| format!("Cannot parse {}", path.display()))?;
    Ok(Some(tbl))
}

fn write_codex_mcp_servers(path: &Path, upsert: &[&ToolEntry], remove: &[String]) -> Result<()> {
    let existing = read_codex_toml(path)?;
    if existing.is_none() && upsert.is_empty() {
        return Ok(());
    }
    let mut root = existing.unwrap_or_default();
    let mut servers = match root.remove("mcp_servers") {
        Some(toml::Value::Table(t)) => t,
        Some(_) => bail!("{} has a non-table mcp_servers field", path.display()),
        None => toml::map::Map::new(),
    };
    for name in remove {
        servers.remove(name);
    }
    for entry in upsert {
        servers.insert(
            entry.name.clone(),
            toml::Value::Table(codex_mcp_table(entry)),
        );
    }
    if servers.is_empty() && root.is_empty() {
        if path.is_file() {
            fs::remove_file(path).with_context(|| format!("Cannot remove {}", path.display()))?;
            cleanup_empty_parent(path);
        }
        return Ok(());
    }
    if !servers.is_empty() {
        root.insert("mcp_servers".to_string(), toml::Value::Table(servers));
    }
    let text = toml::to_string_pretty(&root)?;
    atomic_write_bytes(path, text.as_bytes(), false)
}

fn cleanup_empty_parent(file: &Path) {
    if let Some(parent) = file.parent() {
        if parent
            .read_dir()
            .ok()
            .is_some_and(|mut it| it.next().is_none())
        {
            let _ = fs::remove_dir(parent);
        }
    }
}

fn squad_tools_path(dir: &Path) -> PathBuf {
    dir.join(ledger::DIR).join("TOOLS.md")
}

/// Parse `<dir>/.squad/TOOLS.md` into `name -> (command, description)`.
pub fn read_squad_tools(dir: &Path) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    let Ok(text) = fs::read_to_string(squad_tools_path(dir)) else {
        return out;
    };
    let mut current_name: Option<String> = None;
    let mut current_cmd = String::new();
    let mut current_desc = String::new();
    let flush = |name: &mut Option<String>,
                 cmd: &mut String,
                 desc: &mut String,
                 out: &mut BTreeMap<String, (String, String)>| {
        if let Some(n) = name.take() {
            out.insert(n, (std::mem::take(cmd), std::mem::take(desc)));
        } else {
            cmd.clear();
            desc.clear();
        }
    };
    for line in text.lines() {
        let t = line.trim();
        if let Some(heading) = t.strip_prefix("### ") {
            flush(
                &mut current_name,
                &mut current_cmd,
                &mut current_desc,
                &mut out,
            );
            let h = heading.trim();
            if !h.is_empty() {
                current_name = Some(h.to_string());
            }
        } else if current_name.is_some() {
            if let Some(after) = t.strip_prefix("- Command:") {
                let c = after.trim();
                let c = c
                    .strip_prefix('`')
                    .and_then(|s| s.strip_suffix('`'))
                    .unwrap_or(c);
                current_cmd = c.trim().to_string();
            } else if let Some(after) = t.strip_prefix("- Description:") {
                current_desc = after.trim().to_string();
            }
        }
    }
    flush(
        &mut current_name,
        &mut current_cmd,
        &mut current_desc,
        &mut out,
    );
    out
}

fn write_squad_tools(dir: &Path, entries: &BTreeMap<String, (String, String)>) -> Result<()> {
    let file = squad_tools_path(dir);
    if entries.is_empty() {
        if file.is_file() {
            fs::remove_file(&file).with_context(|| format!("Cannot remove {}", file.display()))?;
        }
        return Ok(());
    }
    fs::create_dir_all(dir.join(ledger::DIR))?;
    let _ = ledger::exclude_from_git(dir);
    let mut out = String::from(
        "# Project Tools\n\nWritten by squad whenever tools are assigned. Do not edit.\n",
    );
    for (name, (cmd, desc)) in entries {
        out.push_str(&format!("\n### {name}\n- Command: `{cmd}`\n"));
        if !desc.trim().is_empty() {
            out.push_str(&format!("- Description: {}\n", desc.trim()));
        }
    }
    atomic_write_bytes(&file, out.as_bytes(), false)
}

fn mcp_paths(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        dir.join(".mcp.json"),
        dir.join(".agents").join("mcp_config.json"),
        dir.join(".codex").join("config.toml"),
    )
}

fn read_json_servers_map(path: &Path) -> BTreeMap<String, JsonValue> {
    let mut map = BTreeMap::new();
    let Ok(Some(root)) = read_json_mcp_servers(path) else {
        return map;
    };
    if let Some(entries) = root.get("mcpServers").and_then(|v| v.as_object()) {
        for (k, v) in entries {
            map.insert(k.clone(), v.clone());
        }
    }
    map
}

fn read_codex_servers_map(path: &Path) -> BTreeMap<String, toml::map::Map<String, toml::Value>> {
    let mut map = BTreeMap::new();
    let Ok(Some(root)) = read_codex_toml(path) else {
        return map;
    };
    if let Some(toml::Value::Table(servers)) = root.get("mcp_servers") {
        for (k, v) in servers {
            if let toml::Value::Table(t) = v {
                map.insert(k.clone(), t.clone());
            }
        }
    }
    map
}

/// Inspect `<dir>` MCP configs (`.mcp.json`, `.agents/mcp_config.json`, `.codex/config.toml`)
/// and `.squad/TOOLS.md` against `vault`.
pub fn read_project_tools(dir: &Path, vault: &ToolVault) -> ProjectTools {
    let mut state = ProjectTools::default();
    if !dir.is_dir() {
        return state;
    }
    let (claude_path, agy_path, codex_path) = mcp_paths(dir);
    let claude_map = read_json_servers_map(&claude_path);
    let agy_map = read_json_servers_map(&agy_path);
    let codex_map = read_codex_servers_map(&codex_path);
    let cli_map = read_squad_tools(dir);

    let vault_names: BTreeSet<&str> = vault.tools.iter().map(|t| t.name.as_str()).collect();
    let mut local_mcp: BTreeSet<String> = BTreeSet::new();
    for name in claude_map
        .keys()
        .chain(agy_map.keys())
        .chain(codex_map.keys())
    {
        if !vault_names.contains(name.as_str()) {
            local_mcp.insert(name.clone());
        }
    }
    for name in local_mcp {
        state.local_only.push((name, ToolKind::Mcp));
    }
    for name in cli_map.keys() {
        if !vault_names.contains(name.as_str()) {
            state.local_only.push((name.clone(), ToolKind::Cli));
        }
    }

    for entry in &vault.tools {
        match entry.kind {
            ToolKind::Mcp => {
                let c = claude_map.get(&entry.name);
                let a = agy_map.get(&entry.name);
                let x = codex_map.get(&entry.name);
                if c.is_none() && a.is_none() && x.is_none() {
                    continue;
                }
                let want_c = mcp_json_entry(entry, true);
                let want_a = mcp_json_entry(entry, false);
                let want_x = codex_mcp_table(entry);
                let all_match = c == Some(&want_c) && a == Some(&want_a) && x == Some(&want_x);
                state.vault_state.insert(
                    entry.name.clone(),
                    if all_match {
                        SyncState::Installed
                    } else {
                        SyncState::Differs
                    },
                );
            }
            ToolKind::Cli => {
                if let Some((cmd, desc)) = cli_map.get(&entry.name) {
                    let matches = cmd == entry.command.trim() && desc == entry.description.trim();
                    state.vault_state.insert(
                        entry.name.clone(),
                        if matches {
                            SyncState::Installed
                        } else {
                            SyncState::Differs
                        },
                    );
                }
            }
        }
    }
    state
}

/// Apply `entries` to `<dir>`: MCP entries update `.mcp.json`, `.agents/mcp_config.json`,
/// and `.codex/config.toml`; CLI entries update `.squad/TOOLS.md`.
pub fn apply_tools(dir: &Path, entries: &[ToolEntry]) -> Result<(usize, usize)> {
    if entries.is_empty() {
        return Ok((0, 0));
    }
    if !dir.is_dir() {
        bail!("Project folder does not exist: {}", dir.display());
    }
    let (claude_path, agy_path, codex_path) = mcp_paths(dir);
    let claude_map = read_json_servers_map(&claude_path);
    let agy_map = read_json_servers_map(&agy_path);
    let codex_map = read_codex_servers_map(&codex_path);
    let mut cli_map = read_squad_tools(dir);

    let mut mcps = Vec::new();
    let mut added = 0usize;
    let mut updated = 0usize;
    let mut cli_dirty = false;

    for entry in entries {
        match entry.kind {
            ToolKind::Mcp => {
                let existed = claude_map.contains_key(&entry.name)
                    || agy_map.contains_key(&entry.name)
                    || codex_map.contains_key(&entry.name);
                if existed {
                    updated += 1;
                } else {
                    added += 1;
                }
                mcps.push(entry);
            }
            ToolKind::Cli => {
                let existed = cli_map.contains_key(&entry.name);
                cli_map.insert(
                    entry.name.clone(),
                    (
                        entry.command.trim().to_string(),
                        entry.description.trim().to_string(),
                    ),
                );
                cli_dirty = true;
                if existed {
                    updated += 1;
                } else {
                    added += 1;
                }
            }
        }
    }

    if !mcps.is_empty() {
        write_json_mcp_servers(&claude_path, &mcps, &[], true)?;
        write_json_mcp_servers(&agy_path, &mcps, &[], false)?;
        write_codex_mcp_servers(&codex_path, &mcps, &[])?;
    }
    if cli_dirty {
        write_squad_tools(dir, &cli_map)?;
    }
    Ok((added, updated))
}

pub fn apply_tool_names(dir: &Path, vault: &ToolVault, names: &[String]) -> Result<(usize, usize)> {
    let mut selected = Vec::new();
    for raw in names {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(entry) = vault
            .tools
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(trimmed))
        else {
            bail!("Unknown shared tool: {trimmed}");
        };
        if !selected
            .iter()
            .any(|t: &ToolEntry| t.name.eq_ignore_ascii_case(&entry.name))
        {
            selected.push(entry.clone());
        }
    }
    apply_tools(dir, &selected)
}

/// Remove `names` from `.mcp.json`, `.agents/mcp_config.json`, `.codex/config.toml`,
/// and `.squad/TOOLS.md` inside `<dir>`, leaving unrelated entries intact.
pub fn remove_tools(dir: &Path, names: &[String]) -> Result<usize> {
    if names.is_empty() || !dir.is_dir() {
        return Ok(0);
    }
    let targets: Vec<String> = names
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if targets.is_empty() {
        return Ok(0);
    }
    let (claude_path, agy_path, codex_path) = mcp_paths(dir);
    let claude_map = read_json_servers_map(&claude_path);
    let agy_map = read_json_servers_map(&agy_path);
    let codex_map = read_codex_servers_map(&codex_path);
    let mut cli_map = read_squad_tools(dir);

    let mut removed = 0usize;
    let mut mcp_remove = Vec::new();
    let mut cli_changed = false;

    for name in &targets {
        let mut hit = false;
        if claude_map.contains_key(name)
            || agy_map.contains_key(name)
            || codex_map.contains_key(name)
        {
            mcp_remove.push(name.clone());
            hit = true;
        }
        if cli_map.remove(name).is_some() {
            cli_changed = true;
            hit = true;
        }
        if hit {
            removed += 1;
        }
    }

    if !mcp_remove.is_empty() {
        write_json_mcp_servers(&claude_path, &[], &mcp_remove, true)?;
        write_json_mcp_servers(&agy_path, &[], &mcp_remove, false)?;
        write_codex_mcp_servers(&codex_path, &[], &mcp_remove)?;
    }
    if cli_changed {
        write_squad_tools(dir, &cli_map)?;
    }
    Ok(removed)
}

/// Sync checked tools into `<dir>` and remove unchecked vault tools, leaving local tools untouched.
pub fn sync_project_tools(
    dir: &Path,
    vault: &ToolVault,
    picked: &BTreeSet<String>,
    current: &ProjectTools,
) -> Result<(usize, usize, usize)> {
    let write: Vec<String> = picked
        .iter()
        .filter(|name| current.vault_state.get(*name) != Some(&SyncState::Installed))
        .cloned()
        .collect();
    let remove: Vec<String> = vault
        .tools
        .iter()
        .filter(|t| current.vault_state.contains_key(&t.name) && !picked.contains(&t.name))
        .map(|t| t.name.clone())
        .collect();
    if write.is_empty() && remove.is_empty() {
        return Ok((0, 0, 0));
    }
    let (added, updated) = apply_tool_names(dir, vault, &write)?;
    let removed = remove_tools(dir, &remove)?;
    Ok((added, updated, removed))
}
