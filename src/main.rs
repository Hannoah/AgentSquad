pub mod app;
pub mod config;
pub mod envvault;
pub mod history;
pub mod ime;
pub mod input;
pub mod ledger;
pub mod pathpick;
pub mod select;
pub mod session;
pub mod sys;
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
    build_project, check_superior, clean_path, create_missing_dir, load, persist_add,
    persist_remove, persist_under, shell, Kind,
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
                "Usage: squad add [<path>] [--kind agent|service] [--autostart] [--ledger] [--create] [--under <commander>] [--env KEY[,KEY...]] [-- command args...] (path optional for services)";
            let tail: &[String] = args.get(1..).unwrap_or(&[]);
            let (path_raw, tail) = match tail.first().map(String::as_str) {
                Some(flag) if flag.starts_with("--") => ("", tail),
                Some(_) => (tail[0].as_str(), &tail[1..]),
                None => ("", tail),
            };
            let mut kind = Kind::default();
            let mut autostart = false;
            let mut ledger = false;
            let mut create = false;
            let mut under: Option<String> = None;
            let mut env_keys: Vec<String> = Vec::new();
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
                        let raw = tail.get(i + 1).context(USAGE)?;
                        for k in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                            env_keys.push(k.to_string());
                        }
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
                        bail!("Unknown option: {other}. Use --kind agent|service, --autostart, --ledger, --create, --under <commander>, --env KEY or -- command")
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
            let mut p = build_project(&c.projects, path_raw, kind, command.unwrap_or_else(shell))
                .map_err(anyhow::Error::msg)?;
            p.autostart = autostart;
            p.ledger = ledger;
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
        Some(other) => bail!("Unknown command: {other}. Use squad --help"),
    }
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
            let p = c
                .projects
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
                .with_context(|| format!("Unknown project: {name}"))?;
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
            let p = c
                .projects
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
                .with_context(|| format!("Unknown project: {name}"))?;
            let removed = envvault::remove_dotenv(&p.path, &args[2..])?;
            println!("Updated {}/.env (-{removed} removed)", p.name);
            Ok(())
        }
        Some(_) => bail!("{USAGE}"),
    }
}
