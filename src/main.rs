pub mod app;
pub mod config;
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
    io::{self, IsTerminal},
};

use app::run;
use config::{
    build_project, check_superior, load, persist_add, persist_remove, persist_under, shell, Kind,
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
    if args.first().map(String::as_str) == Some("--version") {
        println!("squad {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // Needs no config, so a broken config.toml must not block updating.
    if args.first().map(String::as_str) == Some("update") {
        if cfg!(debug_assertions) {
            bail!("Debug builds never self-update; install a release build first");
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
    let c = load()?;
    match args.first().map(String::as_str) {
        None => run(c, None),
        Some("add") => {
            // The path may be omitted for services: `squad add --kind service -- cmd`.
            const USAGE: &str =
                "Usage: squad add [<path>] [--kind agent|service] [--autostart] [--ledger] [--under <commander>] [-- command args...] (path optional for services)";
            let tail: &[String] = args.get(1..).unwrap_or(&[]);
            let (path_raw, tail) = match tail.first().map(String::as_str) {
                Some(flag) if flag.starts_with("--") => ("", tail),
                Some(_) => (tail[0].as_str(), &tail[1..]),
                None => ("", tail),
            };
            let mut kind = Kind::default();
            let mut autostart = false;
            let mut ledger = false;
            let mut under: Option<String> = None;
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
                    "--under" => {
                        under = Some(tail.get(i + 1).context(USAGE)?.clone());
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
                        bail!("Unknown option: {other}. Use --kind agent|service, --autostart, --ledger, --under <commander> or -- command")
                    }
                }
            }
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
            if p.reports_to.is_some() {
                // Report in to the commander (it learns its head count).
                ledger::report_transfer(&load()?.projects, &p.name, None);
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
            run(c, Some(i))
        }
        Some(other) => bail!("Unknown command: {other}. Use squad --help"),
    }
}
