use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    style::{Color, Style},
    text::Line,
    Terminal,
};
use std::{
    env, fs,
    num::NonZero,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use crate::config::*;
use crate::input::*;
use crate::session::*;
use crate::sys::*;
use crate::ui::*;
use crate::updater::*;

mod config;
mod input;
mod ledger;
mod pathpick;
mod session;
mod sys;
mod ui;
mod updater;

#[cfg(windows)]
fn ps_stdout(script: &str) -> String {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    String::from_utf8_lossy(&out).into_owned()
}
#[cfg(windows)]
fn child_pids(parent: u32) -> Vec<u32> {
    ps_stdout(&format!(
            "Get-CimInstance Win32_Process -Filter 'ParentProcessId={parent}' | Select-Object -ExpandProperty ProcessId"
        ))
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .collect()
}
#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    let out = ps_stdout(&format!("tasklist /FI \"PID eq {pid}\" /FO CSV /NH"));
    out.contains(&format!("\"{pid}\""))
}
fn scratch_dir(tag: &str) -> PathBuf {
    let d = env::temp_dir().join(format!("squad-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
