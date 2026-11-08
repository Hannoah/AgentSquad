use crate::session::Session;
use anyhow::{bail, Context, Result};
use portable_pty::PtySize;
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Agent,
    Service,
}
impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Agent => "agent",
            Kind::Service => "service",
        }
    }
    /// The other kind, for the add form's kind toggle.
    pub fn toggled(self) -> Kind {
        match self {
            Kind::Agent => Kind::Service,
            Kind::Service => Kind::Agent,
        }
    }
}
impl FromStr for Kind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "agent" => Ok(Kind::Agent),
            "service" => Ok(Kind::Service),
            other => bail!("Unknown kind: {other}. Use agent or service"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Project {
    pub name: String,
    pub path: PathBuf,
    pub command: Vec<String>,
    #[serde(default)]
    pub kind: Kind,
    /// Start this project's session as soon as the manager opens.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub autostart: bool,
}
/// Bump when `load_from` gains a one-time migration. A file stamped with
/// this version is left exactly as the user wrote it.
pub const CONFIG_VERSION: u32 = 1;
#[derive(Serialize, Deserialize)]
pub struct Config {
    /// Migrations already applied; a file without it predates versioning.
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub projects: Vec<Project>,
    /// Lines of scrollback kept per session (default 2000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback: Option<usize>,
    /// Ring the terminal bell when a background agent needs input or
    /// finishes (default true; `bell = false` silences it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bell: Option<bool>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            projects: Vec::new(),
            scrollback: None,
            bell: None,
        }
    }
}
pub fn config_path() -> Result<PathBuf> {
    if let Some(p) = env::var_os("AGENTSQUAD_CONFIG") {
        return Ok(p.into());
    }
    let base = env::var_os("LOCALAPPDATA")
        .or_else(|| env::var_os("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .context("Set AGENTSQUAD_CONFIG to a writable config file")?;
    Ok(base.join("AgentSquad").join("config.toml"))
}
pub fn load_from(p: &Path) -> Result<Config> {
    if !p.exists() {
        return Ok(Config::default());
    }
    let text = fs::read_to_string(p).with_context(|| format!("Cannot read {}", p.display()))?;
    let mut config: Config =
        toml::from_str(&text).with_context(|| format!("Cannot parse {}", p.display()))?;
    let mut seen_names: Vec<String> = Vec::new();
    // Legacy rewrites run once per file, so a deliberate `cmd.exe` or a
    // service named after its folder survives later loads.
    let migrate = config.version < CONFIG_VERSION;
    config.version = CONFIG_VERSION;
    for proj in &mut config.projects {
        proj.path = plain_path(proj.path.clone());
        if !migrate {
            continue;
        }
        // Legacy migration: in <=0.5.9, the default shell on Windows was bare
        // `cmd.exe`. Upgrade stored bare `cmd.exe` defaults to PowerShell.
        if cfg!(windows)
            && proj.command.len() == 1
            && proj.command[0].eq_ignore_ascii_case("cmd.exe")
        {
            proj.command = shell();
        }
        // Legacy migration: in <=0.5.4, services defaulted to directory name.
        // If a service's name matches its directory name, migrate it to the
        // served program name so it reads as a service rather than a folder.
        // Conversely, if a bare-shell service was previously renamed to "cmd",
        // restore its directory name until a foreground process runs inside it.
        if proj.kind == Kind::Service {
            let dir_name = proj
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !dir_name.is_empty()
                && is_bare_shell(&proj.command)
                && proj.name.eq_ignore_ascii_case("cmd")
            {
                proj.name = dir_name.clone();
            }
            if !dir_name.is_empty() && proj.name.eq_ignore_ascii_case(&dir_name) {
                if let Some(prog) = program_name(&proj.command) {
                    let mut candidate = prog.clone();
                    let mut i = 2;
                    while seen_names
                        .iter()
                        .any(|n| n.eq_ignore_ascii_case(&candidate))
                    {
                        candidate = format!("{prog}-{i}");
                        i += 1;
                    }
                    proj.name = candidate;
                }
            }
        }
        seen_names.push(proj.name.clone());
    }
    Ok(config)
}
pub fn load() -> Result<Config> {
    let p = config_path()?;
    load_from(&p)
}
pub fn save_to(c: &Config, p: &Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    // Write-then-rename so a crash mid-write never leaves a torn config.
    // pid in the name: two squads saving at once must not share a temp file.
    let tmp = p.with_extension(format!("toml.tmp{}", std::process::id()));
    fs::write(&tmp, toml::to_string_pretty(c)?)?;
    fs::rename(&tmp, p)?;
    Ok(())
}
pub fn save(c: &Config) -> Result<()> {
    let p = config_path()?;
    save_to(c, &p)
}
/// Register `p` on disk, re-reading the file first so projects added or
/// removed by another process are not clobbered.
pub fn persist_add(p: &Project) -> Result<()> {
    let mut disk = load()?;
    if !disk
        .projects
        .iter()
        .any(|q| q.name.eq_ignore_ascii_case(&p.name))
    {
        disk.projects.push(p.clone());
    }
    save(&disk)
}
/// Unregister `name` on disk, re-reading the file first (see `persist_add`).
pub fn persist_remove(name: &str) -> Result<()> {
    let mut disk = load()?;
    disk.projects.retain(|q| !q.name.eq_ignore_ascii_case(name));
    save(&disk)
}
/// Drop the Windows verbatim prefix `fs::canonicalize` adds, so stored
/// paths read `C:\x` instead of `\\?\C:\x` (many tools choke on the latter).
pub fn plain_path(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}
/// Split a command line on whitespace, keeping "double quoted" runs whole.
pub fn split_command(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}
/// Default command for a project without one: PowerShell on Windows
/// (PowerShell 7 `pwsh.exe` when installed, otherwise the built-in
/// `powershell.exe`). The resolved command is stored in the config, so it
/// never changes underneath an existing project.
pub fn shell() -> Vec<String> {
    if cfg!(windows) {
        let exe = if on_path("pwsh.exe") {
            "pwsh.exe"
        } else {
            "powershell.exe"
        };
        vec![exe.into(), "-NoLogo".into()]
    } else {
        vec!["sh".into()]
    }
}
fn on_path(name: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|d| d.join(name).is_file()))
}
/// True for interactive shells and console hosts (`cmd`, `powershell`,
/// `pwsh`, `bash`, ...), by program name with path and extension ignored.
pub fn is_shell_program(prog: &str) -> bool {
    const SHELLS: [&str; 14] = [
        "cmd",
        "powershell",
        "pwsh",
        "conhost",
        "openconsole",
        "bash",
        "sh",
        "zsh",
        "fish",
        "nu",
        "wsl",
        "wslhost",
        "windowsterminal",
        "wt",
    ];
    let lower = program_stem(prog).to_ascii_lowercase();
    SHELLS.contains(&lower.as_str())
}
/// A project whose command is just a shell (no program of its own), so
/// what it runs is decided at the prompt.
pub fn is_bare_shell(command: &[String]) -> bool {
    command.first().is_some_and(|p| is_shell_program(p)) && program_name(command).is_none()
}
/// Fold in projects registered by another process (e.g. `squad add` in a
/// second terminal) so a later save does not clobber them. Only appends
/// names we don't have, keeping `sessions` aligned by index.
pub fn merge_projects(c: &mut Config, sessions: &mut Vec<Option<Session>>, disk: Config) {
    for p in disk.projects {
        if !c
            .projects
            .iter()
            .any(|q| q.name.eq_ignore_ascii_case(&p.name))
        {
            c.projects.push(p);
            sessions.push(None);
        }
    }
}
/// Environment advertised to sessions. `COLORTERM` unlocks 24-bit color
/// for apps like Claude that downgrade without it; the parser and the
/// renderer already pass RGB through.
pub fn pty_env() -> [(&'static str, &'static str); 2] {
    [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]
}
pub fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}
/// Sidebar order: all agents first, then all services. Positions in this
/// vector are what the manager selection index refers to.
pub fn display_order(projects: &[Project]) -> Vec<usize> {
    let mut order = Vec::with_capacity(projects.len());
    order.extend((0..projects.len()).filter(|&i| projects[i].kind == Kind::Agent));
    order.extend((0..projects.len()).filter(|&i| projects[i].kind == Kind::Service));
    order
}
/// Find a unique project name by appending `-2`, `-3`, ... when `base` is taken.
pub fn unique_name(base: &str, existing: &[Project]) -> String {
    if !existing.iter().any(|p| p.name.eq_ignore_ascii_case(base)) {
        return base.to_string();
    }
    let mut i = 2;
    loop {
        let candidate = format!("{base}-{i}");
        if !existing
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(&candidate))
        {
            return candidate;
        }
        i += 1;
    }
}
/// Build and validate a project from user inputs, without touching disk state.
///
/// Services take their name from the program being run and can share a directory
/// with other services (unique names like `node-2` are assigned automatically).
/// Agents require an explicit path and default to the directory's name.
pub fn build_project(
    existing: &[Project],
    path_raw: &str,
    kind: Kind,
    parts: Vec<String>,
) -> Result<Project, String> {
    let trimmed = path_raw.trim().trim_matches('"').trim_matches('\'').trim();
    if trimmed.is_empty() && kind != Kind::Service {
        return Err("Path is empty.".into());
    }
    // An omitted service path means "right here".
    let anchor = if trimmed.is_empty() { "." } else { trimmed };
    let path = plain_path(fs::canonicalize(anchor).map_err(|e| format!("Bad path: {e}"))?);
    if !path.is_dir() {
        return Err("Project path must be a directory.".into());
    }
    let dir_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if dir_name.is_empty() {
        return Err("Cannot use filesystem root as project.".into());
    }
    let name = if kind == Kind::Service {
        let base = program_name(&parts).unwrap_or(dir_name);
        unique_name(&base, existing)
    } else {
        if existing
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(&dir_name))
        {
            return Err(format!("Project name already registered: {dir_name}"));
        }
        dir_name
    };
    let command = if parts.is_empty() { shell() } else { parts };
    Ok(Project {
        name,
        path,
        command,
        kind,
        autostart: false,
    })
}
/// The name a new project would get, guessed from the typed text alone (no
/// disk access, so it can run on every redraw). None while it is unknowable.
pub fn preview_name(
    existing: &[Project],
    path_raw: &str,
    kind: Kind,
    parts: &[String],
) -> Option<String> {
    let trimmed = path_raw.trim().trim_matches('"').trim_matches('\'').trim();
    let dir = Path::new(trimmed)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    match kind {
        Kind::Agent => dir,
        Kind::Service => program_name(parts)
            .or(dir)
            .map(|base| unique_name(&base, existing)),
    }
}
/// File name of a program without directory or `.exe`/`.cmd`/`.bat`/`.com`.
fn program_stem(prog: &str) -> String {
    let base = std::path::Path::new(prog)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| prog.to_string());
    let lower = base.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stem) = lower.strip_suffix(ext) {
            let stem = base[..stem.len()].to_string();
            if !stem.is_empty() {
                return stem;
            }
        }
    }
    base
}
/// The command a shell is told to run: what follows `/c`, `/k`, `-c` or
/// `-Command`. None when the shell is started bare.
fn shell_inner_command(args: &[String]) -> Option<Vec<String>> {
    let at = args.iter().position(|a| {
        matches!(
            a.to_ascii_lowercase().as_str(),
            "/c" | "/k" | "/r" | "-c" | "-command"
        )
    })?;
    let rest = &args[at + 1..];
    let inner = match rest {
        [] => return None,
        [one] => split_command(one),
        many => many.to_vec(),
    };
    (!inner.is_empty()).then_some(inner)
}
/// Program word of a command line for naming path-less services:
/// `ollama.exe` serves as `ollama`, `node` stays `node`. A shell is looked
/// through (`cmd /c jarvis` is `jarvis`); a bare shell has no program name.
pub fn program_name(parts: &[String]) -> Option<String> {
    let first = parts.first()?;
    if is_shell_program(first) {
        return program_name(&shell_inner_command(&parts[1..])?);
    }
    Some(program_stem(first))
}
/// Filtered view: display order narrowed to projects whose name or
/// command contains the query (case-insensitive). Empty query = all.
pub fn apply_filter(projects: &[Project], query: &str) -> Vec<usize> {
    let q = query.to_ascii_lowercase();
    display_order(projects)
        .into_iter()
        .filter(|&i| {
            q.is_empty()
                || projects[i].name.to_ascii_lowercase().contains(&q)
                || projects[i]
                    .command
                    .join(" ")
                    .to_ascii_lowercase()
                    .contains(&q)
        })
        .collect()
}
pub fn fit(s: &str, w: usize) -> String {
    s.chars().take(w).collect()
}
