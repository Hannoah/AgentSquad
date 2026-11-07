use ratatui::style::{Color, Style};
use std::time::Instant;

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
#[cfg(windows)]
fn snapshot_children() -> std::collections::HashMap<u32, Vec<u32>> {
    use std::collections::HashMap;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FALSE, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
    };
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    // SAFETY: process snapshot of our own machine; the handle is closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return children;
    }
    // SAFETY: dwSize is initialized; the API fills the remaining fields.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: `entry` stays a valid buffer for the whole walk.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    while more != FALSE {
        children
            .entry(entry.th32ParentProcessID)
            .or_default()
            .push(entry.th32ProcessID);
        // SAFETY: same valid buffer as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    unsafe { CloseHandle(snapshot) };
    children
}

#[cfg(windows)]
fn descendant_pids_from_map(
    root: u32,
    children: &std::collections::HashMap<u32, Vec<u32>>,
) -> Vec<u32> {
    let mut ordered = Vec::new();
    let mut visited = std::collections::HashSet::new();
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

/// Short uptime label for a running session: "7s", "12m", "3h".
#[cfg(windows)]
pub fn descendant_pids(root: u32) -> Vec<u32> {
    let children = snapshot_children();
    descendant_pids_from_map(root, &children)
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
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FALSE},
        System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE},
    };
    let mut pids = descendant_pids(root);
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
/// (CPU time in 100 ns units, working-set bytes) summed over the process
/// tree rooted at `root`. None when the platform has no probe.
#[cfg(windows)]
pub fn tree_usage(root: u32) -> Option<(u64, u64)> {
    let children = snapshot_children();
    tree_usage_with_map(root, &children)
}

#[cfg(windows)]
fn tree_usage_with_map(
    root: u32,
    children: &std::collections::HashMap<u32, Vec<u32>>,
) -> Option<(u64, u64)> {
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
}
/// Probe the machine and the given session roots. Walking the process
/// table can take tens of milliseconds, which is why this never runs on
/// the thread that draws and reads keys.
pub fn measure(roots: &[(usize, u32)]) -> Measurement {
    #[cfg(windows)]
    let procs = if roots.is_empty() {
        Vec::new()
    } else {
        let children = snapshot_children();
        roots
            .iter()
            .filter_map(|&(pi, pid)| {
                tree_usage_with_map(pid, &children).map(|(cpu, mem)| (pi, pid, cpu, mem))
            })
            .collect()
    };
    #[cfg(not(windows))]
    let procs = roots
        .iter()
        .filter_map(|&(pi, pid)| tree_usage(pid).map(|(cpu, mem)| (pi, pid, cpu, mem)))
        .collect();
    Measurement {
        at: Instant::now(),
        sys: system_snapshot(),
        procs,
    }
}
