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
        self.ascii_active = want_ascii;
        #[cfg(windows)]
        windows_impl::sync(self, want_ascii);
        #[cfg(unix)]
        unix_impl::sync(self, want_ascii);
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
    use std::ffi::c_void;
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
    const IME_CMODE_NATIVE: usize = 0x0001;

    pub(super) fn console_hwnd() -> isize {
        // SAFETY: Win32 query returning an HWND (or null).
        unsafe { GetConsoleWindow() as isize }
    }

    /// Collect our own PID and up to 16 ancestor PIDs (e.g. powershell -> WindowsTerminal)
    /// so we only touch the IME when our own host terminal window is focused.
    pub(super) fn ancestor_pids() -> Vec<u32> {
        let mut parents = HashMap::new();
        // SAFETY: read-only process snapshot closed before return.
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snap != INVALID_HANDLE_VALUE {
            let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = unsafe { Process32FirstW(snap, &mut entry) };
            while more != FALSE {
                parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
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
            out.push(cur);
            cur = parents.get(&cur).copied().unwrap_or(0);
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

    pub(super) fn sync(guard: &mut ImeGuard, want_ascii: bool) {
        let Some(ime) = active_ime_wnd(guard) else {
            return;
        };
        let _ = std::ptr::null::<c_void>();
        if want_ascii {
            let open = ime_ctrl(ime, IMC_GETOPENSTATUS, 0).unwrap_or(0);
            let conv = ime_ctrl(ime, IMC_GETCONVERSIONMODE, 0).unwrap_or(0);
            if open != 0 || (conv & IME_CMODE_NATIVE) != 0 {
                if guard.saved.is_none() {
                    guard.saved = Some((conv, open));
                }
                let ascii_conv = (conv & !IME_CMODE_NATIVE) as isize;
                let _ = ime_ctrl(ime, IMC_SETCONVERSIONMODE, ascii_conv);
                let _ = ime_ctrl(ime, IMC_SETOPENSTATUS, 0);
            }
        } else if let Some((conv, open)) = guard.saved.take() {
            let restore_open = if open != 0 { open as isize } else { 1 };
            let _ = ime_ctrl(ime, IMC_SETOPENSTATUS, restore_open);
            let _ = ime_ctrl(
                ime,
                IMC_SETCONVERSIONMODE,
                (conv | IME_CMODE_NATIVE) as isize,
            );
        }
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
