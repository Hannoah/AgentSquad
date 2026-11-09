use crate::config::{is_shell_program, program_name, split_command};
use ratatui::style::{Color, Style};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::Instant,
};

/// CPU and memory of one session's process tree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Usage {
    pub cpu: f32,
    pub mem: u64,
}
/// CPU percent from process CPU time (100 ns units) between two samples.
pub fn cpu_percent(prev: u64, now: u64, secs: f64, cores: usize) -> f32 {
    if secs <= 0.0 || cores == 0 {
        return 0.0;
    }
    (now.saturating_sub(prev) as f64 / 1e7 / secs / cores as f64 * 100.0).clamp(0.0, 100.0) as f32
}
/// Whole-machine CPU percent from two (idle, kernel, user) readings.
/// Kernel time already includes idle time.
pub fn sys_cpu_percent(prev: CpuTimes, now: CpuTimes) -> f32 {
    let idle = now.0.saturating_sub(prev.0);
    let total = (now.1 + now.2).saturating_sub(prev.1 + prev.2);
    if total == 0 {
        0.0
    } else {
        (total.saturating_sub(idle) as f64 / total as f64 * 100.0) as f32
    }
}
/// Compact byte size: "512M", "1.4G".
pub fn mem_label(bytes: u64) -> String {
    pub const MIB: u64 = 1 << 20;
    if bytes >= 1 << 30 {
        format!("{:.1}G", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{}M", bytes / MIB)
    }
}
/// Sidebar/title label for a session: "12% 340M".
pub fn usage_label(u: Usage) -> String {
    format!("{:.0}% {}", u.cpu, mem_label(u.mem))
}
/// Whole-machine load, refreshed by `sample`.
#[derive(Default)]
pub struct SysUsage {
    pub cpu: f32,
    pub used: u64,
    pub total: u64,
    pub prev: Option<CpuTimes>,
}
impl SysUsage {
    pub fn apply(&mut self, (times, (total, avail)): (CpuTimes, MemInfo)) {
        if let Some(prev) = self.prev {
            self.cpu = sys_cpu_percent(prev, times);
        }
        self.prev = Some(times);
        self.total = total;
        self.used = total.saturating_sub(avail);
    }
    pub fn ram_percent(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.used as f64 / self.total as f64 * 100.0) as f32
        }
    }
    /// Right-hand status bar text: "CPU 23% · RAM 9.7G/16.0G".
    pub fn summary(&self) -> String {
        format!(
            "CPU {:.0}% · RAM {}/{}",
            self.cpu,
            mem_label(self.used),
            mem_label(self.total)
        )
    }
    /// Yellow past 70%, red past 90% on whichever resource is worse.
    pub fn style(&self) -> Style {
        let load = self.cpu.max(self.ram_percent());
        if load >= 90.0 {
            Style::default().fg(Color::Red)
        } else if load >= 70.0 {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    }
}

/// One process-table snapshot: who is whose child, and what each runs.
/// Taken once per round so several session trees share a single walk.
#[derive(Default)]
pub struct ProcTable {
    pub children: HashMap<u32, Vec<u32>>,
    pub names: HashMap<u32, String>,
}

#[cfg(windows)]
fn snapshot_table() -> ProcTable {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FALSE, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
    };
    let mut table = ProcTable::default();
    // SAFETY: process snapshot of our own machine; the handle is closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return table;
    }
    // SAFETY: dwSize is initialized; the API fills the remaining fields.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: `entry` stays a valid buffer for the whole walk.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    while more != FALSE {
        table
            .children
            .entry(entry.th32ParentProcessID)
            .or_default()
            .push(entry.th32ProcessID);
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        table.names.insert(
            entry.th32ProcessID,
            String::from_utf16_lossy(&entry.szExeFile[..len]),
        );
        // SAFETY: same valid buffer as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    unsafe { CloseHandle(snapshot) };
    table
}

fn descendant_pids_from_map(root: u32, children: &HashMap<u32, Vec<u32>>) -> Vec<u32> {
    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !visited.insert(pid) {
            continue;
        }
        ordered.push(pid);
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    ordered
}

/// What the user actually launched inside a shell session: the shallowest
/// descendant of `root` that is not itself a shell or console host. A
/// service registered as plain `cmd.exe` that runs `jarvis` reads as
/// "jarvis". None when only shells are running.
pub fn foreground_name(root: u32, table: &ProcTable) -> Option<String> {
    foreground_proc(root, table).map(|(_, stem)| stem)
}
/// `foreground_name` plus the pid it came from, so the caller can look at
/// that process's command line.
fn foreground_proc(root: u32, table: &ProcTable) -> Option<(u32, String)> {
    let mut seen = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        if pid != root {
            if let Some(stem) = table
                .names
                .get(&pid)
                .and_then(|n| program_name(std::slice::from_ref(n)))
            {
                if !is_shell_program(&stem) {
                    return Some((pid, stem));
                }
            }
        }
        for &kid in table.children.get(&pid).into_iter().flatten() {
            if seen.insert(kid) {
                queue.push_back(kid);
            }
        }
    }
    None
}

/// Programs whose own name says nothing about what they run.
const INTERPRETERS: [&str; 7] = ["node", "python", "pythonw", "py", "deno", "bun", "ruby"];
/// Name for an interpreter running a script: `node …\node_modules\jarvis-cli\
/// dist\cli.js` is "jarvis-cli", `python app.py` is "app". None when `stem`
/// is not an interpreter or no script argument is found.
pub fn script_label(stem: &str, cmdline: &str) -> Option<String> {
    if !INTERPRETERS.contains(&stem.to_ascii_lowercase().as_str()) {
        return None;
    }
    let args = split_command(cmdline);
    let script = args.iter().skip(1).find(|a| !a.starts_with('-'))?;
    let parts: Vec<&str> = script
        .split(['\\', '/'])
        .filter(|p| !p.is_empty())
        .collect();
    // The folder after the last node_modules names the tool; `@scope/pkg`
    // is named by pkg.
    if let Some(i) = parts.iter().rposition(|p| *p == "node_modules") {
        let at = if parts.get(i + 1).is_some_and(|p| p.starts_with('@')) {
            i + 2
        } else {
            i + 1
        };
        if let Some(pkg) = parts.get(at) {
            return Some(pkg.to_string());
        }
    }
    let file = parts.last()?;
    let stem = std::path::Path::new(file).file_stem()?.to_string_lossy();
    (!stem.is_empty()).then(|| stem.into_owned())
}
/// Command line of process `pid`, read from its PEB. Best effort: None when
/// the process is gone, protected, or not a 64-bit process.
#[cfg(all(windows, target_pointer_width = "64"))]
pub fn command_line(pid: u32) -> Option<String> {
    use std::ffi::c_void;
    use windows_sys::{
        Wdk::System::Threading::NtQueryInformationProcess,
        Win32::{
            Foundation::{CloseHandle, FALSE},
            System::{
                Diagnostics::Debug::ReadProcessMemory,
                Threading::{
                    OpenProcess, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
                    PROCESS_VM_READ,
                },
            },
        },
    };
    // SAFETY: the handle is opened and closed here, and every out-pointer
    // references a live local buffer of the stated size. The offsets are the
    // documented x64 layout: PEB.ProcessParameters at 0x20 and
    // RTL_USER_PROCESS_PARAMETERS.CommandLine (a UNICODE_STRING) at 0x70.
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            FALSE,
            pid,
        );
        if handle.is_null() {
            return None;
        }
        let read = |addr: usize, buf: *mut c_void, len: usize| {
            ReadProcessMemory(
                handle,
                addr as *const c_void,
                buf,
                len,
                std::ptr::null_mut(),
            ) != 0
        };
        let result = (|| {
            let mut pbi: PROCESS_BASIC_INFORMATION = std::mem::zeroed();
            let size = std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32;
            let status = NtQueryInformationProcess(
                handle,
                0, // ProcessBasicInformation
                &mut pbi as *mut _ as *mut c_void,
                size,
                std::ptr::null_mut(),
            );
            if status != 0 || pbi.PebBaseAddress.is_null() {
                return None;
            }
            let mut params = [0u8; 8];
            if !read(
                pbi.PebBaseAddress as usize + 0x20,
                params.as_mut_ptr().cast(),
                8,
            ) {
                return None;
            }
            let mut us = [0u8; 16];
            if !read(
                usize::from_le_bytes(params) + 0x70,
                us.as_mut_ptr().cast(),
                16,
            ) {
                return None;
            }
            let bytes = u16::from_le_bytes([us[0], us[1]]) as usize;
            let ptr = usize::from_le_bytes(us[8..16].try_into().ok()?);
            if bytes == 0 || ptr == 0 {
                return None;
            }
            let mut buf = vec![0u16; bytes / 2];
            if !read(ptr, buf.as_mut_ptr().cast(), bytes / 2 * 2) {
                return None;
            }
            Some(String::from_utf16_lossy(&buf))
        })();
        CloseHandle(handle);
        result
    }
}
#[cfg(not(all(windows, target_pointer_width = "64")))]
pub fn command_line(_pid: u32) -> Option<String> {
    None
}

#[cfg(windows)]
pub fn descendant_pids(root: u32) -> Vec<u32> {
    descendant_pids_from_map(root, &snapshot_table().children)
}

#[cfg(not(windows))]
pub fn descendant_pids(_root: u32) -> Vec<u32> {
    Vec::new()
}

/// Best-effort kill of a whole process tree by PID, leaves first.
///
/// Terminates every enumerated descendant (and the root) directly instead of
/// asking a live wrapper to take its tree down, so already-orphaned
/// grandchildren die too. Missing or denied PIDs are skipped silently.
#[cfg(windows)]
pub fn kill_tree_by_pid(root: u32) {
    kill_trees(&[root]);
}

/// Kill several process trees using one process-table snapshot, so closing
/// N sessions costs one walk instead of N.
#[cfg(windows)]
pub fn kill_trees(roots: &[u32]) {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FALSE},
        System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE},
    };
    if roots.is_empty() {
        return;
    }
    let table = snapshot_table();
    for &root in roots {
        let mut pids = descendant_pids_from_map(root, &table.children);
        pids.reverse();
        for pid in pids {
            // SAFETY: terminating processes inside our own session tree; a null
            // handle only means the PID is already gone or out of reach.
            unsafe {
                let handle = OpenProcess(PROCESS_TERMINATE, FALSE, pid);
                if handle.is_null() {
                    continue;
                }
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }
}
/// (CPU time in 100 ns units, working-set bytes) summed over the process
/// tree rooted at `root`. None when the platform has no probe.
#[cfg(windows)]
pub fn tree_usage(root: u32) -> Option<(u64, u64)> {
    tree_usage_with_map(root, &snapshot_table().children)
}

#[cfg(windows)]
fn tree_usage_with_map(root: u32, children: &HashMap<u32, Vec<u32>>) -> Option<(u64, u64)> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FALSE, FILETIME},
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    };
    let ticks = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    let (mut cpu, mut mem) = (0u64, 0u64);
    for pid in descendant_pids_from_map(root, children) {
        // SAFETY: read-only queries on a handle we open and close here; a
        // null handle only means the process is gone or out of reach.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
            if handle.is_null() {
                continue;
            }
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
            if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0 {
                cpu += ticks(kernel) + ticks(user);
            }
            let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            if GetProcessMemoryInfo(handle, &mut counters, counters.cb) != 0 {
                mem += counters.WorkingSetSize as u64;
            }
            CloseHandle(handle);
        }
    }
    Some((cpu, mem))
}
#[cfg(not(windows))]
pub fn tree_usage(_root: u32) -> Option<(u64, u64)> {
    None
}
/// (idle, kernel, user) system CPU times in 100 ns units.
pub type CpuTimes = (u64, u64, u64);
/// (total, available) physical memory in bytes.
pub type MemInfo = (u64, u64);
#[cfg(windows)]
pub fn system_snapshot() -> Option<(CpuTimes, MemInfo)> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
            Threading::GetSystemTimes,
        },
    };
    let ticks = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut idle, mut kernel, mut user) = (zero, zero, zero);
    // SAFETY: out-pointers reference live locals; dwLength is set as required.
    unsafe {
        if GetSystemTimes(&mut idle, &mut kernel, &mut user) == 0 {
            return None;
        }
        let mut mem: MEMORYSTATUSEX = std::mem::zeroed();
        mem.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut mem) == 0 {
            return None;
        }
        Some((
            (ticks(idle), ticks(kernel), ticks(user)),
            (mem.ullTotalPhys, mem.ullAvailPhys),
        ))
    }
}
#[cfg(not(windows))]
pub fn system_snapshot() -> Option<(CpuTimes, MemInfo)> {
    None
}
/// One round of measurements, taken off the UI thread.
pub struct Measurement {
    pub at: Instant,
    pub sys: Option<(CpuTimes, MemInfo)>,
    pub procs: Vec<(usize, u32, u64, u64)>,
    /// (project index, root pid, foreground program) per measured session.
    pub labels: Vec<(usize, u32, Option<String>)>,
}
/// Probe the machine and the given session roots. Walking the process
/// table can take tens of milliseconds, which is why this never runs on
/// the thread that draws and reads keys.
pub fn measure(roots: &[(usize, u32)]) -> Measurement {
    #[cfg(windows)]
    let (procs, labels) = if roots.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let table = snapshot_table();
        let procs = roots
            .iter()
            .filter_map(|&(pi, pid)| {
                tree_usage_with_map(pid, &table.children).map(|(cpu, mem)| (pi, pid, cpu, mem))
            })
            .collect();
        let labels = roots
            .iter()
            .map(|&(pi, pid)| {
                let label = foreground_proc(pid, &table).map(|(fg, stem)| {
                    command_line(fg)
                        .and_then(|cmd| script_label(&stem, &cmd))
                        .unwrap_or(stem)
                });
                (pi, pid, label)
            })
            .collect();
        (procs, labels)
    };
    #[cfg(not(windows))]
    let (procs, labels) = (
        roots
            .iter()
            .filter_map(|&(pi, pid)| tree_usage(pid).map(|(cpu, mem)| (pi, pid, cpu, mem)))
            .collect(),
        Vec::new(),
    );
    Measurement {
        at: Instant::now(),
        sys: system_snapshot(),
        procs,
        labels,
    }
}

/// The console window's current title, so it can be put back on exit.
#[cfg(windows)]
pub fn console_title() -> Option<String> {
    use windows_sys::Win32::System::Console::GetConsoleTitleW;
    let mut buf = [0u16; 1024];
    // SAFETY: `buf` is a live buffer of the stated length.
    let n = unsafe { GetConsoleTitleW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n.min(buf.len())]))
}
#[cfg(not(windows))]
pub fn console_title() -> Option<String> {
    None
}

/// Put `text` on the Windows clipboard as Unicode text.
#[cfg(windows)]
pub fn set_clipboard(text: &str) -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::GlobalFree,
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        },
    };
    const CF_UNICODETEXT: u32 = 13;
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the clipboard is opened and closed here; the global block is
    // sized for `wide`, filled while locked, and handed to the system only
    // when SetClipboardData succeeds (otherwise it is freed).
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err("the clipboard is in use by another program".into());
        }
        let result = (|| {
            EmptyClipboard();
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
            if mem.is_null() {
                return Err("out of memory".to_string());
            }
            let dst = GlobalLock(mem).cast::<u16>();
            if dst.is_null() {
                GlobalFree(mem);
                return Err("cannot lock clipboard memory".to_string());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
            GlobalUnlock(mem);
            if SetClipboardData(CF_UNICODETEXT, mem).is_null() {
                GlobalFree(mem);
                return Err("the clipboard refused the text".to_string());
            }
            Ok(())
        })();
        CloseClipboard();
        result
    }
}
#[cfg(not(windows))]
pub fn set_clipboard(_text: &str) -> Result<(), String> {
    Err("copying is only supported on Windows".into())
}
