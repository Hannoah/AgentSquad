use crate::session::Session;
use anyhow::{bail, Context, Result};
use portable_pty::PtySize;
use serde::{Deserialize, Serialize};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
    /// Keep a `.squad/` ledger in the project (created on first start).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ledger: bool,
    /// Name of the commander this agent reports to. Anyone with
    /// subordinates is a commander; commanders may report to commanders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_to: Option<String>,
    /// Listed first in its group (`f` in the manager).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}
impl Project {
    /// Agents always keep an operations log (their work history lives in it);
    /// `ledger = true` only matters for a service.
    pub fn has_ledger(&self) -> bool {
        self.ledger || self.kind == Kind::Agent
    }
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
    /// Ring the terminal bell when a background agent starts waiting for
    /// input (default true; `bell = false` silences it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bell: Option<bool>,
    /// A session whose process tree holds more RAM than this (MiB) is
    /// flagged and reported once (default 4096; 0 turns the check off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_limit_mb: Option<u64>,
}
impl Config {
    pub fn mem_limit_bytes(&self) -> u64 {
        self.mem_limit_mb.unwrap_or(4096) << 20
    }
}
impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            projects: Vec::new(),
            scrollback: None,
            bell: None,
            mem_limit_mb: None,
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
/// File next to the config that remembers the project selected at exit.
fn last_project_path() -> Result<PathBuf> {
    Ok(config_path()?.with_file_name("last-project"))
}
/// Name of the project that was selected when squad last closed.
pub fn load_last_project() -> Option<String> {
    let text = fs::read_to_string(last_project_path().ok()?).ok()?;
    Some(text.trim().to_string()).filter(|s| !s.is_empty())
}
/// Best effort: forgetting the selection is not worth an error.
pub fn save_last_project(name: &str) {
    if let Ok(p) = last_project_path() {
        let _ = fs::write(p, name);
    }
}
/// Row to select at startup: an explicit `squad start` target wins, then
/// the project selected last time, then the first row.
pub fn start_selection(
    projects: &[Project],
    order: &[usize],
    initial: Option<usize>,
    last: Option<&str>,
) -> usize {
    let wanted = initial.or_else(|| {
        last.and_then(|n| projects.iter().position(|p| p.name.eq_ignore_ascii_case(n)))
    });
    wanted
        .and_then(|i| order.iter().position(|&x| x == i))
        .unwrap_or(0)
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
    let text = toml::to_string_pretty(c)?;
    // Synced before the rename: a power cut must not leave an empty config.
    let written = fs::File::create(&tmp)
        .and_then(|mut f| {
            io::Write::write_all(&mut f, text.as_bytes())?;
            f.sync_all()
        })
        .and_then(|()| fs::rename(&tmp, p));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("Cannot write {}", p.display()));
    }
    Ok(())
}
pub fn save(c: &Config) -> Result<()> {
    let p = config_path()?;
    save_to(c, &p)
}
/// Reporting needs a shared log: turn the operations log on for agent `i`
/// and the commander it reports to. Never turns anything off.
pub fn enable_chain_ledger(projects: &mut [Project], i: usize) {
    if let Some(c) = superior(projects, i) {
        projects[i].ledger = true;
        projects[c].ledger = true;
    }
}
/// Register `p` on disk, re-reading the file first so projects added or
/// removed by another process are not clobbered.
// ponytail: read-modify-write without a lock; two writers inside the same few
// milliseconds still race (last rename wins). Add a lock file if that ever bites.
pub fn persist_add(p: &Project) -> Result<()> {
    let mut disk = load()?;
    if !disk
        .projects
        .iter()
        .any(|q| q.name.eq_ignore_ascii_case(&p.name))
    {
        disk.projects.push(p.clone());
        let last = disk.projects.len() - 1;
        enable_chain_ledger(&mut disk.projects, last);
    }
    save(&disk)
}
/// Unregister `name` on disk, re-reading the file first (see `persist_add`).
pub fn persist_remove(name: &str) -> Result<()> {
    let mut disk = load()?;
    disk.projects.retain(|q| !q.name.eq_ignore_ascii_case(name));
    save(&disk)
}
/// Change (or with `None`, clear) whom `name` reports to on disk, re-reading
/// the file first (see `persist_add`) and validating against what is there.
pub fn persist_under(name: &str, under: Option<&str>) -> Result<()> {
    let mut disk = load()?;
    let stored = match under {
        Some(u) => Some(check_reassign(&disk.projects, name, u).map_err(anyhow::Error::msg)?),
        None => None,
    };
    let i = disk
        .projects
        .iter()
        .position(|q| q.name.eq_ignore_ascii_case(name))
        .with_context(|| format!("Unknown project: {name}"))?;
    disk.projects[i].reports_to = stored;
    enable_chain_ledger(&mut disk.projects, i);
    save(&disk)
}
/// Pin or unpin `name` on disk, re-reading the file first (see `persist_add`).
pub fn persist_pin(name: &str, pinned: bool) -> Result<()> {
    let mut disk = load()?;
    let p = disk
        .projects
        .iter_mut()
        .find(|q| q.name.eq_ignore_ascii_case(name))
        .with_context(|| format!("Unknown project: {name}"))?;
    p.pinned = pinned;
    save(&disk)
}
/// Change the command and autostart of `name` on disk, re-reading the file
/// first (see `persist_add`). An empty command means the default shell.
pub fn persist_edit(name: &str, command: Vec<String>, autostart: bool) -> Result<()> {
    let mut disk = load()?;
    let p = disk
        .projects
        .iter_mut()
        .find(|q| q.name.eq_ignore_ascii_case(name))
        .with_context(|| format!("Unknown project: {name}"))?;
    p.command = if command.is_empty() { shell() } else { command };
    p.autostart = autostart;
    save(&disk)
}
/// Move `name` to folder `path` on disk, re-reading the file first (see
/// `persist_add`). Returns the name it has afterwards (see `relocate`).
pub fn persist_move(name: &str, path: &Path) -> Result<String> {
    let mut disk = load()?;
    let i = disk
        .projects
        .iter()
        .position(|q| q.name.eq_ignore_ascii_case(name))
        .with_context(|| format!("Unknown project: {name}"))?;
    let new = relocate(&mut disk.projects, i, path.to_path_buf());
    save(&disk)?;
    Ok(new)
}
/// Inverse of `split_command`: quote the words that hold spaces (or are
/// empty) so the line splits back into the same words.
pub fn join_command(parts: &[String]) -> String {
    parts
        .iter()
        .map(|w| {
            if w.is_empty() || w.contains(char::is_whitespace) {
                format!("\"{w}\"")
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
/// `powershell.exe`), the user's `$SHELL` elsewhere. The resolved command is stored in the config, so it
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
        // $SHELL is the user's login shell (zsh on macOS).
        vec![env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "sh".into())]
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
/// The commander project `i` reports to, if it names a registered agent.
pub fn superior(projects: &[Project], i: usize) -> Option<usize> {
    let name = projects[i].reports_to.as_deref()?;
    projects
        .iter()
        .enumerate()
        .position(|(j, p)| j != i && p.kind == Kind::Agent && p.name.eq_ignore_ascii_case(name))
}
/// Agents that report directly to project `i`, in registration order.
pub fn subordinates(projects: &[Project], i: usize) -> Vec<usize> {
    (0..projects.len())
        .filter(|&j| superior(projects, j) == Some(i))
        .collect()
}
pub fn same_dir(a: &Project, b: &Project) -> bool {
    a.path
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.path.to_string_lossy())
}
/// Commanders: agents with agents under them. An agent becomes one the
/// moment another agent attaches to it.
pub fn is_commander(projects: &[Project], i: usize) -> bool {
    !subordinates(projects, i).is_empty()
}
/// Every agent below project `i`, at any depth (subordinates of
/// subordinates included), without repeats even if a hand-edited loop exists.
pub fn all_subordinates(projects: &[Project], i: usize) -> Vec<usize> {
    let mut found = vec![i];
    let mut next = 0;
    while next < found.len() {
        for j in subordinates(projects, found[next]) {
            if !found.contains(&j) {
                found.push(j);
            }
        }
        next += 1;
    }
    found.remove(0);
    found
}
/// Sidebar prefix drawing the chain of command as a tree: a star for
/// commanders and, below the top, branch lines (`├─`, `└─`) with a vertical
/// line under every ancestor that has later siblings. Unit names (squad,
/// platoon, company) can be derived from the subtree size here later.
pub fn rank_prefix(projects: &[Project], i: usize) -> String {
    // Chain from the top commander down to `i`; the length bound and the
    // repeat check keep a hand-edited loop from hanging the UI.
    let mut chain = vec![i];
    while let Some(up) = superior(projects, chain[chain.len() - 1]) {
        if chain.len() > projects.len() || chain.contains(&up) {
            break;
        }
        chain.push(up);
    }
    chain.reverse();
    let mut out = String::new();
    for w in chain.windows(2) {
        let last = subordinates(projects, w[0]).last() == Some(&w[1]);
        let own = w[1] == i;
        out.push_str(match (own, last) {
            (true, true) => "└─",
            (true, false) => "├─",
            (false, true) => "  ",
            (false, false) => "│ ",
        });
    }
    match (chain.len() > 1, is_commander(projects, i)) {
        (_, true) => out.push_str("★ "),
        (true, false) => out.push_str("─ "),
        (false, false) => {}
    }
    out
}
/// Move agent `name` under commander `under`, checked against `projects`:
/// both must be agents, and the chain of command may not loop back to
/// `name`. Returns the commander's stored name.
pub fn check_reassign(projects: &[Project], name: &str, under: &str) -> Result<String, String> {
    let i = projects
        .iter()
        .position(|p| p.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("Unknown project: {name}"))?;
    let stored = check_superior(projects, projects[i].kind, under)?;
    let mut at = projects.iter().position(|p| p.name == stored).unwrap_or(i);
    for _ in 0..=projects.len() {
        if at == i {
            return Err(format!(
                "{name} cannot report to itself or its own subordinate."
            ));
        }
        match superior(projects, at) {
            Some(up) => at = up,
            None => break,
        }
    }
    Ok(stored)
}
/// Validate `--under`: the new project must be an agent and the named
/// commander must be a registered agent. Returns the commander's stored name.
pub fn check_superior(existing: &[Project], kind: Kind, under: &str) -> Result<String, String> {
    if kind != Kind::Agent {
        return Err("Only agents can report to a commander.".into());
    }
    existing
        .iter()
        .find(|p| p.kind == Kind::Agent && p.name.eq_ignore_ascii_case(under))
        .map(|p| p.name.clone())
        .ok_or_else(|| format!("No agent named {under} to report to."))
}
/// Sidebar order: agents first (each commander followed by its
/// subordinates), then all services. Positions in this vector are what the
/// manager selection index refers to.
pub fn display_order(projects: &[Project]) -> Vec<usize> {
    fn push_tree(projects: &[Project], i: usize, order: &mut Vec<usize>) {
        if order.contains(&i) {
            return;
        }
        order.push(i);
        for j in subordinates(projects, i) {
            push_tree(projects, j, order);
        }
    }
    let mut order = Vec::with_capacity(projects.len());
    let agents = || (0..projects.len()).filter(|&i| projects[i].kind == Kind::Agent);
    // Pinned commanders (and services) come first; the sort is stable, so
    // everything else keeps its registration order.
    let mut roots: Vec<usize> = agents()
        .filter(|&i| superior(projects, i).is_none())
        .collect();
    roots.sort_by_key(|&i| !projects[i].pinned);
    for i in roots {
        push_tree(projects, i, &mut order);
    }
    // ponytail: a hand-edited reporting loop is listed flat rather than lost.
    for i in agents() {
        if !order.contains(&i) {
            order.push(i);
        }
    }
    let mut services: Vec<usize> = (0..projects.len())
        .filter(|&i| projects[i].kind == Kind::Service)
        .collect();
    services.sort_by_key(|&i| !projects[i].pinned);
    order.extend(services);
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
/// Typed or pasted path text without surrounding spaces and quotes.
pub fn clean_path(raw: &str) -> &str {
    raw.trim().trim_matches('"').trim_matches('\'').trim()
}
/// Create the folder named by `path_raw` (parents too) when it does not
/// exist yet. An existing folder or file is left alone; a file then fails
/// later in `build_project`. Blank input does nothing.
pub fn create_missing_dir(path_raw: &str) -> Result<()> {
    let p = clean_path(path_raw);
    if p.is_empty() || Path::new(p).exists() {
        return Ok(());
    }
    fs::create_dir_all(p).with_context(|| format!("Cannot create folder {p}"))
}
/// Canonical form of a project folder and its name, or why it cannot be one.
pub fn resolve_dir(anchor: &str) -> Result<(PathBuf, String), String> {
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
    Ok((path, dir_name))
}
/// Point project `i` at `path`. An agent is named after its folder, so it
/// takes the new folder's name (made unique among the others) and everyone
/// who reported to the old name follows it; a service is named after its
/// program and keeps its name. Returns the project's name afterwards.
pub fn relocate(projects: &mut [Project], i: usize, path: PathBuf) -> String {
    let old = projects[i].name.clone();
    if projects[i].kind == Kind::Agent {
        let dir = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !dir.is_empty() {
            let others: Vec<Project> = projects
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, p)| p.clone())
                .collect();
            let name = unique_name(&dir, &others);
            if name != old {
                for p in projects.iter_mut() {
                    if p.reports_to
                        .as_deref()
                        .is_some_and(|c| c.eq_ignore_ascii_case(&old))
                    {
                        p.reports_to = Some(name.clone());
                    }
                }
                projects[i].name = name;
            }
        }
    }
    projects[i].path = path;
    projects[i].name.clone()
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
    let trimmed = clean_path(path_raw);
    if trimmed.is_empty() && kind != Kind::Service {
        return Err("Path is empty.".into());
    }
    // An omitted service path means "right here".
    let anchor = if trimmed.is_empty() { "." } else { trimmed };
    let (path, dir_name) = resolve_dir(anchor)?;
    // Agents may share a directory too: the second becomes `dir-2`, like
    // services, and stays independent until given a commander (`--under`,
    // `u`, or right-click > Set superior).
    let base = if kind == Kind::Service {
        program_name(&parts).unwrap_or(dir_name)
    } else {
        dir_name
    };
    let name = unique_name(&base, existing);
    let command = if parts.is_empty() { shell() } else { parts };
    Ok(Project {
        name,
        path,
        command,
        kind,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
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
    let trimmed = clean_path(path_raw);
    let dir = Path::new(trimmed)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let base = match kind {
        Kind::Agent => dir,
        Kind::Service => program_name(parts).or(dir),
    };
    base.map(|b| unique_name(&b, existing))
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
/// Filtered view: narrowed to projects whose name, path, or command
/// contains the query (case-insensitive). Empty query = all.
pub fn apply_filter(projects: &[Project], query: &str) -> Vec<usize> {
    let q = query.to_ascii_lowercase();
    display_order(projects)
        .into_iter()
        .filter(|&i| {
            q.is_empty()
                || projects[i].name.to_ascii_lowercase().contains(&q)
                || projects[i]
                    .path
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains(&q)
                || projects[i]
                    .command
                    .join(" ")
                    .to_ascii_lowercase()
                    .contains(&q)
        })
        .collect()
}
/// Terminal columns `s` takes: Hangul and other wide characters take two.
pub fn width(s: &str) -> usize {
    s.width()
}
/// The leading run of `chars` that fits in `w` columns.
pub fn take_cols(chars: impl Iterator<Item = char>, w: usize) -> String {
    let mut used = 0;
    chars
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= w
        })
        .collect()
}
/// `s` cut to `w` columns, ending in `…` when something was cut.
pub fn fit(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    if w <= 1 {
        return take_cols(s.chars(), w);
    }
    // Keep a visible marker so a clipped name/command reads as clipped.
    take_cols(s.chars(), w - 1) + "…"
}
