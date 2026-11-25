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

mod app;
mod completion;
mod config;
mod envvault;
mod history;
mod input;
mod json;
mod ledger;
mod pathpick;
mod refs;
mod select;
mod session;
mod skills;
mod sys;
mod tools;
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
/// Processes held open by handle so a test can wait for them to die. The
/// handle pins each pid: once the kill frees a number, a stranger reusing it
/// cannot pass for a survivor (polling `tasklist` by pid could, under load).
#[cfg(windows)]
struct Watched(Vec<(u32, windows_sys::Win32::Foundation::HANDLE)>);
#[cfg(windows)]
impl Watched {
    /// Open every pid that is still alive; check `len()` against what you asked for.
    fn new(pids: &[u32]) -> Self {
        use windows_sys::Win32::{
            Foundation::FALSE,
            System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE},
        };
        Watched(
            pids.iter()
                .filter_map(|&pid| {
                    // SAFETY: plain call; a null handle means the pid is gone.
                    let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, FALSE, pid) };
                    (!h.is_null()).then_some((pid, h))
                })
                .collect(),
        )
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    /// The pids still alive once `limit` (shared by all of them) runs out.
    fn survivors(&self, limit: Duration) -> Vec<u32> {
        use windows_sys::Win32::{
            Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject,
        };
        let end = Instant::now() + limit;
        self.0
            .iter()
            .filter(|(_, h)| {
                let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
                // SAFETY: the handle stays open until `drop`.
                unsafe { WaitForSingleObject(*h, left) != WAIT_OBJECT_0 }
            })
            .map(|(pid, _)| *pid)
            .collect()
    }
}
#[cfg(windows)]
impl Drop for Watched {
    fn drop(&mut self) {
        for (_, h) in &self.0 {
            // SAFETY: each handle was opened by `new` and is closed once.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(*h) };
        }
    }
}
fn scratch_dir(tag: &str) -> PathBuf {
    let d = env::temp_dir().join(format!("squad-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
