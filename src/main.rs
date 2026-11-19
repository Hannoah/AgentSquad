pub mod app;
pub mod completion;
pub mod config;
pub mod envvault;
pub mod history;
pub mod ime;
pub mod input;
pub mod json;
pub mod ledger;
pub mod pathpick;
pub mod refs;
pub mod select;
pub mod session;
pub mod skills;
pub mod sys;
pub mod tools;
pub mod ui;
pub mod updater;

#[cfg(test)]
mod tests;

use anyhow::{bail, Context, Result};
use std::{
    env,
    io::{self, IsTerminal, Read},
};

use app::run;
use config::{
    build_project, check_superior, clean_path, create_missing_dir, display_order, load,
    persist_add, persist_remove, persist_under, shell, Kind,
};
use ui::{cli_help, empty_list_hint, list_table};
use updater::{install_update, is_newer, latest_tag_result};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if matches!(
        args.first().map(String::as_str),
        Some("--help" | "-h" | "help")
    ) {
        println!("{}", cli_help());
        return Ok(());
    }
    if matches!(
        args.first().map(String::as_str),
        Some("--version" | "-V" | "version")
    ) {
        println!("squad {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // Needs no config, so a broken config.toml must not block updating.
    if args.first().map(String::as_str) == Some("update") {
        if cfg!(debug_assertions) {
            bail!("Debug builds never self-update; install a release build first");
        }
        if !updater::SUPPORTED {
            bail!("Releases are built for Windows and macOS only; update from source here");
        }
        let check_only = args.get(1).map(String::as_str) == Some("--check");
        let current = env!("CARGO_PKG_VERSION");
        let tag = latest_tag_result()?;
        if !is_newer(current, &tag) {
            println!("squad v{current} is up to date (latest {tag})");
            return Ok(());
        }
        println!("squad {tag} is available (you have v{current})");
        if check_only {
            return Ok(());
        }
        println!("Downloading and verifying {tag}...");
        install_update(&tag)?;
        println!("Updated to {tag}. Restart squad to use it.");
        return Ok(());
    }
    // Claude Code's Stop hook (see ledger::stop_hook); needs no config either.
    if args.first().map(String::as_str) == Some("hook") {
        if args.get(1).map(String::as_str) != Some("stop") {
            bail!("Usage: squad hook stop (run by Claude Code, not by hand)");
        }
        let mut input = String::new();
        let _ = io::stdin().read_to_string(&mut input);
        if let Some(out) = env::current_dir()
            .ok()
            .and_then(|d| ledger::stop_hook(&input, &d))
        {
            println!("{out}");
        }
        return Ok(());
    }
    // Shell completion needs no config: `squad completion powershell|bash`.
    if args.first().map(String::as_str) == Some("completion") {
        let shell = args.get(1).map(String::as_str).unwrap_or("");
        match completion::script_for(shell) {
            Some(script) => {
                print!("{script}");
                return Ok(());
            }
            None => bail!("Usage: squad completion <powershell|bash>"),
        }
    }
    let c = load()?;
    match args.first().map(String::as_str) {
        None => run(c, None, Vec::new()),
        // Used by the update restart: start these sessions again, stay in the manager.
        Some("resume") => {
            let idx = args[1..]
                .iter()
                .filter_map(|n| {
                    c.projects
                        .iter()
                        .position(|p| p.name.eq_ignore_ascii_case(n))
                })
                .collect();
            run(c, None, idx)
        }
        Some("add") => {
            // The path may be omitted for services: `squad add --kind service -- cmd`.
            const USAGE: &str =
                "Usage: squad add [<path>] [--kind agent|service] [--autostart] [--restart-on-crash] [--ledger] [--create] [--under <commander>] [--env KEY[,KEY...]] [--skill NAME[,NAME...]] [--tool NAME[,NAME...]] [--ref NAME[,NAME...]] [-- command args...] (path optional for services)";
            let tail: &[String] = args.get(1..).unwrap_or(&[]);
            let (path_raw, tail) = match tail.first().map(String::as_str) {
                Some(flag) if flag.starts_with("--") => ("", tail),
                Some(_) => (tail[0].as_str(), &tail[1..]),
                None => ("", tail),
            };
            let mut kind = Kind::default();
            let mut autostart = false;
            let mut restart_on_crash = false;
            let mut ledger = false;
            let mut create = false;
            let mut under: Option<String> = None;
            let mut env_keys: Vec<String> = Vec::new();
            let mut skill_names: Vec<String> = Vec::new();
            let mut tool_names: Vec<String> = Vec::new();
            let mut ref_names: Vec<String> = Vec::new();
            let mut command: Option<Vec<String>> = None;
            let mut i = 0;
            while i < tail.len() {
                match tail[i].as_str() {
                    "--kind" => {
                        let v = tail.get(i + 1).context(USAGE)?;
                        kind = v.parse()?;
                        i += 2;
                    }
                    "--autostart" => {
                        autostart = true;
                        i += 1;
                    }
                    "--restart-on-crash" => {
                        restart_on_crash = true;
                        i += 1;
                    }
                    "--ledger" => {
                        ledger = true;
                        i += 1;
                    }
                    "--create" => {
                        create = true;
                        i += 1;
                    }
                    "--under" => {
                        under = Some(tail.get(i + 1).context(USAGE)?.clone());
                        i += 2;
                    }
                    "--env" => {
                        push_csv(&mut env_keys, tail.get(i + 1).context(USAGE)?);
                        i += 2;
                    }
                    "--skill" | "--skills" => {
                        push_csv(&mut skill_names, tail.get(i + 1).context(USAGE)?);
                        i += 2;
                    }
                    "--tool" | "--tools" => {
                        push_csv(&mut tool_names, tail.get(i + 1).context(USAGE)?);
                        i += 2;
                    }
                    "--ref" | "--refs" => {
                        push_csv(&mut ref_names, tail.get(i + 1).context(USAGE)?);
                        i += 2;
                    }
                    "--" => {
                        if i + 1 >= tail.len() {
                            bail!("Use -- before command: squad add [<path>] -- claude");
                        }
                        command = Some(tail[i + 1..].to_vec());
                        break;
                    }
                    other => {
                        bail!("Unknown option: {other}. Use --kind agent|service, --autostart, --restart-on-crash, --ledger, --create, --under <commander>, --env KEY, --skill NAME, --tool NAME, --ref NAME or -- command")
                    }
                }
            }
            if create {
                create_missing_dir(path_raw)?;
            } else if !clean_path(path_raw).is_empty()
                && !std::path::Path::new(clean_path(path_raw)).exists()
            {
                bail!(
                    "Folder {} does not exist. Add --create to make it.",
                    clean_path(path_raw)
                );
            }
            let vault = if env_keys.is_empty() {
                None
            } else {
                let v = envvault::load_env()?;
                for k in &env_keys {
                    if !v.vars.iter().any(|e| e.key == *k) {
                        bail!("Unknown shared env key: {k}. Register it with `squad env set {k} <value>`.");
                    }
                }
                Some(v)
            };
            let s_vault = if skill_names.is_empty() {
                None
            } else {
                let v = skills::load_skills()?;
                for name in &skill_names {
                    let norm = skills::validate_skill_name(name).map_err(anyhow::Error::msg)?;
                    if !v.skills.iter().any(|s| s.name == norm) {
                        bail!("Unknown shared skill: {name}. Register it with `squad skills add {name} ...`.");
                    }
                }
                Some(v)
            };
            let t_vault = if tool_names.is_empty() {
                None
            } else {
                let v = tools::load_tools()?;
                for name in &tool_names {
                    if !v.tools.iter().any(|t| t.name.eq_ignore_ascii_case(name)) {
                        bail!("Unknown shared tool: {name}. Register it with `squad tools add {name} ...`.");
                    }
                }
                Some(v)
            };
            let r_vault = if ref_names.is_empty() {
                None
            } else {
                let v = refs::load_refs()?;
                for name in &ref_names {
                    if !v
                        .references
                        .iter()
                        .any(|r| r.name.eq_ignore_ascii_case(name))
                    {
                        bail!("Unknown shared reference: {name}. Register it with `squad refs add {name} --url <URL>`.");
                    }
                }
                Some(v)
            };
            let mut p = build_project(&c.projects, path_raw, kind, command.unwrap_or_else(shell))
                .map_err(anyhow::Error::msg)?;
            p.autostart = autostart;
            p.restart_on_crash = restart_on_crash;
            p.ledger = ledger;
            if restart_on_crash && kind != Kind::Service {
                eprintln!("Note: --restart-on-crash only acts on services; it is stored but ignored for agents.");
            }
            if let Some(u) = under {
                p.reports_to =
                    Some(check_superior(&c.projects, kind, &u).map_err(anyhow::Error::msg)?);
            }
            // Re-reads the file so a running squad's own writes are not clobbered.
            persist_add(&p)?;
            ledger::ensure_agent(&p);
            if p.reports_to.is_some() {
                // Report in to the commander (it learns its head count).
                ledger::report_transfer(&load()?.projects, &p.name, None);
            }
            if let Some(v) = vault {
                let (added, updated) = envvault::apply_dotenv_keys(&p.path, &v, &env_keys)?;
                println!(
                    "Updated {}/.env (+{added} added, {updated} updated)",
                    p.name
                );
            }
            if let Some(v) = s_vault {
                let (added, updated) = skills::apply_skill_names(&p.path, &v, &skill_names)?;
                println!(
                    "Updated {} skills (+{added} added, {updated} updated)",
                    p.name
                );
            }
            if let Some(v) = t_vault {
                let (added, updated) = tools::apply_tool_names(&p.path, &v, &tool_names)?;
                println!(
                    "Updated {} tools (+{added} added, {updated} updated)",
                    p.name
                );
            }
            if let Some(v) = r_vault {
                let (added, updated) = refs::apply_ref_names(&p.path, &v, &ref_names)?;
                println!(
                    "Updated {} refs (+{added} added, {updated} updated)",
                    p.name
                );
            }
            match &p.reports_to {
                Some(c) => println!("Registered {} ({}) under {c}", p.name, kind.as_str()),
                None => println!("Registered {} ({})", p.name, kind.as_str()),
            }
            Ok(())
        }
        Some("under") => {
            let name = args
                .get(1)
                .context("Usage: squad under <name> [<commander>] (no commander clears it)")?;
            let under = args.get(2).map(String::as_str);
            let old = c
                .projects
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
                .and_then(|p| p.reports_to.clone());
            persist_under(name, under)?;
            ledger::report_transfer(&load()?.projects, name, old.as_deref());
            match under {
                Some(u) => println!("{name} now reports to {u}"),
                None => println!("{name} now reports to no one"),
            }
            Ok(())
        }
        Some("list") => {
            // Machine-readable names for completion scripts and shell loops.
            if args.get(1).map(String::as_str) == Some("--names") {
                for &i in &display_order(&c.projects) {
                    println!("{}", c.projects[i].name);
                }
                return Ok(());
            }
            if c.projects.is_empty() {
                eprintln!("{}", empty_list_hint());
                return Ok(());
            }
            // Colors only on a real terminal; pipes and NO_COLOR stay plain.
            let color = io::stdout().is_terminal() && env::var("NO_COLOR").is_err();
            for line in list_table(&c.projects, color) {
                println!("{line}");
            }
            Ok(())
        }
        Some("remove") => {
            let name = args.get(1).context("Usage: squad remove <name>")?;
            if !c.projects.iter().any(|p| p.name.eq_ignore_ascii_case(name)) {
                bail!("Unknown project: {name}");
            }
            persist_remove(name)?;
            println!("Unregistered {name}; project files unchanged");
            Ok(())
        }
        Some("start") => {
            let name = args.get(1).context("Usage: squad start <name>")?;
            let i = c
                .projects
                .iter()
                .position(|p| p.name.eq_ignore_ascii_case(name))
                .with_context(|| format!("Project not registered: {name}"))?;
            run(c, Some(i), Vec::new())
        }
        Some("env") => run_env_cli(&c, args.get(1..).unwrap_or(&[])),
        Some("skills") => run_skills_cli(&c, args.get(1..).unwrap_or(&[])),
        Some("tools") => run_tools_cli(&c, args.get(1..).unwrap_or(&[])),
        Some("refs" | "references") => run_refs_cli(&c, args.get(1..).unwrap_or(&[])),
        Some(other) => bail!("Unknown command: {other}. Use squad --help"),
    }
}

fn push_csv(dst: &mut Vec<String>, raw: &str) {
    for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        dst.push(item.to_string());
    }
}

fn find_project<'a>(c: &'a config::Config, name: &str) -> Result<&'a config::Project> {
    c.projects
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .with_context(|| format!("Unknown project: {name}"))
}

fn run_env_cli(c: &config::Config, args: &[String]) -> Result<()> {
    const USAGE: &str =
        "Usage: squad env <list | set KEY VALUE | remove KEY | apply <project> [KEY...] | unapply <project> KEY...>";
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let vault = envvault::load_env()?;
            if vault.vars.is_empty() {
                println!("No shared environment variables registered.");
                println!("Add one with: squad env set KEY VALUE");
                return Ok(());
            }
            for e in &vault.vars {
                println!("{}={}", e.key, envvault::mask_value(&e.value));
            }
            Ok(())
        }
        Some("set") => {
            let key = args.get(1).context(USAGE)?;
            let value = args.get(2).context(USAGE)?;
            let mut vault = envvault::load_env()?;
            let inserted =
                envvault::upsert_env(&mut vault, key, value).map_err(anyhow::Error::msg)?;
            envvault::save_env(&vault)?;
            let action = if inserted { "Registered" } else { "Updated" };
            println!("{action} shared env var {}", key.trim());
            Ok(())
        }
        Some("remove" | "rm") => {
            let key = args.get(1).context(USAGE)?;
            let mut vault = envvault::load_env()?;
            if !envvault::remove_env(&mut vault, key.trim()) {
                bail!("Unknown shared env key: {key}");
            }
            envvault::save_env(&vault)?;
            println!("Removed shared env var {}", key.trim());
            Ok(())
        }
        Some("apply") => {
            let name = args.get(1).context(USAGE)?;
            let p = find_project(c, name)?;
            let vault = envvault::load_env()?;
            let keys: Vec<String> = if args.len() > 2 {
                args[2..].to_vec()
            } else {
                vault.vars.iter().map(|v| v.key.clone()).collect()
            };
            if keys.is_empty() {
                bail!("No shared env vars to apply. Register one with `squad env set KEY VALUE`.");
            }
            let (added, updated) = envvault::apply_dotenv_keys(&p.path, &vault, &keys)?;
            println!(
                "Updated {}/.env (+{added} added, {updated} updated)",
                p.name
            );
            Ok(())
        }
        Some("unapply") => {
            let name = args.get(1).context(USAGE)?;
            if args.len() < 3 {
                bail!("{USAGE}");
            }
            let p = find_project(c, name)?;
            let removed = envvault::remove_dotenv(&p.path, &args[2..])?;
            println!("Updated {}/.env (-{removed} removed)", p.name);
            Ok(())
        }
        Some(_) => bail!("{USAGE}"),
    }
}

fn run_skills_cli(c: &config::Config, args: &[String]) -> Result<()> {
    const USAGE: &str = "Usage: squad skills <list | add <name> [--path <dir|file> | --prompt <text>] [--desc <text>] | remove <name> | apply <project> [NAME...] | unapply <project> NAME...>";
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let vault = skills::load_skills()?;
            if vault.skills.is_empty() {
                println!("No shared skills registered.");
                println!("Add one with: squad skills add <name> --path <dir|SKILL.md>");
                return Ok(());
            }
            for s in &vault.skills {
                let src = s
                    .source_path
                    .as_deref()
                    .map(|p| format!("path:{p}"))
                    .unwrap_or_else(|| "inline".to_string());
                if s.description.is_empty() {
                    println!("{}  [{src}]", s.name);
                } else {
                    println!("{}  [{src}]  {}", s.name, s.description);
                }
            }
            Ok(())
        }
        Some("add" | "set") => {
            let name = args.get(1).context(USAGE)?;
            let tail = args.get(2..).unwrap_or(&[]);
            let mut path_arg: Option<String> = None;
            let mut prompt_arg: Option<String> = None;
            let mut desc = String::new();
            let mut i = 0;
            while i < tail.len() {
                match tail[i].as_str() {
                    "--path" => {
                        path_arg = Some(tail.get(i + 1).context(USAGE)?.clone());
                        i += 2;
                    }
                    "--prompt" | "--instructions" => {
                        prompt_arg = Some(tail.get(i + 1).context(USAGE)?.clone());
                        i += 2;
                    }
                    "--desc" | "--description" => {
                        desc = tail.get(i + 1).context(USAGE)?.clone();
                        i += 2;
                    }
                    other => bail!("Unknown option: {other}. {USAGE}"),
                }
            }
            let (source_path, instructions) = match (path_arg, prompt_arg) {
                (Some(_), Some(_)) => bail!("Specify either --path or --prompt, not both."),
                (Some(p), None) => {
                    let (src, body) =
                        skills::classify_source_or_body(&p).map_err(anyhow::Error::msg)?;
                    if src.is_none() {
                        bail!("Source path does not exist: {p}");
                    }
                    (src, body)
                }
                (None, Some(text)) => {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        bail!("Prompt cannot be empty.");
                    }
                    (None, trimmed.to_string())
                }
                (None, None) => bail!("{USAGE}"),
            };
            let mut vault = skills::load_skills()?;
            let entry = skills::SkillEntry {
                name: name.to_string(),
                description: desc,
                source_path,
                instructions,
            };
            let norm = skills::validate_skill_name(&entry.name).map_err(anyhow::Error::msg)?;
            let inserted = skills::upsert_skill(&mut vault, entry).map_err(anyhow::Error::msg)?;
            skills::save_skills(&vault)?;
            let action = if inserted { "Registered" } else { "Updated" };
            println!("{action} shared skill {norm}");
            Ok(())
        }
        Some("remove" | "rm") => {
            let name = args.get(1).context(USAGE)?;
            let mut vault = skills::load_skills()?;
            if !skills::remove_skill(&mut vault, name) {
                bail!("Unknown shared skill: {name}");
            }
            skills::save_skills(&vault)?;
            println!("Removed shared skill {}", name.trim().to_ascii_lowercase());
            Ok(())
        }
        Some("apply") => {
            let proj_name = args.get(1).context(USAGE)?;
            let p = find_project(c, proj_name)?;
            let vault = skills::load_skills()?;
            let names: Vec<String> = if args.len() > 2 {
                args[2..].to_vec()
            } else {
                vault.skills.iter().map(|s| s.name.clone()).collect()
            };
            if names.is_empty() {
                bail!(
                    "No shared skills to apply. Register one with `squad skills add <name> ...`."
                );
            }
            let (added, updated) = skills::apply_skill_names(&p.path, &vault, &names)?;
            println!(
                "Updated {} skills (+{added} added, {updated} updated)",
                p.name
            );
            Ok(())
        }
        Some("unapply") => {
            let proj_name = args.get(1).context(USAGE)?;
            if args.len() < 3 {
                bail!("{USAGE}");
            }
            let p = find_project(c, proj_name)?;
            let removed = skills::remove_skills(&p.path, &args[2..])?;
            println!("Updated {} skills (-{removed} removed)", p.name);
            Ok(())
        }
        Some(_) => bail!("{USAGE}"),
    }
}

fn run_tools_cli(c: &config::Config, args: &[String]) -> Result<()> {
    const USAGE: &str = "Usage: squad tools <list | add <name> [--kind mcp|cli] [--desc <text>] [--env KEY=VAL]... [--] <command|url...> | remove <name> | apply <project> [NAME...] | unapply <project> NAME...>";
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let vault = tools::load_tools()?;
            if vault.tools.is_empty() {
                println!("No shared tools registered.");
                println!("Add one with: squad tools add <name> -- npx -y <mcp-package>");
                return Ok(());
            }
            for t in &vault.tools {
                if t.description.is_empty() {
                    println!("[{}] {}  {}", t.kind.as_str(), t.name, t.command);
                } else {
                    println!(
                        "[{}] {}  {}  ({})",
                        t.kind.as_str(),
                        t.name,
                        t.command,
                        t.description
                    );
                }
            }
            Ok(())
        }
        Some("add" | "set") => {
            let name = args.get(1).context(USAGE)?;
            let tail = args.get(2..).unwrap_or(&[]);
            let mut kind = tools::ToolKind::Mcp;
            let mut desc = String::new();
            let mut env_map = std::collections::BTreeMap::new();
            let mut cmd_parts: Vec<String> = Vec::new();
            let mut i = 0;
            while i < tail.len() {
                match tail[i].as_str() {
                    "--kind" => {
                        kind = tail.get(i + 1).context(USAGE)?.parse()?;
                        i += 2;
                    }
                    "--desc" | "--description" => {
                        desc = tail.get(i + 1).context(USAGE)?.clone();
                        i += 2;
                    }
                    "--url" => {
                        cmd_parts.push(tail.get(i + 1).context(USAGE)?.clone());
                        i += 2;
                    }
                    "--env" | "-e" => {
                        let pair = tail.get(i + 1).context(USAGE)?;
                        let (k, v) = pair.split_once('=').with_context(|| {
                            format!("Expected KEY=VALUE for --env, got: {pair}")
                        })?;
                        let valid_k = envvault::validate_env_key(k).map_err(anyhow::Error::msg)?;
                        env_map.insert(valid_k, v.to_string());
                        i += 2;
                    }
                    "--" => {
                        cmd_parts.extend(tail[i + 1..].iter().cloned());
                        break;
                    }
                    other if other.starts_with("--") => bail!("Unknown option: {other}. {USAGE}"),
                    _ => {
                        cmd_parts.extend(tail[i..].iter().cloned());
                        break;
                    }
                }
            }
            let command = config::join_command(&cmd_parts);
            let mut vault = tools::load_tools()?;
            let entry = tools::ToolEntry {
                name: name.to_string(),
                kind,
                command,
                description: desc,
                env: env_map,
            };
            let valid_name = tools::validate_tool_name(&entry.name).map_err(anyhow::Error::msg)?;
            let inserted = tools::upsert_tool(&mut vault, entry).map_err(anyhow::Error::msg)?;
            tools::save_tools(&vault)?;
            let action = if inserted { "Registered" } else { "Updated" };
            println!("{action} shared {} tool {valid_name}", kind.as_str());
            Ok(())
        }
        Some("remove" | "rm") => {
            let name = args.get(1).context(USAGE)?;
            let mut vault = tools::load_tools()?;
            if !tools::remove_tool(&mut vault, name) {
                bail!("Unknown shared tool: {name}");
            }
            tools::save_tools(&vault)?;
            println!("Removed shared tool {}", name.trim());
            Ok(())
        }
        Some("apply") => {
            let proj_name = args.get(1).context(USAGE)?;
            let p = find_project(c, proj_name)?;
            let vault = tools::load_tools()?;
            let names: Vec<String> = if args.len() > 2 {
                args[2..].to_vec()
            } else {
                vault.tools.iter().map(|t| t.name.clone()).collect()
            };
            if names.is_empty() {
                bail!("No shared tools to apply. Register one with `squad tools add <name> ...`.");
            }
            let (added, updated) = tools::apply_tool_names(&p.path, &vault, &names)?;
            println!(
                "Updated {} tools (+{added} added, {updated} updated)",
                p.name
            );
            Ok(())
        }
        Some("unapply") => {
            let proj_name = args.get(1).context(USAGE)?;
            if args.len() < 3 {
                bail!("{USAGE}");
            }
            let p = find_project(c, proj_name)?;
            let removed = tools::remove_tools(&p.path, &args[2..])?;
            println!("Updated {} tools (-{removed} removed)", p.name);
            Ok(())
        }
        Some(_) => bail!("{USAGE}"),
    }
}

fn run_refs_cli(c: &config::Config, args: &[String]) -> Result<()> {
    const USAGE: &str = "Usage: squad refs <list | add [<name>] --url <URL> [--desc <text>] | remove <name> | apply <project> [NAME...] | unapply <project> NAME...>";
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let vault = refs::load_refs()?;
            if vault.references.is_empty() {
                println!("No shared references registered.");
                println!("Add one with: squad refs add <name> --url https://github.com/<owner>/<repo> --desc \"...\"");
                return Ok(());
            }
            for r in &vault.references {
                if r.description.is_empty() {
                    println!("{}  {}", r.name, r.url);
                } else {
                    println!("{}  {}  ({})", r.name, r.url, r.description);
                }
            }
            Ok(())
        }
        Some("add" | "set") => {
            let tail = args.get(1..).unwrap_or(&[]);
            let mut name_arg: Option<String> = None;
            let mut url: Option<String> = None;
            let mut desc = String::new();
            let mut i = 0;
            while i < tail.len() {
                match tail[i].as_str() {
                    "--url" | "-u" => {
                        url = Some(tail.get(i + 1).context(USAGE)?.clone());
                        i += 2;
                    }
                    "--desc" | "--description" | "-d" => {
                        desc = tail.get(i + 1).context(USAGE)?.clone();
                        i += 2;
                    }
                    "--name" | "-n" => {
                        name_arg = Some(tail.get(i + 1).context(USAGE)?.clone());
                        i += 2;
                    }
                    other if other.starts_with("--") => bail!("Unknown option: {other}. {USAGE}"),
                    pos => {
                        let looks_like_url = pos.starts_with("http://")
                            || pos.starts_with("https://")
                            || pos.to_ascii_lowercase().starts_with("github.com/");
                        if url.is_none() && looks_like_url {
                            url = Some(pos.to_string());
                        } else if name_arg.is_none() {
                            name_arg = Some(pos.to_string());
                        } else if url.is_none() {
                            url = Some(pos.to_string());
                        } else {
                            bail!("{USAGE}");
                        }
                        i += 1;
                    }
                }
            }
            let raw_url = url.context(USAGE)?;
            let saved_url = refs::normalize_url(&raw_url).map_err(anyhow::Error::msg)?;
            let raw_name = match name_arg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(n) => n.to_string(),
                None => refs::infer_name_from_url(&saved_url),
            };
            let saved_name = refs::validate_ref_name(&raw_name).map_err(anyhow::Error::msg)?;
            let mut vault = refs::load_refs()?;
            let inserted = refs::upsert_ref(
                &mut vault,
                refs::ReferenceEntry {
                    name: saved_name.clone(),
                    url: saved_url.clone(),
                    description: desc,
                },
            )
            .map_err(anyhow::Error::msg)?;
            refs::save_refs(&vault)?;
            let action = if inserted { "Registered" } else { "Updated" };
            println!("{action} shared reference {saved_name} -> {saved_url}");
            Ok(())
        }
        Some("remove" | "rm") => {
            let name = args.get(1).context(USAGE)?;
            let mut vault = refs::load_refs()?;
            if !refs::remove_ref(&mut vault, name) {
                bail!("Unknown shared reference: {name}");
            }
            refs::save_refs(&vault)?;
            println!("Removed shared reference {}", name.trim());
            Ok(())
        }
        Some("apply") => {
            let proj_name = args.get(1).context(USAGE)?;
            let p = find_project(c, proj_name)?;
            let vault = refs::load_refs()?;
            let names: Vec<String> = if args.len() > 2 {
                args[2..].to_vec()
            } else {
                vault.references.iter().map(|r| r.name.clone()).collect()
            };
            if names.is_empty() {
                bail!(
                    "No shared references to apply. Register one with `squad refs add <name> --url <URL>`."
                );
            }
            let (added, updated) = refs::apply_ref_names(&p.path, &vault, &names)?;
            println!(
                "Updated {} refs (+{added} added, {updated} updated)",
                p.name
            );
            Ok(())
        }
        Some("unapply") => {
            let proj_name = args.get(1).context(USAGE)?;
            if args.len() < 3 {
                bail!("{USAGE}");
            }
            let p = find_project(c, proj_name)?;
            let vault = refs::load_refs()?;
            let removed = refs::remove_refs(&p.path, &vault, &args[2..])?;
            println!("Updated {} refs (-{removed} removed)", p.name);
            Ok(())
        }
        Some(_) => bail!("{USAGE}"),
    }
}
