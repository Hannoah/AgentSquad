use super::*;

#[test]
fn cpu_percent_normalizes_by_cores() {
    // 1 s of CPU time (1e7 ticks) over 1 s wall on 4 cores = 25%.
    assert_eq!(cpu_percent(0, 10_000_000, 1.0, 4), 25.0);
    assert_eq!(cpu_percent(10, 10, 1.0, 4), 0.0);
    // A shrinking tree (a child exited) never goes negative.
    assert_eq!(cpu_percent(500, 100, 1.0, 4), 0.0);
    assert_eq!(cpu_percent(0, 1, 0.0, 4), 0.0, "zero interval");
    assert_eq!(cpu_percent(0, u64::MAX, 1.0, 1), 100.0, "clamped");
}
#[test]
fn sys_cpu_percent_uses_idle_share() {
    // Over the interval: 100 ticks total, 75 of them idle => 25% busy.
    assert_eq!(sys_cpu_percent((0, 0, 0), (75, 80, 20)), 25.0);
    assert_eq!(
        sys_cpu_percent((5, 10, 10), (5, 10, 10)),
        0.0,
        "no time passed"
    );
}
#[test]
fn mem_and_usage_labels() {
    assert_eq!(mem_label(0), "0M");
    assert_eq!(mem_label(340 << 20), "340M");
    assert_eq!(mem_label(1 << 30), "1.0G");
    assert_eq!(mem_label(5 * (1 << 29)), "2.5G");
    let u = Usage {
        cpu: 12.4,
        mem: 340 << 20,
    };
    assert_eq!(usage_label(u), "12% 340M");
    let sys = SysUsage {
        cpu: 23.0,
        used: 5 << 30,
        total: 16 << 30,
        prev: None,
    };
    assert_eq!(sys.summary(), "CPU 23% · RAM 5.0G/16.0G");
    assert_eq!(sys.style().fg, Some(Color::DarkGray));
    let hot = SysUsage { cpu: 95.0, ..sys };
    assert_eq!(hot.style().fg, Some(Color::Red));
    let warm = SysUsage {
        cpu: 1.0,
        used: 12 << 30,
        total: 16 << 30,
        prev: None,
    };
    assert_eq!(warm.style().fg, Some(Color::Yellow), "75% RAM warns");
}
#[test]
#[cfg(any(windows, target_os = "macos"))]
fn probes_read_real_numbers() {
    let (times, (total, avail)) = system_snapshot().expect("system snapshot");
    assert!(total > 0 && avail <= total);
    assert!(times.1 + times.2 > 0);
    // `ps` reports CPU time in 10 ms steps: burn enough that it is not 0.
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(150) {
        std::hint::black_box(0u64);
    }
    let (cpu, mem) = tree_usage(std::process::id()).expect("tree usage");
    assert!(cpu > 0, "this test process has used some CPU");
    assert!(mem > 0, "this test process has a working set");
    let mut s = SysUsage::default();
    s.apply(system_snapshot().unwrap());
    std::thread::sleep(Duration::from_millis(50));
    s.apply(system_snapshot().unwrap());
    assert!(s.total > 0 && s.used > 0 && (0.0..=100.0).contains(&s.cpu));
    // Cost of one round on this machine; it is why measuring is off-thread.
    let t = Instant::now();
    let m = measure(&[(0, std::process::id())]);
    eprintln!("measure() took {:?}", t.elapsed());
    assert_eq!(m.procs.len(), 1);
}

#[test]
#[cfg(unix)]
fn ps_table_strips_the_login_shell_dash() {
    let table = crate::sys::snapshot_table();
    assert!(
        !table.names.values().any(|n| n.starts_with('-')),
        "login shells must read as zsh, not -zsh"
    );
}
