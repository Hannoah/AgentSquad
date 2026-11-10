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
/// True when `u` holds more RAM than `limit` bytes (0 means no limit).
pub fn over_mem_limit(u: Usage, limit: u64) -> bool {
    limit > 0 && u.mem > limit
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
    /// pid -> (CPU time in 100 ns units, resident bytes).
    #[cfg(unix)]
    pub usage: HashMap<u32, (u64, u64)>,
    /// pid -> process group id.
    #[cfg(unix)]
    pub pgid: HashMap<u32, u32>,
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

/// `ps` CPU time ("1:02.34" or "1:02:03.45") in 100 ns units; 0 when unreadable.
#[cfg(unix)]
fn parse_cputime(s: &str) -> u64 {
    let secs = s
        .split(':')
        .try_fold(0.0f64, |acc, p| Some(acc * 60.0 + p.parse::<f64>().ok()?));
    (secs.unwrap_or(0.0) * 1e7) as u64
}

#[cfg(unix)]
pub(crate) fn snapshot_table() -> ProcTable {
    let mut table = ProcTable::default();
    let out = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,pgid=,rss=,cputime=,comm="])
        .output();
    let Ok(out) = out else { return table };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // comm is last because the executable path may contain spaces.
        let mut f = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(pgid), Some(rss), Some(cpu)) =
            (f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        let (Ok(pid), Ok(ppid), Ok(pgid), Ok(rss)) = (
            pid.parse::<u32>(),
            ppid.parse::<u32>(),
            pgid.parse::<u32>(),
            rss.parse::<u64>(),
        ) else {
            continue;
        };
        table.children.entry(ppid).or_default().push(pid);
        table.pgid.insert(pid, pgid);
        let name = f.collect::<Vec<_>>().join(" ");
        // A login shell shows up as "-zsh".
        let name = match name.strip_prefix('-') {
            Some(rest) if !rest.contains('/') => rest.to_string(),
            _ => name,
        };
        table.names.insert(pid, name);
        table.usage.insert(pid, (parse_cputime(cpu), rss * 1024));
    }
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
fn is_interpreter(stem: &str) -> bool {
    INTERPRETERS.contains(&stem.to_ascii_lowercase().as_str())
}
/// Name for an interpreter running a script: `node …\node_modules\jarvis-cli\
/// dist\cli.js` is "jarvis-cli", `python app.py` is "app". None when `stem`
/// is not an interpreter or no script argument is found.
pub fn script_label(stem: &str, cmdline: &str) -> Option<String> {
    if !is_interpreter(stem) {
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
#[cfg(unix)]
pub fn command_line(pid: u32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}
#[cfg(not(any(unix, all(windows, target_pointer_width = "64"))))]
pub fn command_line(_pid: u32) -> Option<String> {
    None
}

#[cfg(windows)]
pub fn descendant_pids(root: u32) -> Vec<u32> {
    descendant_pids_from_map(root, &snapshot_table().children)
}

#[cfg(unix)]
pub fn descendant_pids(root: u32) -> Vec<u32> {
    descendant_pids_from_map(root, &snapshot_table().children)
}
#[cfg(not(any(unix, windows)))]
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
#[cfg(unix)]
pub fn kill_tree_by_pid(root: u32) {
    kill_trees(&[root]);
}
#[cfg(unix)]
pub fn kill_trees(roots: &[u32]) {
    if roots.is_empty() {
        return;
    }
    let table = snapshot_table();
    for &root in roots {
        let mut pids = descendant_pids_from_map(root, &table.children);
        pids.reverse();
        // Orphans reparented to launchd are no longer descendants but keep the group.
        pids.extend(group_members(root, &table));
        for pid in pids {
            sigkill(pid);
        }
    }
}
/// Kill what is left of the process group `pgid` after its leader exited and
/// was reaped. Pids are recycled, so a reaped root must never be swept by
/// walking its (possibly reused) pid; a group id, in contrast, cannot be
/// reused while any member lives.
#[cfg(unix)]
pub fn kill_group(pgid: u32) {
    let table = snapshot_table();
    // A live process with the leader's pid is a stranger that reused it.
    if table.names.contains_key(&pgid) {
        return;
    }
    for pid in group_members(pgid, &table) {
        sigkill(pid);
    }
}
#[cfg(unix)]
fn group_members(pgid: u32, table: &ProcTable) -> Vec<u32> {
    table
        .pgid
        .iter()
        .filter(|&(&p, &g)| g == pgid && p != pgid)
        .map(|(&p, _)| p)
        .collect()
}
#[cfg(unix)]
fn sigkill(pid: u32) {
    // SAFETY: SIGKILL to a pid in our own session; ESRCH/EPERM are ignored.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}
/// One process in the memory list.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub mem: u64,
    pub ppid: u32,
    /// Name of the parent, empty when it is gone or unknown.
    pub parent: String,
    /// Total CPU time used, in seconds.
    pub cpu_secs: u64,
    /// Parent, grandparent and so on (bounded), to tell whose session it runs in.
    pub ancestors: Vec<u32>,
}
/// pid -> parent pid, from a snapshot's child lists.
#[cfg(any(windows, unix))]
fn parent_map(children: &HashMap<u32, Vec<u32>>) -> HashMap<u32, u32> {
    children
        .iter()
        .flat_map(|(&ppid, kids)| kids.iter().map(move |&k| (k, ppid)))
        .collect()
}
/// Up to 32 ancestors of `pid`, nearest first; stops at a loop or the root.
#[cfg(any(windows, unix))]
pub(crate) fn ancestors_of(pid: u32, parents: &HashMap<u32, u32>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut at = pid;
    while let Some(&up) = parents.get(&at) {
        if up == 0 || up == pid || out.contains(&up) || out.len() >= 32 {
            break;
        }
        out.push(up);
        at = up;
    }
    out
}
/// The file name of a process path, as people know it.
#[cfg(any(windows, unix))]
fn base_name(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).to_string()
}
/// Fill the parent fields of a process from one snapshot.
#[cfg(any(windows, unix))]
fn relate(
    pid: u32,
    name: String,
    mem: u64,
    cpu_secs: u64,
    parents: &HashMap<u32, u32>,
    names: &HashMap<u32, String>,
) -> ProcInfo {
    let ppid = parents.get(&pid).copied().unwrap_or(0);
    ProcInfo {
        pid,
        name,
        mem,
        ppid,
        parent: names.get(&ppid).map(|n| base_name(n)).unwrap_or_default(),
        cpu_secs,
        ancestors: ancestors_of(pid, parents),
    }
}
/// The `n` biggest by RAM, ties by pid so the order never flickers.
#[cfg(any(windows, unix))]
fn sort_top(mut all: Vec<ProcInfo>, n: usize) -> Vec<ProcInfo> {
    all.sort_by(|a, b| b.mem.cmp(&a.mem).then(a.pid.cmp(&b.pid)));
    all.truncate(n);
    all
}
/// The `n` processes on the machine holding the most RAM, biggest first.
#[cfg(unix)]
pub fn top_processes(n: usize) -> Vec<ProcInfo> {
    let t = snapshot_table();
    let parents = parent_map(&t.children);
    let all = t
        .names
        .iter()
        .filter_map(|(&pid, name)| {
            let &(cpu, mem) = t.usage.get(&pid)?;
            // `comm` is a full path on macOS; the file name is what people know.
            Some(relate(
                pid,
                base_name(name),
                mem,
                cpu / 10_000_000,
                &parents,
                &t.names,
            ))
        })
        .collect();
    sort_top(all, n)
}
#[cfg(windows)]
pub fn top_processes(n: usize) -> Vec<ProcInfo> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FALSE, FILETIME},
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    };
    let t = snapshot_table();
    let parents = parent_map(&t.children);
    let ticks = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    let all = t
        .names
        .iter()
        .filter_map(|(&pid, name)| {
            // SAFETY: read-only queries on a handle opened and closed here; a
            // null handle only means the process is gone or out of reach.
            unsafe {
                let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
                if handle.is_null() {
                    return None;
                }
                let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
                counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
                let ok = GetProcessMemoryInfo(handle, &mut counters, counters.cb) != 0;
                let zero = FILETIME {
                    dwLowDateTime: 0,
                    dwHighDateTime: 0,
                };
                let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
                let cpu =
                    if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user)
                        != 0
                    {
                        (ticks(kernel) + ticks(user)) / 10_000_000
                    } else {
                        0
                    };
                CloseHandle(handle);
                ok.then_some(relate(
                    pid,
                    name.clone(),
                    counters.WorkingSetSize as u64,
                    cpu,
                    &parents,
                    &t.names,
                ))
            }
        })
        .collect();
    sort_top(all, n)
}
#[cfg(not(any(unix, windows)))]
pub fn top_processes(_n: usize) -> Vec<ProcInfo> {
    Vec::new()
}
/// Why squad must not kill `pid`, or None when it may. Killing the shell or
/// terminal squad runs in would take squad and every session down with it.
#[cfg(any(windows, unix))]
pub fn kill_refusal(pid: u32) -> Option<&'static str> {
    if pid <= 4 {
        return Some("system process");
    }
    // The tree walk includes the root, so squad's own pid counts too.
    let table = snapshot_table();
    descendant_pids_from_map(pid, &table.children)
        .contains(&std::process::id())
        .then_some("squad runs inside it")
}
#[cfg(not(any(windows, unix)))]
pub fn kill_refusal(_pid: u32) -> Option<&'static str> {
    Some("not supported on this platform")
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
#[cfg(unix)]
pub fn tree_usage(root: u32) -> Option<(u64, u64)> {
    Some(tree_usage_with_table(root, &snapshot_table()))
}
#[cfg(unix)]
fn tree_usage_with_table(root: u32, table: &ProcTable) -> (u64, u64) {
    descendant_pids_from_map(root, &table.children)
        .iter()
        .filter_map(|pid| table.usage.get(pid))
        .fold((0, 0), |(c, m), &(cpu, mem)| (c + cpu, m + mem))
}
#[cfg(not(any(unix, windows)))]
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
// libc marks the mach calls deprecated in favour of the mach2 crate; not worth a dependency.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
pub fn system_snapshot() -> Option<(CpuTimes, MemInfo)> {
    // SAFETY: the out-structs are live locals and the counts match their size.
    unsafe {
        // One port for the process lifetime: every call to mach_host_self adds a reference.
        static HOST: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let host = *HOST.get_or_init(|| libc::mach_host_self());
        let mut cpu: libc::host_cpu_load_info = std::mem::zeroed();
        let mut n = libc::HOST_CPU_LOAD_INFO_COUNT;
        if libc::host_statistics(
            host,
            libc::HOST_CPU_LOAD_INFO,
            (&mut cpu as *mut libc::host_cpu_load_info).cast(),
            &mut n,
        ) != 0
        {
            return None;
        }
        let mut vm: libc::vm_statistics64 = std::mem::zeroed();
        let mut n = libc::HOST_VM_INFO64_COUNT;
        if libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            (&mut vm as *mut libc::vm_statistics64).cast(),
            &mut n,
        ) != 0
        {
            return None;
        }
        let mut total: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        if libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut total as *mut u64).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        let [user, system, idle, nice] = [0, 1, 2, 3].map(|i| cpu.cpu_ticks[i] as u64);
        let page = libc::vm_page_size as u64;
        let avail =
            (vm.free_count as u64 + vm.inactive_count as u64 + vm.speculative_count as u64) * page;
        // Kernel time includes idle, as on Windows.
        Some(((idle, system + idle, user + nice), (total, avail)))
    }
}
#[cfg(not(any(windows, target_os = "macos")))]
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
    #[cfg(any(windows, unix))]
    let (procs, labels) = if roots.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let table = snapshot_table();
        let procs = roots
            .iter()
            .filter_map(|&(pi, pid)| {
                #[cfg(windows)]
                let usage = tree_usage_with_map(pid, &table.children);
                #[cfg(unix)]
                let usage = Some(tree_usage_with_table(pid, &table));
                usage.map(|(cpu, mem)| (pi, pid, cpu, mem))
            })
            .collect();
        let labels = roots
            .iter()
            .map(|&(pi, pid)| {
                let label = foreground_proc(pid, &table).map(|(fg, stem)| {
                    // The command line is a process probe: only interpreters need it.
                    is_interpreter(&stem)
                        .then(|| command_line(fg))
                        .flatten()
                        .and_then(|cmd| script_label(&stem, &cmd))
                        .unwrap_or(stem)
                });
                (pi, pid, label)
            })
            .collect();
        (procs, labels)
    };
    #[cfg(not(any(windows, unix)))]
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

/// Put `text` on the system clipboard as Unicode text.
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
#[cfg(target_os = "macos")]
pub fn set_clipboard(text: &str) -> Result<(), String> {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run pbcopy: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("pbcopy has no stdin")?
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn set_clipboard(_text: &str) -> Result<(), String> {
    Err("copying is only supported on Windows and macOS".into())
}
