//! Mode-aware IME synchronization so single-key shortcuts (`j`/`k`/`a`/`s`/`d`/`e`…)
//! work immediately even when the user's IME is in Korean (`한글`) mode, while
//! restoring Korean composition automatically in text-entry modes (`Focus`, `Send`,
//! `Filter`, `Add` text fields, and `EnvEditor`) and on exit.

/// Manages automatic IME switching between shortcut-only modes and text-entry
/// modes, restoring the user's original IME state on exit (`Drop`).
#[derive(Default)]
pub struct ImeGuard {
    ascii_active: bool,
    #[cfg(windows)]
    saved: Option<(usize, usize)>,
    #[cfg(windows)]
    console_hwnd: isize,
    #[cfg(windows)]
    host_pids: Vec<u32>,
    #[cfg(unix)]
    kitty_supported: bool,
    #[cfg(unix)]
    kitty_pushed: bool,
}

impl ImeGuard {
    pub fn new() -> Self {
        #[cfg(windows)]
        {
            Self {
                ascii_active: false,
                saved: None,
                console_hwnd: windows_impl::console_hwnd(),
                host_pids: windows_impl::ancestor_pids(),
            }
        }
        #[cfg(unix)]
        {
            Self {
                ascii_active: false,
                kitty_supported: unix_impl::terminal_supports_kitty(),
                kitty_pushed: false,
            }
        }
        #[cfg(not(any(windows, unix)))]
        {
            Self::default()
        }
    }

    /// Synchronize the terminal/OS IME state with `want_ascii`.
    /// Set `force` when a Hangul key was received during a shortcut-only mode
    /// (for instance, if the user toggled IME back to Korean manually).
    pub fn sync(&mut self, want_ascii: bool, force: bool) {
        if want_ascii == self.ascii_active && !force {
            return;
        }
        // Only a switch that reached the IME counts as done: while another
        // window has the foreground nothing is touched, and the next pass
        // retries until our terminal is focused again.
        #[cfg(windows)]
        let applied = windows_impl::sync(self, want_ascii);
        #[cfg(unix)]
        let applied = {
            unix_impl::sync(self, want_ascii);
            true
        };
        #[cfg(not(any(windows, unix)))]
        let applied = true;
        if applied {
            self.ascii_active = want_ascii;
        }
    }
}

/// IMM conversion-mode bits `plan` works with.
pub const IME_CMODE_NATIVE: usize = 0x0001;
pub const IME_CMODE_FULLSHAPE: usize = 0x0008;

/// An IME state: `(conversion mode, open status)`.
pub type ImeState = (usize, usize);

/// What to do with an IME currently at `(conv, open)`: the state to save for
/// later and the `(conv, open)` to set, if it must change.
///
/// Full-width shape (`Ｔｏｋｅｎ`, `U+3000` spaces) is never kept or restored:
/// a Hangul state can carry the bit, and only clearing NATIVE once left the
/// terminal typing full-width English and then full-width Hangul spacing.
pub fn plan(
    want_ascii: bool,
    conv: usize,
    open: usize,
    saved: Option<ImeState>,
) -> (Option<ImeState>, Option<ImeState>) {
    let half = |c: usize| c & !IME_CMODE_FULLSHAPE;
    if want_ascii {
        // Korean IMEs report "open" in English mode too; only the native
        // (Hangul) bit means keys would compose, so only that is saved.
        let saved = if conv & IME_CMODE_NATIVE != 0 {
            saved.or(Some((conv, open)))
        } else {
            saved
        };
        let set = if conv & IME_CMODE_NATIVE != 0 {
            Some((half(conv) & !IME_CMODE_NATIVE, 0))
        } else if conv & IME_CMODE_FULLSHAPE != 0 {
            Some((half(conv), open))
        } else {
            None
        };
        (saved, set)
    } else {
        // Put back what the user had (or keep what is there), half-width.
        let (c, o) = saved.unwrap_or((conv, open));
        let target = (half(c), o);
        (None, (target != (conv, open)).then_some(target))
    }
}

impl Drop for ImeGuard {
    fn drop(&mut self) {
        self.sync(false, false);
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::ImeGuard;
    use std::collections::HashMap;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FALSE, HWND, INVALID_HANDLE_VALUE},
        System::{
            Console::GetConsoleWindow,
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
        },
        UI::{
            Input::Ime::ImmGetDefaultIMEWnd,
            WindowsAndMessaging::{
                GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
                SendMessageTimeoutW, GUITHREADINFO, SMTO_ABORTIFHUNG,
            },
        },
    };

    const WM_IME_CONTROL: u32 = 0x0283;
    const IMC_GETCONVERSIONMODE: usize = 0x0001;
    const IMC_SETCONVERSIONMODE: usize = 0x0002;
    const IMC_GETOPENSTATUS: usize = 0x0005;
    const IMC_SETOPENSTATUS: usize = 0x0006;

    pub(super) fn console_hwnd() -> isize {
        // SAFETY: Win32 query returning an HWND (or null).
        unsafe { GetConsoleWindow() as isize }
    }

    /// Desktop-shell and system processes that may sit above the terminal in
    /// the parent chain; their windows (Explorer, the taskbar) are not ours.
    const NOT_A_HOST: [&str; 6] = [
        "explorer.exe",
        "svchost.exe",
        "sihost.exe",
        "services.exe",
        "winlogon.exe",
        "userinit.exe",
    ];

    /// Collect our own PID and up to 16 ancestor PIDs (e.g. powershell -> WindowsTerminal)
    /// so we only touch the IME when our own host terminal window is focused.
    /// The walk stops at the desktop shell, or focusing File Explorer would
    /// count as focusing squad.
    pub(super) fn ancestor_pids() -> Vec<u32> {
        let mut parents: HashMap<u32, (u32, String)> = HashMap::new();
        // SAFETY: read-only process snapshot closed before return.
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snap != INVALID_HANDLE_VALUE {
            let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = unsafe { Process32FirstW(snap, &mut entry) };
            while more != FALSE {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
                parents.insert(entry.th32ProcessID, (entry.th32ParentProcessID, exe));
                more = unsafe { Process32NextW(snap, &mut entry) };
            }
            unsafe { CloseHandle(snap) };
        }
        let mut out = Vec::new();
        let mut cur = std::process::id();
        for _ in 0..16 {
            if cur == 0 || out.contains(&cur) {
                break;
            }
            let (parent, exe) = parents
                .get(&cur)
                .map_or((0, ""), |(pp, e)| (*pp, e.as_str()));
            if NOT_A_HOST.iter().any(|n| exe.eq_ignore_ascii_case(n)) {
                break;
            }
            out.push(cur);
            cur = parent;
        }
        out
    }

    fn active_ime_wnd(guard: &ImeGuard) -> Option<HWND> {
        // SAFETY: Read-only Win32 focus/IME queries on HWND/thread handles.
        unsafe {
            let fg = GetForegroundWindow();
            if fg.is_null() {
                return None;
            }
            let mut fg_pid = 0u32;
            let tid = GetWindowThreadProcessId(fg, &mut fg_pid);
            let is_our_terminal =
                (fg as isize) == guard.console_hwnd || guard.host_pids.contains(&fg_pid);
            if !is_our_terminal {
                return None;
            }
            let mut target = fg;
            if tid != 0 {
                let mut gui: GUITHREADINFO = std::mem::zeroed();
                gui.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
                if GetGUIThreadInfo(tid, &mut gui) != 0 && !gui.hwndFocus.is_null() {
                    target = gui.hwndFocus;
                }
            }
            let ime = ImmGetDefaultIMEWnd(target);
            let ime = if ime.is_null() && target != fg {
                ImmGetDefaultIMEWnd(fg)
            } else {
                ime
            };
            (!ime.is_null()).then_some(ime)
        }
    }

    fn ime_ctrl(ime: HWND, cmd: usize, arg: isize) -> Option<usize> {
        let mut res = 0usize;
        // SAFETY: SendMessageTimeoutW with SMTO_ABORTIFHUNG and 50ms timeout never blocks the UI thread.
        let ok = unsafe {
            SendMessageTimeoutW(
                ime,
                WM_IME_CONTROL,
                cmd,
                arg,
                SMTO_ABORTIFHUNG,
                50,
                &mut res,
            )
        };
        (ok != 0).then_some(res)
    }

    /// Returns false when our terminal is not in the foreground (nothing done).
    pub(super) fn sync(guard: &mut ImeGuard, want_ascii: bool) -> bool {
        let Some(ime) = active_ime_wnd(guard) else {
            return false;
        };
        let (Some(conv), Some(open)) = (
            ime_ctrl(ime, IMC_GETCONVERSIONMODE, 0),
            ime_ctrl(ime, IMC_GETOPENSTATUS, 0),
        ) else {
            return false; // the IME did not answer; try again next pass
        };
        let (saved, set) = super::plan(want_ascii, conv, open, guard.saved);
        guard.saved = saved;
        if let Some((conv, open)) = set {
            // Open first: reopening a Korean IME resets its conversion mode.
            let _ = ime_ctrl(ime, IMC_SETOPENSTATUS, open as isize);
            let _ = ime_ctrl(ime, IMC_SETCONVERSIONMODE, conv as isize);
        }
        true
    }
}

#[cfg(unix)]
mod unix_impl {
    use super::ImeGuard;
    use std::io::Write;

    pub(super) fn terminal_supports_kitty() -> bool {
        if std::env::var_os("KITTY_WINDOW_ID").is_some() {
            return true;
        }
        matches!(
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            Some("ghostty" | "WezTerm" | "iTerm.app")
        )
    }

    pub(super) fn sync(guard: &mut ImeGuard, want_ascii: bool) {
        if !guard.kitty_supported {
            return;
        }
        if want_ascii && !guard.kitty_pushed {
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b[>1u");
            let _ = out.flush();
            guard.kitty_pushed = true;
        } else if !want_ascii && guard.kitty_pushed {
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b[<u");
            let _ = out.flush();
            guard.kitty_pushed = false;
        }
    }
}
