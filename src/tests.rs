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

#[test]
fn config_roundtrip() {
    let c = Config {
        projects: vec![Project {
            name: "demo".into(),
            path: PathBuf::from("demo"),
            command: shell(),
            kind: Kind::Agent,
            autostart: false,
            ledger: false,
        }],
        scrollback: None,
        ..Default::default()
    };
    let decoded: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
    assert_eq!(decoded.projects[0].name, "demo");
}
#[test]
fn kind_defaults_to_agent() {
    let c: Config =
        toml::from_str("[[projects]]\nname = \"demo\"\npath = \"demo\"\ncommand = [\"sh\"]")
            .unwrap();
    assert_eq!(c.projects[0].kind, Kind::Agent);
}
#[test]
fn kind_parses_service() {
    let c: Config = toml::from_str("[[projects]]\nname = \"api\"\npath = \"api\"\ncommand = [\"node\", \"server.js\"]\nkind = \"service\"").unwrap();
    assert_eq!(c.projects[0].kind, Kind::Service);
}
#[test]
fn kind_from_str() {
    assert_eq!("Service".parse::<Kind>().unwrap(), Kind::Service);
    assert!("bot".parse::<Kind>().is_err());
}
#[test]
fn order_groups_agents_first() {
    let mk = |kind| Project {
        name: "x".into(),
        path: PathBuf::from("x"),
        command: vec!["sh".into()],
        kind,
        autostart: false,
        ledger: false,
    };
    let ps = vec![mk(Kind::Service), mk(Kind::Agent), mk(Kind::Service)];
    assert_eq!(display_order(&ps), vec![1, 0, 2]);
}
#[test]
fn status_colors() {
    assert_eq!(status_style("Running"), Style::default().fg(Color::Green));
    assert_eq!(
        status_style("Exited (0)"),
        Style::default().fg(Color::DarkGray)
    );
    assert_eq!(status_style("Exited (1)"), Style::default().fg(Color::Red));
    assert_eq!(
        status_style("Exited (137)"),
        Style::default().fg(Color::Red)
    );
    assert_eq!(status_style("Error: boom"), Style::default().fg(Color::Red));
    assert_eq!(
        status_style("Stopped"),
        Style::default().fg(Color::DarkGray)
    );
}
#[test]
fn cli_help_covers_commands() {
    let h = cli_help();
    assert!(h.contains(env!("CARGO_PKG_VERSION")));
    for c in [
        "squad add",
        "squad list",
        "squad remove",
        "squad start",
        "--version",
        "Manager keys",
        "Terminal keys",
        "s  stop",
        "d/Del  remove",
        "Ctrl+Q  back to manager",
    ] {
        assert!(h.contains(c), "help mentions {c}");
    }
}
#[test]
fn help_covers_keys() {
    let h = help_lines().join("\n");
    for k in [
        "Enter", "/", "j/k", "filter", "restart", "add", "scroll", "1-9", "stop", "d/Del", "q",
        "?", "Ctrl+Q",
    ] {
        assert!(h.contains(k), "help missing {k}");
    }
}
#[test]
fn mode_chip_and_hints() {
    assert!(mode_hints(Mode::Manager).contains("Enter"));
    assert!(mode_hints(Mode::Send).contains("Esc"));
    let chips: Vec<&str> = [
        Mode::Manager,
        Mode::Focus,
        Mode::Send,
        Mode::Filter,
        Mode::Add,
        Mode::Help,
    ]
    .iter()
    .map(|m| mode_chip(*m).0.trim())
    .collect();
    assert_eq!(
        chips,
        vec!["MANAGER", "FOCUS", "SEND", "FILTER", "ADD", "HELP"]
    );
}
#[test]
fn mode_chips_have_solid_backgrounds() {
    for m in [
        Mode::Manager,
        Mode::Focus,
        Mode::Send,
        Mode::Filter,
        Mode::Add,
        Mode::Help,
    ] {
        let style = mode_chip(m).1;
        assert!(style.bg.is_some(), "{m:?} chip needs a background");
        assert!(style.fg.is_some(), "{m:?} chip needs a foreground");
    }
}
#[test]
fn empty_state_guides_first_run() {
    let fresh = empty_state_lines(false).join("\n");
    assert!(fresh.contains("No projects"));
    assert!(fresh.contains("squad add"));
    let idle = empty_state_lines(true).join("\n");
    assert!(idle.contains("Enter"));
    assert!(!idle.contains("squad add"));
}
#[test]
fn spinner_cycles() {
    assert_eq!(
        (0..8).map(spinner_frame).collect::<Vec<char>>(),
        vec!['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧']
    );
    assert_eq!(status_glyph("Running", 0), '⠋');
    assert_eq!(status_glyph("Running", 99), spinner_frame(99));
    assert_eq!(status_glyph("Stopped", 99), '●');
    assert_eq!(status_glyph("Exited (0)", 99), '●');
    assert_eq!(status_glyph("Error: boom", 99), '●');
}
#[test]
fn agent_phase_badge_and_pane_display() {
    assert_eq!(AgentPhase::Working.icon(0).0, "⠋");
    assert_eq!(AgentPhase::NeedsInput.icon(0).0, "?");
    assert_eq!(AgentPhase::Done.icon(0).0, "✔");
    assert_eq!(AgentPhase::Idle.icon(0).0, "○");

    let (text, _) = pane_status_display(
        "Running",
        Some(12),
        0,
        Kind::Agent,
        Some(AgentPhase::Working),
    );
    assert_eq!(text, "⠋ Engaged 12s");

    let (text, _) = pane_status_display(
        "Running",
        Some(5),
        0,
        Kind::Agent,
        Some(AgentPhase::NeedsInput),
    );
    assert_eq!(text, "? Awaiting orders 5s");

    let (text, _) =
        pane_status_display("Running", Some(42), 0, Kind::Agent, Some(AgentPhase::Done));
    assert_eq!(text, "✔ Task complete 42s");

    let (text, _) = pane_status_display("Running", Some(0), 0, Kind::Agent, Some(AgentPhase::Idle));
    assert_eq!(text, "○ Standby 0s");

    let (text, _) = pane_status_display("Running", Some(10), 0, Kind::Service, None);
    assert_eq!(text, "⠋ 10s");

    let (text, _) = pane_status_display("Stopped", None, 0, Kind::Agent, None);
    assert_eq!(text, "● Stopped");
}
#[test]
fn pane_title_name_prefers_service_command() {
    let agent = Project {
        name: "my-agent".into(),
        path: PathBuf::from("."),
        command: vec!["cargo".into(), "run".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    assert_eq!(pane_title_name(&agent), "my-agent");

    let service = Project {
        name: "backend".into(),
        path: PathBuf::from("."),
        command: vec!["npm".into(), "run".into(), "dev".into()],
        kind: Kind::Service,
        autostart: false,
        ledger: false,
    };
    assert_eq!(pane_title_name(&service), "npm run dev");
}
#[test]
fn agent_needs_input_and_working_detection() {
    assert!(is_needs_input("Would you like to execute this tool? [y/n]"));
    assert!(is_needs_input("Permission to read /etc/hosts? (yes/no)"));
    assert!(is_needs_input("Allow tool execution?"));
    assert!(is_needs_input(
        "Do you want to proceed?\n ❯ 1. Yes\n   Esc to cancel"
    ));
    assert!(!is_needs_input("All tests passed with 0 errors."));

    assert!(is_working_text("Thinking about next refactor step..."));
    assert!(is_working_text("Executing command in background"));
    assert!(is_working_text("Searching codebase for usages"));
    assert!(is_working_text("✶ Clauding… (esc to interrupt)"));
    assert!(!is_working_text("Finished compilation successfully."));
}
#[test]
fn confirm_start_all_modal_formatting() {
    assert_eq!(confirm_start_all_prompt(3), "Start all 3 sessions?");
    let lines = confirm_start_all_lines(3);
    assert!(lines[0]
        .spans
        .iter()
        .any(|s| s.content.contains("Start all 3 sessions?")));
    assert!(lines[2].spans.iter().any(|s| s.content.contains("starts")));
}
#[test]
fn modal_backdrop_dim() {
    use ratatui::buffer::Buffer;
    let mut buf = Buffer::empty(ratatui::layout::Rect::new(0, 0, 10, 5));
    buf.set_string(0, 0, "hello", Style::default());
    assert!(!buf[(0, 0)]
        .style()
        .add_modifier
        .contains(ratatui::style::Modifier::DIM));
    dim_area(&mut buf, ratatui::layout::Rect::new(0, 0, 10, 5));
    assert!(buf[(0, 0)]
        .style()
        .add_modifier
        .contains(ratatui::style::Modifier::DIM));
}
#[test]
fn scrollback_view() {
    let mut p = vt100::Parser::new(NonZero::new(4).unwrap(), NonZero::new(20).unwrap(), 10);
    for i in 0..7 {
        p.process(format!("line{i}\r\n").as_bytes());
    }
    assert!(p.screen().scrollback_len() > 0);
    p.screen_mut().set_scrollback(2);
    assert_eq!(p.screen().scrollback(), 2);
    let top: String = p.screen().contents();
    assert!(top.contains("line"));
    p.screen_mut().set_scrollback(0);
    assert_eq!(p.screen().scrollback(), 0);
}
#[test]
fn alternate_screen_mouse_detection() {
    let mut p = vt100::Parser::new(NonZero::new(24).unwrap(), NonZero::new(80).unwrap(), 100);
    assert!(!p.screen().alternate_screen());
    assert_eq!(
        p.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::None
    );

    // Switch to alternate screen: \x1b[?1049h
    p.process(b"\x1b[?1049h");
    assert!(p.screen().alternate_screen());

    // Enable SGR mouse tracking: \x1b[?1000h\x1b[?1006h
    p.process(b"\x1b[?1000h\x1b[?1006h");
    assert_ne!(
        p.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::None
    );
    assert_eq!(
        p.screen().mouse_protocol_encoding(),
        vt100::MouseProtocolEncoding::Sgr
    );
}
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
#[test]
#[cfg(windows)]
fn stop_kills_process_tree() {
    let p = Project {
        name: "treekill".into(),
        path: env::current_dir().unwrap(),
        command: vec![
            "cmd.exe".into(),
            "/d".into(),
            "/c".into(),
            "timeout".into(),
            "/t".into(),
            "30".into(),
            "/nobreak".into(),
        ],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    let s = Session::spawn(&p, 24, 80).unwrap();
    let pid = s.child.process_id().expect("pty child pid");
    let mut grandkids = Vec::new();
    for _ in 0..50 {
        grandkids = child_pids(pid);
        if !grandkids.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(!grandkids.is_empty(), "grandchild never appeared");
    drop(s);
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(100));
        if !pid_alive(pid) && grandkids.iter().all(|g| !pid_alive(*g)) {
            return;
        }
    }
    panic!(
        "process tree survived stop: parent alive={} kids={:?}",
        pid_alive(pid),
        grandkids
            .iter()
            .map(|g| (*g, pid_alive(*g)))
            .collect::<Vec<_>>()
    );
}
#[test]
#[cfg(windows)]
fn stop_kills_orphaned_grandchildren() {
    // A `start`ed grandchild outlives its wrapper: the direct child is
    // already dead when the session drops, yet the orphan must still die.
    let p = Project {
        name: "orphankill".into(),
        path: env::current_dir().unwrap(),
        command: vec![
            "cmd.exe".into(),
            "/d".into(),
            "/c".into(),
            "start".into(),
            "/min".into(),
            "ping".into(),
            // Long-lived on purpose: the stop poll below stretches over
            // tens of seconds (one powershell probe per PID per round),
            // so a short ping would die naturally and fake a pass.
            "-n".into(),
            "300".into(),
            "127.0.0.1".into(),
        ],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    let mut s = Session::spawn(&p, 24, 80).unwrap();
    let pid = s.child.process_id().expect("pty child pid");
    let mut orphans: Vec<u32> = Vec::new();
    for _ in 0..100 {
        let wrapper_dead = s.child.try_wait().ok().flatten().is_some();
        orphans = descendant_pids(pid)
            .into_iter()
            .filter(|p| *p != pid)
            .collect();
        if wrapper_dead && !orphans.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(!orphans.is_empty(), "orphan never appeared");
    drop(s);
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(100));
        if orphans.iter().all(|g| !pid_alive(*g)) {
            return;
        }
    }
    panic!(
        "orphaned grandchild survived stop: {:?}",
        orphans
            .iter()
            .map(|g| (*g, pid_alive(*g)))
            .collect::<Vec<_>>()
    );
}
#[test]
fn jump_keys() {
    assert_eq!(jump_to(5, '1'), Some(0));
    assert_eq!(jump_to(5, '5'), Some(4));
    assert_eq!(jump_to(5, '6'), None);
    assert_eq!(jump_to(0, '1'), None);
    assert_eq!(jump_to(12, '9'), Some(8));
    assert_eq!(jump_to(12, '0'), None);
    assert_eq!(jump_to(3, 'x'), None);
}
#[test]
fn build_project_validates() {
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_string).collect() };
    let dir = env::temp_dir();
    let dir_str = dir.to_str().unwrap();
    let p = build_project(&[], dir_str, Kind::Service, parts("node server.js")).unwrap();
    assert_eq!(p.kind, Kind::Service);
    assert_eq!(p.name, "node");
    assert_eq!(p.command, vec!["node".to_string(), "server.js".to_string()]);
    // Duplicate agent with same directory is rejected:
    let agent = build_project(&[], dir_str, Kind::Agent, parts("")).unwrap();
    let existing = vec![agent];
    assert!(build_project(&existing, dir_str, Kind::Agent, parts("")).is_err());
    assert!(build_project(&[], "", Kind::Agent, parts("")).is_err());
    assert!(build_project(&[], "C:/definitely/not/here/xyz", Kind::Agent, parts("")).is_err());
    let shell_p = build_project(&[], dir_str, Kind::Agent, parts("   ")).unwrap();
    assert_eq!(shell_p.command, shell());
    // Quoted path (e.g. from Windows "Copy as path") is accepted cleanly:
    let quoted_dir = format!("\"{dir_str}\"");
    let qp = build_project(&[], &quoted_dir, Kind::Agent, parts("")).unwrap();
    assert_eq!(qp.path, shell_p.path);
}
#[test]
fn services_allow_multiple_in_same_directory() {
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_string).collect() };
    let dir = env::temp_dir();
    let dir_str = dir.to_str().unwrap();
    let s1 = build_project(&[], dir_str, Kind::Service, parts("npm run dev")).unwrap();
    assert_eq!(s1.name, "npm");
    assert_eq!(s1.path, plain_path(fs::canonicalize(&dir).unwrap()));
    let existing = [s1.clone()];
    let s2 = build_project(&existing, dir_str, Kind::Service, parts("npm run server")).unwrap();
    assert_eq!(s2.name, "npm-2");
    assert_eq!(s2.path, s1.path);
    let existing_both = [s1.clone(), s2];
    let s3 = build_project(
        &existing_both,
        dir_str,
        Kind::Service,
        parts("python app.py"),
    )
    .unwrap();
    assert_eq!(s3.name, "python");
    assert_eq!(s3.path, s1.path);
}
#[test]
fn service_without_path_uses_current_dir() {
    // Services may skip the path: they run where squad runs, and the
    // project takes its name from the program being served.
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_string).collect() };
    let p = build_project(&[], "", Kind::Service, parts("node server.js")).unwrap();
    assert_eq!(p.kind, Kind::Service);
    assert_eq!(p.name, "node");
    assert_eq!(p.command, vec!["node".to_string(), "server.js".to_string()]);
    assert_eq!(
        p.path,
        plain_path(fs::canonicalize(env::current_dir().unwrap()).unwrap())
    );
    assert!(!p.path.to_string_lossy().starts_with(r"\\?\"));
    // An .exe program names the project without its extension.
    let q = build_project(&[], "", Kind::Service, parts("ollama.exe serve")).unwrap();
    assert_eq!(q.name, "ollama");
    // Agents still require a path.
    assert!(build_project(&[], "", Kind::Agent, parts("claude")).is_err());
}
#[test]
fn filter_matches_name_and_command() {
    let mk = |name: &str, cmd: &[&str], kind| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        kind,
        autostart: false,
        ledger: false,
    };
    let ps = vec![
        mk("api", &["node", "server.js"], Kind::Service),
        mk("web", &["npm", "run", "dev"], Kind::Agent),
        mk("agent-alpha", &["claude"], Kind::Agent),
    ];
    assert_eq!(apply_filter(&ps, ""), vec![1, 2, 0]);
    assert_eq!(apply_filter(&ps, "api"), vec![0]);
    assert_eq!(apply_filter(&ps, "node"), vec![0]);
    assert_eq!(apply_filter(&ps, "AGENT"), vec![2]);
    assert!(apply_filter(&ps, "zzz").is_empty());
}
#[test]
fn keyboard() {
    assert_eq!(
        key_bytes(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            false
        ),
        vec![3]
    );
    assert_eq!(
        key_bytes(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true),
        b"\x1bOA"
    );
}
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
#[cfg(windows)]
fn probes_read_real_numbers() {
    let (times, (total, avail)) = system_snapshot().expect("system snapshot");
    assert!(total > 0 && avail <= total);
    assert!(times.1 + times.2 > 0);
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
#[cfg(windows)]
fn session_sampling_tracks_a_live_tree() {
    let p = Project {
        name: "usage".into(),
        path: env::current_dir().unwrap(),
        command: vec![
            "cmd.exe".into(),
            "/d".into(),
            "/c".into(),
            "timeout".into(),
            "/t".into(),
            "20".into(),
            "/nobreak".into(),
        ],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    let mut s = Session::spawn(&p, 24, 80).unwrap();
    let pid = s.child.process_id().expect("pid");
    assert!(s.usage.is_none());
    let first = measure(&[(0, pid)]);
    let (_, _, cpu, mem) = first.procs[0];
    s.apply_usage(pid, cpu, mem, first.at, 4);
    let u = s.usage.expect("running session reports usage");
    assert!(u.mem > 0);
    assert_eq!(u.cpu, 0.0, "first sample has no baseline");
    let second = measure(&[(0, pid)]);
    let (_, _, cpu, mem) = second.procs[0];
    s.apply_usage(pid, cpu, mem, second.at, 4);
    assert!(s.usage.unwrap().cpu >= 0.0);
    // A result for some other process (restarted session) is ignored.
    let before = s.usage;
    s.apply_usage(pid + 1, 1, 1, Instant::now(), 4);
    assert_eq!(s.usage, before);
    // A finished session stops reporting.
    s.status = SessionStatus::Exited(0);
    s.clear_usage();
    assert!(s.usage.is_none());
}
#[test]
fn hints_and_help_stay_in_sync() {
    // Every key the status bar advertises is documented in the help
    // popup and the CLI help, and the two hint tables list the same keys.
    let help = help_lines().join("\n");
    let cli = cli_help();
    let mut a: Vec<&str> = mode_hint_parts(Mode::Manager).iter().map(|p| p.0).collect();
    let mut b: Vec<&str> = priority_hint_parts(Mode::Manager)
        .iter()
        .map(|p| p.0)
        .collect();
    for key in &a {
        assert!(help.contains(key), "help popup missing {key}");
        assert!(cli.contains(key), "CLI help missing {key}");
    }
    a.sort();
    b.sort();
    assert_eq!(a, b, "hint tables disagree");
}
#[test]
fn fixed_rect_fits_short_terminals() {
    use ratatui::layout::Rect;
    let r = fixed_rect(66, 18, Rect::new(0, 0, 100, 24));
    assert_eq!((r.width, r.height, r.x, r.y), (66, 18, 17, 3));
    // Clamped, never larger than the screen.
    let r = fixed_rect(66, 18, Rect::new(0, 0, 40, 10));
    assert_eq!((r.width, r.height), (40, 10));
    // Every help row fits the popup interior on a 24-line terminal.
    assert!(help_lines().len() as u16 + 2 <= 24);
}
#[test]
fn sidebar_click_maps_rows() {
    use ratatui::layout::Rect;
    let side = Rect::new(0, 0, 30, 10);
    // header, two projects, header, (none)
    let t = [None, Some(0), Some(1), None, None];
    assert_eq!(sidebar_click(side, 0, &t, 5, 2), Some(0));
    assert_eq!(sidebar_click(side, 0, &t, 5, 3), Some(1));
    assert_eq!(sidebar_click(side, 0, &t, 5, 1), None, "header");
    assert_eq!(sidebar_click(side, 0, &t, 5, 5), None, "placeholder");
    assert_eq!(sidebar_click(side, 0, &t, 5, 0), None, "top border");
    assert_eq!(sidebar_click(side, 0, &t, 0, 2), None, "left border");
    assert_eq!(sidebar_click(side, 0, &t, 29, 2), None, "right border");
    assert_eq!(sidebar_click(side, 0, &t, 5, 9), None, "bottom border");
    // A scrolled list shifts the mapping by the offset.
    assert_eq!(sidebar_click(side, 1, &t, 5, 1), Some(0));
    assert_eq!(sidebar_click(side, 0, &t, 40, 2), None, "outside");
}
#[test]
fn plain_path_strips_verbatim_prefix() {
    assert_eq!(
        plain_path(PathBuf::from(r"\\?\C:\work\app")),
        PathBuf::from(r"C:\work\app")
    );
    // UNC shares and ordinary paths are left alone.
    let unc = PathBuf::from(r"\\?\UNC\srv\share");
    assert_eq!(plain_path(unc.clone()), unc);
    assert_eq!(plain_path(PathBuf::from("rel")), PathBuf::from("rel"));
}
#[test]
fn load_from_normalizes_stored_verbatim_paths() {
    let dir = env::temp_dir().join(format!("squad-test-norm-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("config.toml");
    let toml_content = r#"
[[projects]]
name = "legacy"
path = "\\\\?\\C:\\work\\app"
command = ["cmd.exe"]
kind = "agent"

[[projects]]
name = "cmd"
path = "C:\\Users\\kornah"
command = ["cmd.exe"]
kind = "service"
"#;
    fs::write(&path, toml_content).unwrap();
    let loaded = load_from(&path).unwrap();
    assert_eq!(loaded.projects[0].path, PathBuf::from(r"C:\work\app"));
    if cfg!(windows) {
        assert_eq!(loaded.projects[0].command, shell());
        assert_eq!(loaded.projects[1].command, shell());
        assert_eq!(loaded.projects[1].name, "kornah");
    }
    let _ = fs::remove_dir_all(&dir);
}
#[test]
fn split_command_honors_quotes() {
    assert_eq!(split_command("node  server.js"), ["node", "server.js"]);
    assert_eq!(
        split_command(r#""C:\Program Files\x.exe" --flag "a b""#),
        [r"C:\Program Files\x.exe", "--flag", "a b"]
    );
    assert_eq!(split_command(r#"echo """#), ["echo", ""]);
    assert!(split_command("   ").is_empty());
}
#[test]
fn function_keys_encode_xterm() {
    let f = |n, m| key_bytes(KeyEvent::new(KeyCode::F(n), m), false);
    let none = KeyModifiers::NONE;
    assert_eq!(f(1, none), b"\x1bOP");
    assert_eq!(f(4, none), b"\x1bOS");
    assert_eq!(f(5, none), b"\x1b[15~");
    assert_eq!(f(10, none), b"\x1b[21~");
    assert_eq!(f(12, none), b"\x1b[24~");
    assert_eq!(f(1, KeyModifiers::SHIFT), b"\x1b[1;2P");
    assert_eq!(f(5, KeyModifiers::CONTROL), b"\x1b[15;5~");
}
#[test]
fn persist_helpers_keep_other_writers_projects() {
    let file = env::temp_dir().join(format!("agentsquad-persist-{}.toml", std::process::id()));
    let mk = |name: &str| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: vec!["sh".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    // Single-threaded section: AGENTSQUAD_CONFIG is process-global, and
    // no other test reads it.
    env::set_var("AGENTSQUAD_CONFIG", &file);
    save_to(
        &Config {
            projects: vec![mk("a"), mk("b")],
            scrollback: None,
            ..Default::default()
        },
        &file,
    )
    .unwrap();
    // Another process registers "c" after we loaded; our remove of "a"
    // and add of "d" must keep it.
    let mut other = load_from(&file).unwrap();
    other.projects.push(mk("c"));
    save_to(&other, &file).unwrap();
    persist_remove("A").unwrap();
    persist_add(&mk("d")).unwrap();
    persist_add(&mk("D")).unwrap();
    let names: Vec<String> = load_from(&file)
        .unwrap()
        .projects
        .into_iter()
        .map(|p| p.name)
        .collect();
    env::remove_var("AGENTSQUAD_CONFIG");
    std::fs::remove_file(&file).ok();
    assert_eq!(names, ["b", "c", "d"]);
}
#[test]
fn version_comparison() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(parse_version("v0.5.4"), Some((0, 5, 4)));
    assert_eq!(parse_version("0.5.4\n"), Some((0, 5, 4)));
    assert_eq!(parse_version("v1.2"), None);
    assert_eq!(parse_version("v1.2.3.4"), None);
    assert_eq!(parse_version("v1.2.3-rc1"), None);
    assert_eq!(parse_version("latest"), None);
    assert!(is_newer("0.5.3", "v0.5.4"));
    assert!(is_newer("0.5.9", "v0.6.0"));
    assert!(is_newer("0.5.3", "v0.10.0"), "numeric, not textual");
    assert!(!is_newer("0.5.3", "v0.5.3"), "same version");
    assert!(!is_newer("0.5.3", "v0.5.2"), "never downgrade");
    assert!(!is_newer("0.5.3", "garbage"), "unparseable tag is ignored");
}
#[test]
fn update_prompt_accepts_only_y() {
    assert_eq!(update_key(KeyCode::Char('y')), Some(true));
    assert_eq!(update_key(KeyCode::Enter), None, "Enter must not update");
    assert_eq!(update_key(KeyCode::Char(' ')), None);
    assert_eq!(update_key(KeyCode::Esc), Some(false));
    assert_eq!(update_key(KeyCode::Char('n')), Some(false));
    let text: String = confirm_update_lines("v9.9.9")
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(text.contains("v9.9.9") && text.contains(env!("CARGO_PKG_VERSION")));
}
#[test]
fn swap_binary_replaces_and_rolls_back() {
    let dir = env::temp_dir().join(format!("squad-swap-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (cur, new) = (dir.join("squad.exe"), dir.join("new.exe"));
    fs::write(&cur, "old build").unwrap();
    fs::write(&new, "new build").unwrap();
    swap_binary(&new, &cur).unwrap();
    assert_eq!(fs::read_to_string(&cur).unwrap(), "new build");
    let old = format!("squad.exe.old-{}", std::process::id());
    assert_eq!(fs::read_to_string(dir.join(&old)).unwrap(), "old build");
    // A failed copy puts the previous build back.
    fs::remove_file(dir.join(&old)).unwrap();
    assert!(swap_binary(&dir.join("missing.exe"), &cur).is_err());
    assert_eq!(fs::read_to_string(&cur).unwrap(), "new build");
    fs::remove_dir_all(&dir).ok();
}
#[test]
#[ignore = "needs network"]
fn fetch_release_downloads_and_verifies() {
    // The real pipeline minus the swap: curl, bsdtar, `--version` check.
    let work = env::temp_dir().join(format!("squad-fetch-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();
    let exe = fetch_release("v0.5.3", &work).expect("download and verify v0.5.3");
    assert!(exe.is_file());
    // A tag whose binary reports another version is refused.
    let wrong = fetch_release("v0.5.2", &work);
    assert!(wrong.is_ok(), "v0.5.2 reports itself as 0.5.2");
    assert!(fetch_release("v0.0.1", &work).is_err(), "no such release");
    fs::remove_dir_all(&work).ok();
}
#[test]
#[ignore = "needs network"]
fn latest_tag_reads_github() {
    let tag = latest_tag().expect("curl and network available");
    assert!(parse_version(&tag).is_some(), "got {tag}");
}
#[test]
fn only_ctrl_q_leaves_terminal_focus() {
    use crossterm::event::KeyModifiers as M;
    assert!(focus_exit_key(KeyCode::Char('q'), M::CONTROL));
    // Esc and Ctrl+G are the agent's, not ours.
    assert!(!focus_exit_key(KeyCode::Esc, M::NONE));
    assert!(!focus_exit_key(KeyCode::Char('g'), M::CONTROL));
    assert!(!focus_exit_key(KeyCode::Char('q'), M::NONE));
    assert!(!focus_exit_key(KeyCode::Char('q'), M::CONTROL | M::SHIFT));
    assert_eq!(key_bytes(KeyEvent::new(KeyCode::Esc, M::NONE), false), [27]);
    assert_eq!(
        key_bytes(KeyEvent::new(KeyCode::Char('g'), M::CONTROL), false),
        [7]
    );
}
#[test]
fn key_bytes_encodes_modifiers() {
    use crossterm::event::KeyModifiers as M;
    let ev = |code, mods| KeyEvent::new(code, mods);
    assert_eq!(key_bytes(ev(KeyCode::Left, M::NONE), false), b"\x1b[D");
    assert_eq!(key_bytes(ev(KeyCode::Left, M::SHIFT), false), b"\x1b[1;2D");
    assert_eq!(key_bytes(ev(KeyCode::Up, M::CONTROL), true), b"\x1b[1;5A");
    assert_eq!(key_bytes(ev(KeyCode::Home, M::SHIFT), false), b"\x1b[1;2H");
    assert_eq!(
        key_bytes(ev(KeyCode::Delete, M::CONTROL), false),
        b"\x1b[3;5~"
    );
    // Alt keeps the ESC-prefix form on top of the base sequence.
    assert_eq!(key_bytes(ev(KeyCode::Up, M::ALT), false), b"\x1b\x1b[A");
}
#[test]
fn terminal_parser() {
    let mut p = vt100::Parser::new(NonZero::new(10).unwrap(), NonZero::new(40).unwrap(), 50);
    p.process(b"hello\r\nworld");
    assert!(p.screen().contents().contains("world"));
}
#[test]
fn pty_smoke() {
    let p = Project {
        name: "smoke".into(),
        path: env::current_dir().unwrap(),
        command: if cfg!(windows) {
            vec![
                "cmd.exe".into(),
                "/d".into(),
                "/c".into(),
                "echo AGENTSQUAD_OK".into(),
            ]
        } else {
            vec!["sh".into(), "-c".into(), "echo AGENTSQUAD_OK".into()]
        },
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    let mut s = Session::spawn(&p, 24, 80).unwrap();
    for _ in 0..100 {
        s.update();
        if s.parser.screen().contents().contains("AGENTSQUAD_OK") {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "PTY output not received; status={} screen={:?}",
        s.status,
        s.parser.screen().contents()
    );
}
#[test]
fn line_input_editing() {
    let mut l = LineInput::default();
    for ch in "hello".chars() {
        l.insert(ch);
    }
    assert_eq!(l.as_str(), "hello");
    l.move_left();
    l.move_left();
    l.insert('X');
    assert_eq!(l.as_str(), "helXlo");
    l.backspace();
    assert_eq!(l.as_str(), "hello");
    l.delete();
    assert_eq!(l.as_str(), "helo");
    l.move_home();
    l.insert('S');
    assert_eq!(l.as_str(), "Shelo");
    l.move_end();
    l.insert('!');
    assert_eq!(l.as_str(), "Shelo!");
    l.clear();
    assert!(l.is_empty());
    assert_eq!(l.as_str(), "");
}
#[test]
fn line_input_cursor_parts() {
    let mut l = LineInput::default();
    for ch in "ab".chars() {
        l.insert(ch);
    }
    // Cursor at end: cursor block shows a space.
    assert_eq!(l.render_parts(), ("ab".to_string(), ' ', String::new()));
    l.move_left();
    assert_eq!(l.render_parts(), ("a".to_string(), 'b', String::new()));
    l.move_home();
    assert_eq!(l.render_parts(), (String::new(), 'a', "b".to_string()));
}
#[test]
fn stop_asks_to_confirm() {
    let yes = |c: KeyCode| confirm_key(c);
    assert_eq!(yes(KeyCode::Char('y')), Some(true));
    assert_eq!(yes(KeyCode::Char('Y')), Some(true));
    assert_eq!(yes(KeyCode::Enter), Some(true));
    assert_eq!(yes(KeyCode::Esc), Some(false));
    assert_eq!(yes(KeyCode::Char('n')), Some(false));
    assert_eq!(yes(KeyCode::Char('N')), Some(false));
    assert_eq!(yes(KeyCode::Char('q')), None);
    let prompt = confirm_stop_prompt("web");
    assert!(prompt.contains("web"), "prompt names the session");
}
#[test]
fn remove_unregisters_project() {
    // `Delete` removes the selected project with confirmation, whether
    // or not it holds a session.
    let mk = |name: &str| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: vec!["sh".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    let mut projects = vec![mk("a"), mk("b")];
    let mut slots: Vec<Option<u8>> = vec![None, Some(1)];
    assert_eq!(
        remove_project(&mut projects, &mut slots, 1).map(|p| p.name),
        Some("b".to_string())
    );
    assert_eq!(projects.len(), 1);
    assert_eq!(slots.len(), 1);
    assert!(remove_project(&mut projects, &mut slots, 5).is_none());
    assert_eq!(projects.len(), 1);
    let text: String = confirm_remove_lines("b")
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(text.contains('b'), "remove modal names the project");
    assert!(text.contains('y') && text.contains("removes"));
}
#[test]
fn remove_persists_to_config_file() {
    // A TUI removal must survive a restart: write with the real saver,
    // read back with the real loader, and confirm the project is gone.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let file = std::env::temp_dir().join(format!(
        "agentsquad-remove-test-{}-{}.toml",
        std::process::id(),
        n
    ));
    let mk = |name: &str| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: vec!["sh".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    let config = Config {
        projects: vec![mk("a"), mk("b")],
        scrollback: None,
        ..Default::default()
    };
    save_to(&config, &file).unwrap();
    let mut loaded = load_from(&file).unwrap();
    assert_eq!(loaded.projects.len(), 2);
    let mut slots: Vec<Option<u8>> = vec![None, None];
    let removed = remove_project(&mut loaded.projects, &mut slots, 0).unwrap();
    assert_eq!(removed.name, "a");
    save_to(&loaded, &file).unwrap();
    let reread = load_from(&file).unwrap();
    assert_eq!(reread.projects.len(), 1);
    assert_eq!(reread.projects[0].name, "b");
    std::fs::remove_file(&file).ok();
}
#[test]
fn stop_selects_live_session() {
    // `s` stops the selected project, but only while it actually
    // holds a session.
    let order = vec![0usize, 1, 2];
    let live: Vec<Option<u8>> = vec![None, Some(1), None];
    assert_eq!(stop_target(&order, 1, &live), Some(1));
    assert_eq!(stop_target(&order, 0, &live), None);
    assert_eq!(stop_target(&order, 5, &live), None);
    assert_eq!(stop_target(&[], 0, &live), None);
}
#[test]
fn pty_env_advertises_truecolor() {
    let env = pty_env();
    assert!(env.contains(&("TERM", "xterm-256color")));
    assert!(env.contains(&("COLORTERM", "truecolor")));
}
#[test]
fn exit_notice_fires_once() {
    let mut announced = false;
    assert_eq!(
        exit_notice("web", true, "Exited (1)", &mut announced),
        Some("Session 'web' Exited (1); Enter restarts it".to_string())
    );
    assert!(announced);
    assert_eq!(
        exit_notice("web", false, "Exited (1)", &mut announced),
        None
    );
    let mut fresh = false;
    assert_eq!(exit_notice("web", true, "Running", &mut fresh), None);
    assert!(!fresh);
    assert_eq!(
        exit_notice("api", true, "Error: boom", &mut fresh),
        Some("Session 'api' Error: boom; Enter restarts it".to_string())
    );
}
#[test]
fn split_help_line_columns() {
    assert_eq!(
        split_help_line("Enter      start it"),
        ("Enter", "start it")
    );
    assert_eq!(split_help_line("no-split"), ("no-split", ""));
    assert_eq!(split_help_line("a   b  c"), ("a", "b  c"));
}
#[test]
fn list_table_aligns_columns() {
    let mk = |name: &str, kind: Kind, cmd: &[&str]| Project {
        name: name.into(),
        path: PathBuf::from("/tmp"),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        kind,
        autostart: false,
        ledger: false,
    };
    // Registration order is service-first; output is agents-first.
    let projects = vec![
        mk("api", Kind::Service, &["node", "server.js"]),
        mk("web", Kind::Agent, &["claude"]),
    ];
    assert_eq!(
        list_table(&projects, false),
        vec![
            "web  agent    /tmp  claude".to_string(),
            "api  service  /tmp  node server.js".to_string(),
        ]
    );
    assert!(list_table(&[], false).is_empty());
    let colored = list_table(&projects, true).join("\n");
    assert!(colored.contains("agent"), "kind text stays readable");
    assert!(
        colored.contains("\x1b["),
        "terminal output carries ANSI colors"
    );
}
#[test]
fn keep_visible_tracks_selection() {
    assert_eq!(keep_visible(0, 0, 10), 0);
    assert_eq!(keep_visible(0, 9, 10), 0);
    assert_eq!(keep_visible(0, 10, 10), 1);
    assert_eq!(keep_visible(0, 15, 10), 6);
    assert_eq!(keep_visible(5, 8, 10), 5);
    assert_eq!(keep_visible(5, 3, 10), 3);
    assert_eq!(keep_visible(5, 5, 10), 5);
    assert_eq!(keep_visible(0, 5, 0), 0);
}
#[test]
fn row_name_style_marks_kind() {
    use ratatui::style::Modifier as M;
    assert_eq!(
        row_name_style(true, "Running", Kind::Agent),
        selected_row_style()
    );
    assert_eq!(
        row_name_style(false, "Running", Kind::Service),
        Style::default().fg(Color::Magenta).add_modifier(M::BOLD)
    );
    assert_eq!(
        row_name_style(false, "Stopped", Kind::Agent),
        Style::default().fg(Color::Cyan).add_modifier(M::DIM)
    );
    assert_eq!(
        row_name_style(false, "Exited (1)", Kind::Service),
        Style::default().fg(Color::Magenta).add_modifier(M::DIM)
    );
}
#[test]
fn sidebar_width_scales() {
    assert_eq!(sidebar_width(47), 0);
    assert_eq!(sidebar_width(48), 24);
    assert_eq!(sidebar_width(60), 24);
    assert_eq!(sidebar_width(100), 25);
    assert_eq!(sidebar_width(152), 38);
    assert_eq!(sidebar_width(300), 38);
}
#[test]
fn hint_text_matches_legacy_strings() {
    assert_eq!(
            mode_hints(Mode::Manager),
            "Enter open | j/k select | / filter | a add | m message | s stop | r restart | d/Del remove | S start all | q quit | ? help"
        );
    assert_eq!(
        mode_hints(Mode::Focus),
        "Ctrl+Q manager | Alt+1-9 switch | other keys go to the session"
    );
    assert_eq!(mode_hints(Mode::Send), "Enter sends | Esc cancels");
    assert_eq!(
        mode_hints(Mode::Filter),
        "type to narrow | Enter keeps | Esc clears"
    );
    assert_eq!(
        mode_hints(Mode::Add),
        "Tab next | Space toggle | Enter save | Esc cancel"
    );
    assert_eq!(mode_hints(Mode::Help), "?/Esc closes");
}
#[test]
fn fitted_hints_compact_on_narrow() {
    let text = |spans: Vec<Line>| {
        spans
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<String>()
    };
    let wide: String = text(vec![Line::from(fitted_hints(Mode::Manager, 200))]);
    assert!(wide.contains("start all"), "wide bar shows everything");
    let normal: String = text(vec![Line::from(fitted_hints(Mode::Manager, 59))]);
    assert!(normal.contains("q quit"), "normal bar keeps quit");
    assert!(normal.contains("/ filter"), "normal bar keeps filter");
    assert!(!normal.contains("start all"), "normal bar drops the rest");
    let narrow: String = text(vec![Line::from(fitted_hints(Mode::Manager, 40))]);
    assert!(narrow.contains("q quit"), "narrow bar keeps quit");
    assert!(!narrow.contains("? help"), "narrow bar drops the rest");
    let add: String = text(vec![Line::from(fitted_hints(Mode::Add, 40))]);
    assert!(add.contains("Enter save") && !add.contains("Space"));
    let focus: String = text(vec![Line::from(fitted_hints(Mode::Focus, 10))]);
    assert!(
        focus.contains("go to the session"),
        "short modes never compact"
    );
    let help: String = text(vec![Line::from(fitted_hints(Mode::Help, 40))]);
    assert!(help.contains("closes"), "help hints show in help mode");
}
#[test]
fn hint_spans_bold_keys_dim_descriptions() {
    let spans = hint_spans(&[("Enter", "focus"), ("", "type to input")]);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "Enter focus | type to input");
    assert_eq!(
        spans[0].style,
        Style::default().add_modifier(ratatui::style::Modifier::BOLD)
    );
    assert_eq!(
        spans[2].style,
        Style::default().add_modifier(ratatui::style::Modifier::DIM)
    );
}
#[test]
fn row_widths_keep_status_visible() {
    // Narrow panel: status reserved first, name takes the rest.
    assert_eq!(row_widths(22, 3, 9, 10), (9, 9, 0));
    // Wide panel: name fits fully, detail gets the remainder.
    assert_eq!(row_widths(36, 3, 9, 10), (9, 10, 13));
    // Long error status is capped so the name keeps some room.
    let (sw, nw, dw) = row_widths(22, 3, 40, 6);
    assert!(sw + nw + dw + 3 < 22);
    assert!(sw >= 9, "status word must survive: {sw}");
}
#[test]
fn input_window_keeps_cursor_visible() {
    // Short text passes through untouched.
    assert_eq!(
        input_window("ab", 'c', "de", 10),
        ("ab".into(), 'c', "de".into())
    );
    // Typing at the end shows the tail with the cursor last.
    assert_eq!(
        input_window("hello world", ' ', "", 5),
        ("orld".into(), ' ', "".into())
    );
    // Cursor at the front pins the window to the start.
    assert_eq!(
        input_window("", 'h', "ello world", 5),
        ("".into(), 'h', "ello".into())
    );
    // Cursor past the window trims the left side.
    assert_eq!(
        input_window("abcdef", 'g', "hij", 5),
        ("cdef".into(), 'g', "".into())
    );
}
#[test]
fn exit_notice_kind_ranks_outcomes() {
    assert_eq!(exit_notice_kind("Error: boom"), Notice::Error);
    assert_eq!(exit_notice_kind("Exited (1)"), Notice::Warn);
    assert_eq!(exit_notice_kind("Exited (137)"), Notice::Warn);
    assert_eq!(exit_notice_kind("Exited (0)"), Notice::Info);
    assert_eq!(exit_notice_kind("Stopped"), Notice::Info);
}
#[test]
fn plural_sessions_counts_nouns() {
    assert_eq!(plural_sessions(0), "0 sessions");
    assert_eq!(plural_sessions(1), "1 session");
    assert_eq!(plural_sessions(2), "2 sessions");
    assert_eq!(confirm_quit_prompt(1), "Quit and discard 1 session?");
}
#[test]
fn confirm_modals_style_keys() {
    let stop = confirm_stop_lines("web");
    let stop_text: String = stop
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(stop_text.contains("web"), "stop modal names the session");
    assert!(stop_text.contains('y') && stop_text.contains("confirms"));
    let quit = confirm_quit_lines(2);
    let quit_text: String = quit
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(quit_text.contains('2'), "quit modal names the count");
    assert!(quit_text.contains('y') && quit_text.contains("quits"));
}
#[test]
fn rendered_lines_compose_in_buffer() {
    use ratatui::{backend::TestBackend, widgets::Paragraph};
    fn row(line: Line<'static>, width: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(width, 1)).unwrap();
        term.draw(|f| {
            f.render_widget(Paragraph::new(line), f.area());
        })
        .unwrap();
        let buf = term.backend().buffer();
        (0..width).map(|x| buf[(x, 0)].symbol()).collect()
    }
    let header = row(group_header("Agents", Kind::Agent, 2, 3), 30);
    assert!(header.starts_with("Agents (2/3)"), "got {header:?}");
    let stop = confirm_stop_lines("web").into_iter().next().unwrap();
    let first = row(stop, 40);
    assert!(first.contains("Stop session 'web'?"), "got {first:?}");
    let narrow = row(Line::from(fitted_hints(Mode::Manager, 38)), 40);
    assert!(narrow.contains("q quit"), "got {narrow:?}");
    let roomy = row(Line::from(fitted_hints(Mode::Manager, 50)), 60);
    assert!(roomy.contains("? help"), "got {roomy:?}");
}
#[test]
fn notice_styles_rank_severity() {
    assert_eq!(notice_style(Notice::Info), Style::default());
    assert_eq!(
        notice_style(Notice::Warn),
        Style::default().fg(Color::Yellow)
    );
    assert_eq!(
        notice_style(Notice::Error),
        Style::default()
            .fg(Color::Red)
            .add_modifier(ratatui::style::Modifier::BOLD)
    );
}
#[test]
fn fleet_summary_counts() {
    assert_eq!(fleet_summary(2, 5), "● 2/5");
    assert_eq!(fleet_summary(0, 0), "● 0/0");
}
#[test]
fn summary_width_fits_alert() {
    assert_eq!(summary_width(2, 5, 0), ("● 2/5".chars().count() + 1) as u16);
    assert_eq!(
        summary_width(12, 34, 5),
        ("● 12/34".chars().count() + " ✖ 5".chars().count() + 1) as u16
    );
}
#[test]
fn sidebar_border_marks_focus() {
    assert_eq!(
        sidebar_border_style(false),
        Style::default().fg(Color::Cyan)
    );
    assert_eq!(
        sidebar_border_style(true),
        Style::default().fg(Color::DarkGray)
    );
}
#[test]
fn pane_border_marks_focus() {
    assert_eq!(pane_border_style(true), Style::default().fg(Color::Green));
    assert_eq!(
        pane_border_style(false),
        Style::default().fg(Color::DarkGray)
    );
}
#[test]
fn failed_sessions_need_attention() {
    assert!(!needs_attention("Running"));
    assert!(!needs_attention("Stopped"));
    assert!(!needs_attention("Exited (0)"));
    assert!(needs_attention("Exited (1)"));
    assert!(needs_attention("Exited (137)"));
    assert!(needs_attention("Error: boom"));
}
#[test]
fn empty_list_hint_guides_first_run() {
    assert!(empty_list_hint().contains("squad add"));
    assert!(list_table(&[], false).is_empty(), "stdout stays empty");
}
#[test]
fn list_title_shows_filter_counts() {
    let base = format!("AgentSquad v{}", env!("CARGO_PKG_VERSION"));
    assert_eq!(list_title(5, 5), base);
    assert_eq!(list_title(0, 0), base);
    assert_eq!(list_title(5, 2), format!("{base} (2/5)"));
}
#[test]
fn agent_tag_shows_program() {
    let mk = |command: Vec<String>| Project {
        name: "x".into(),
        path: PathBuf::from("/tmp"),
        command,
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    assert_eq!(
        agent_tag(&mk(vec!["claude".into(), "--dangerous".into()])),
        Some("claude")
    );
    assert_eq!(agent_tag(&mk(shell())), None);
    assert_eq!(
        agent_tag(&mk(vec![])),
        None,
        "hand-written configs may omit the command"
    );
}
#[test]
fn paint_kind_matches_tui_hues() {
    assert_eq!(paint_kind("agent  ", Kind::Agent, false), "agent  ");
    assert_eq!(paint_kind("service", Kind::Service, false), "service");
    let agent = paint_kind("agent  ", Kind::Agent, true);
    let service = paint_kind("service", Kind::Service, true);
    assert_ne!(agent, service, "kinds stay distinguishable");
    assert!(agent.contains("agent  "));
    assert!(service.contains("service"));
}
#[test]
fn kind_colors_are_distinct() {
    assert_eq!(kind_color(Kind::Agent), Color::Cyan);
    assert_eq!(kind_color(Kind::Service), Color::Magenta);
    assert_ne!(kind_color(Kind::Agent), kind_color(Kind::Service));
}
#[test]
fn group_header_dims_counts() {
    let line = group_header("Agents", Kind::Agent, 2, 3);
    assert_eq!(line.width(), "Agents (2/3)".len());
    assert_eq!(line.spans.len(), 2);
    assert_eq!(line.spans[0].content, "Agents");
    assert_eq!(
        line.spans[0].style,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(ratatui::style::Modifier::BOLD)
    );
    assert_eq!(line.spans[1].content, " (2/3)");
    assert_eq!(
        line.spans[1].style,
        Style::default().add_modifier(ratatui::style::Modifier::DIM)
    );
}
#[test]
fn group_counts_split_by_kind() {
    let mk = |name: &str, kind: Kind| Project {
        name: name.into(),
        path: PathBuf::from("/tmp"),
        command: vec!["sh".into()],
        kind,
        autostart: false,
        ledger: false,
    };
    let projects = vec![
        mk("a1", Kind::Agent),
        mk("a2", Kind::Agent),
        mk("s1", Kind::Service),
    ];
    let running = [true, false, true];
    assert_eq!(group_counts(&projects, &running, Kind::Agent), (1, 2));
    assert_eq!(group_counts(&projects, &running, Kind::Service), (1, 1));
    assert_eq!(group_counts(&projects, &[], Kind::Agent), (0, 2));
}
#[test]
fn wheel_maps_to_scroll_lines() {
    use crossterm::event::MouseEventKind as M;
    assert_eq!(wheel_lines(M::ScrollUp), Some(3));
    assert_eq!(wheel_lines(M::ScrollDown), Some(-3));
    assert_eq!(
        wheel_lines(M::Down(crossterm::event::MouseButton::Left)),
        None
    );
    assert_eq!(wheel_lines(M::Moved), None);
}
#[test]
fn quit_counts_discardable_sessions() {
    let sessions: Vec<Option<u8>> = vec![None, Some(1), None, Some(2)];
    assert_eq!(live_session_count(&sessions), 2);
    let empty: Vec<Option<u8>> = vec![None];
    assert_eq!(live_session_count(&empty), 0);
    let prompt = confirm_quit_prompt(2);
    assert!(prompt.contains('2'), "prompt names the count");
}
#[test]
fn pane_status_text_shows_state() {
    assert_eq!(
        pane_status_text("Running", Some(125), 0),
        format!("{} Running 2m", spinner_frame(0))
    );
    assert_eq!(
        pane_status_text("Running", None, 3),
        format!("{} Running", spinner_frame(3))
    );
    assert_eq!(pane_status_text("Stopped", None, 9), "● Stopped");
    assert_eq!(pane_status_text("Exited (1)", None, 9), "● Exited (1)");
}
#[test]
fn uptime_labels_scale() {
    assert_eq!(uptime_label(0), "0s");
    assert_eq!(uptime_label(7), "7s");
    assert_eq!(uptime_label(59), "59s");
    assert_eq!(uptime_label(60), "1m");
    assert_eq!(uptime_label(125), "2m");
    assert_eq!(uptime_label(3599), "59m");
    assert_eq!(uptime_label(3600), "1h");
    assert_eq!(uptime_label(3900), "1h5m");
    assert_eq!(uptime_label(9000), "2h30m");
    assert_eq!(uptime_label(90000), "1d1h");
}
#[test]
fn start_session_starts_and_reports() {
    let dir = env::current_dir().unwrap();
    let shell: Vec<String> = if cfg!(windows) {
        vec![
            "cmd.exe".into(),
            "/d".into(),
            "/c".into(),
            "echo AGENTSQUAD_OK".into(),
        ]
    } else {
        vec!["sh".into(), "-c".into(), "echo AGENTSQUAD_OK".into()]
    };
    let projects = vec![
        Project {
            name: "ok".into(),
            path: dir.clone(),
            command: shell,
            kind: Kind::Agent,
            autostart: false,
            ledger: false,
        },
        Project {
            name: "bad".into(),
            path: dir,
            command: vec!["agentsquad-no-such-binary-xyz".into()],
            kind: Kind::Service,
            autostart: false,
            ledger: false,
        },
    ];
    let mut sessions: Vec<Option<Session>> = vec![None, None];
    // Stopped session starts.
    assert!(start_session(&mut sessions, &projects, 0, 24, 80).is_none());
    assert!(sessions[0].is_some());
    // Already running: no-op, no respawn, no error.
    assert!(start_session(&mut sessions, &projects, 0, 24, 80).is_none());
    assert!(sessions[0].is_some());
    // Bad binary surfaces an error and leaves no session behind.
    assert!(start_session(&mut sessions, &projects, 1, 24, 80).is_some());
    assert!(sessions[1].is_none());
    // Unknown index reports instead of panicking.
    assert!(start_session(&mut sessions, &projects, 7, 24, 80).is_some());
}
#[test]
fn merge_appends_missing_projects() {
    let mk = |name: &str| Project {
        name: name.into(),
        path: PathBuf::from("/tmp"),
        command: vec!["sh".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    let mut c = Config {
        projects: vec![mk("a")],
        scrollback: None,
        ..Default::default()
    };
    let mut sessions: Vec<Option<Session>> = vec![None];
    merge_projects(
        &mut c,
        &mut sessions,
        Config {
            projects: vec![mk("a"), mk("B")],
            scrollback: None,
            ..Default::default()
        },
    );
    assert_eq!(c.projects.len(), 2);
    assert_eq!(sessions.len(), 2);
    assert_eq!(c.projects[1].name, "B");
    // Idempotent: a second merge adds nothing.
    merge_projects(
        &mut c,
        &mut sessions,
        Config {
            projects: vec![mk("A"), mk("b")],
            scrollback: None,
            ..Default::default()
        },
    );
    assert_eq!(c.projects.len(), 2);
    assert_eq!(sessions.len(), 2);
}

#[test]
fn shell_defaults_to_powershell_on_windows() {
    let sh = shell();
    assert!(!sh.is_empty());
    if cfg!(windows) {
        let lower = sh[0].to_ascii_lowercase();
        assert!(
            lower == "pwsh.exe" || lower == "powershell.exe",
            "expected PowerShell default on Windows, got {sh:?}"
        );
        assert!(is_bare_shell(&sh));
    }
    assert!(is_shell_program("cmd.exe"));
    assert!(is_shell_program("powershell.exe"));
    assert!(is_shell_program("pwsh"));
    assert!(!is_shell_program("jarvis.exe"));
    assert!(!is_shell_program("claude"));
}

#[test]
fn service_naming_looks_through_shells_and_live_foreground() {
    let parts = |s: &str| -> Vec<String> { split_command(s) };
    // `cmd /c jarvis --serve` extracts `jarvis` instead of `cmd`.
    assert_eq!(
        program_name(&parts("cmd.exe /c jarvis --serve")).as_deref(),
        Some("jarvis")
    );
    assert_eq!(
        program_name(&parts("powershell.exe -NoLogo -Command \"jarvis start\"")).as_deref(),
        Some("jarvis")
    );
    // A bare shell has no program name, so a service falls back to directory name
    // until a foreground process is launched inside it.
    assert_eq!(program_name(&parts("cmd.exe")), None);
    assert_eq!(program_name(&parts("powershell.exe -NoLogo")), None);

    let p = Project {
        name: "my-dir".into(),
        path: PathBuf::from("C:/work/my-dir"),
        command: vec!["powershell.exe".into(), "-NoLogo".into()],
        kind: Kind::Service,
        autostart: false,
        ledger: false,
    };
    assert_eq!(live_name(&p, None), "my-dir");
    assert_eq!(live_name(&p, Some("jarvis")), "jarvis");
    assert_eq!(live_pane_title(&p, Some("jarvis")), "jarvis");

    // Foreground process tree lookup skips shells/conhost and picks the real app.
    let mut table = ProcTable::default();
    table.children.insert(100, vec![101, 102]);
    table.children.insert(102, vec![103]);
    table.names.insert(100, "powershell.exe".into());
    table.names.insert(101, "conhost.exe".into());
    table.names.insert(102, "cmd.exe".into());
    table.names.insert(103, "jarvis.exe".into());
    assert_eq!(foreground_name(100, &table).as_deref(), Some("jarvis"));
}

#[test]
fn needs_input_and_working_ignore_false_positives() {
    // Active spinner / interrupt hint suppresses false `?` even when question text is on screen.
    let busy_screen = "Do you want to refactor this file?\n✶ Thinking… (esc to interrupt)";
    assert!(!is_needs_input(busy_screen));
    assert!(is_working_text(busy_screen));

    let cancel_timer = "Allow tool execution?\n⠋ Running command (esc to cancel, 4s)";
    assert!(!is_needs_input(cancel_timer));
    assert!(is_working_text(cancel_timer));

    // Ordinary prose with a question phrase but no choice menu or `[y/n]` does not trigger `?`.
    assert!(!is_needs_input(
        "Here is the summary:\nDo you want to see more details later."
    ));
    // Source code / diff output containing prompt literals must not trigger `?`.
    assert!(!is_needs_input(
        "180: \"[y/n]\",\n+ assert!(is_needs_input(\"Allow tool? [y/n]\"));"
    ));
    // Mid-sentence verbs do not trigger Working.
    assert!(!is_working_text(
        "Finished reading 12 files and writing output."
    ));
}

#[test]
fn update_cache_and_sha256_parsing() {
    // Fresh tag hit within 900s cooldown:
    assert_eq!(
        parse_cache("1000 v0.5.9\n", 1500),
        Some(Some("v0.5.9".to_string()))
    );
    // Expired after 900s:
    assert_eq!(parse_cache("1000 v0.5.9\n", 2000), None);
    // Recent failure marker `-` is cached within 600s:
    assert_eq!(parse_cache("1000 -\n", 1300), Some(None));
    assert_eq!(parse_cache("1000 -\n", 1700), None);

    let good_sha = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  squad-windows-x86_64.zip\n";
    assert_eq!(
        parse_sha256(good_sha).as_deref(),
        Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    );
    assert_eq!(parse_sha256("not-a-valid-hash"), None);
}

#[test]
fn mouse_wheel_sgr_and_x10_encoding() {
    assert_eq!(
        mouse_wheel_seq(vt100::MouseProtocolEncoding::Sgr, true, 1, 1),
        b"\x1b[<64;1;1M"
    );
    assert_eq!(
        mouse_wheel_seq(vt100::MouseProtocolEncoding::Sgr, false, 10, 5),
        b"\x1b[<65;10;5M"
    );
    assert_eq!(
        mouse_wheel_seq(vt100::MouseProtocolEncoding::Default, true, 1, 1),
        vec![0x1b, b'[', b'M', 96, 33, 33]
    );
}
#[test]
fn legacy_migrations_run_once() {
    let dir = env::temp_dir().join(format!("squad-test-once-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("config.toml");
    let body = r#"
[[projects]]
name = "keep"
path = "work/keep"
command = ["cmd.exe"]
kind = "agent"
"#;
    // Unversioned file: migrated and stamped, so the stamp survives a save.
    fs::write(&path, body).unwrap();
    let old = load_from(&path).unwrap();
    assert_eq!(old.version, CONFIG_VERSION);
    if cfg!(windows) {
        assert_eq!(old.projects[0].command, shell());
    }
    // Stamped file: a deliberate `cmd.exe` is left alone.
    fs::write(&path, format!("version = {CONFIG_VERSION}\n{body}")).unwrap();
    let kept = load_from(&path).unwrap();
    assert_eq!(kept.projects[0].command, ["cmd.exe"]);
    save_to(&kept, &path).unwrap();
    assert_eq!(load_from(&path).unwrap().projects[0].command, ["cmd.exe"]);
    let _ = fs::remove_dir_all(&dir);
}
#[test]
fn interpreters_are_named_by_their_script() {
    let node = r#""C:\Program Files\nodejs\node.exe" C:\Users\me\AppData\Roaming\npm/node_modules/jarvis-cli/dist/cli.js start"#;
    assert_eq!(script_label("node", node).as_deref(), Some("jarvis-cli"));
    let scoped = r"node C:\x\node_modules\@acme\tool\bin.js";
    assert_eq!(script_label("node", scoped).as_deref(), Some("tool"));
    assert_eq!(
        script_label("node", "node --inspect server.js").as_deref(),
        Some("server")
    );
    assert_eq!(
        script_label("python", "python -u app.py").as_deref(),
        Some("app")
    );
    // Real programs keep their own name; a bare interpreter has no script.
    assert_eq!(script_label("ollama", "ollama serve"), None);
    assert_eq!(script_label("node", "node"), None);
}
#[test]
#[cfg(all(windows, target_pointer_width = "64"))]
fn command_line_reads_a_live_process() {
    let me = command_line(std::process::id()).expect("own command line");
    assert!(me.to_ascii_lowercase().contains("squad"), "got {me}");
}
#[test]
fn empty_command_errors_instead_of_panicking() {
    let p = Project {
        name: "x".into(),
        path: env::current_dir().unwrap(),
        command: vec![],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    let err = Session::spawn(&p, 24, 80)
        .err()
        .expect("must fail")
        .to_string();
    assert!(err.contains("empty command"), "{err}");
}
#[test]
fn save_to_leaves_no_temp_file() {
    let dir = env::temp_dir().join(format!("squad-save-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("config.toml");
    save_to(&Config::default(), &file).unwrap();
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(names, ["config.toml"]);
}

#[test]
fn bell_rings_for_attention_not_for_noise() {
    use AgentPhase::*;
    assert!(rings_bell(Working, NeedsInput));
    assert!(rings_bell(Idle, NeedsInput));
    assert!(rings_bell(Working, Done));
    assert!(!rings_bell(Idle, Done), "Done without work is not news");
    assert!(!rings_bell(Done, Working));
    assert!(!rings_bell(NeedsInput, Working));
    assert!(!rings_bell(Working, Idle));
}

#[test]
fn autostart_and_bell_config_roundtrip() {
    let text = r#"
bell = false
[[projects]]
name = "a"
path = "."
command = ["x"]
autostart = true
[[projects]]
name = "b"
path = "."
command = ["x"]
"#;
    let c: Config = toml::from_str(text).unwrap();
    assert_eq!(c.bell, Some(false));
    assert!(c.projects[0].autostart && !c.projects[1].autostart);
    let out = toml::to_string(&c).unwrap();
    assert_eq!(out.matches("autostart").count(), 1, "false is not written");
    let again: Config = toml::from_str(&out).unwrap();
    assert!(again.projects[0].autostart && again.bell == Some(false));
    assert!(Config::default().bell.is_none());
}

#[test]
fn alt_digit_switches_project_inside_terminal() {
    let alt = KeyModifiers::ALT;
    assert_eq!(focus_switch_key(KeyCode::Char('1'), alt, 3), Some(0));
    assert_eq!(focus_switch_key(KeyCode::Char('3'), alt, 3), Some(2));
    assert_eq!(
        focus_switch_key(KeyCode::Char('4'), alt, 3),
        None,
        "no such row"
    );
    assert_eq!(focus_switch_key(KeyCode::Char('0'), alt, 3), None);
    // Plain digits and other modifiers still reach the session.
    assert_eq!(
        focus_switch_key(KeyCode::Char('1'), KeyModifiers::NONE, 3),
        None
    );
    assert_eq!(
        focus_switch_key(KeyCode::Char('1'), KeyModifiers::CONTROL, 3),
        None
    );
    assert_eq!(
        focus_switch_key(KeyCode::Char('1'), alt | KeyModifiers::SHIFT, 3),
        None
    );
}

#[test]
fn messages_expire_and_problems_linger_longer() {
    assert!(message_ttl(Notice::Info) < message_ttl(Notice::Warn));
    assert_eq!(message_ttl(Notice::Warn), message_ttl(Notice::Error));
    assert!(message_ttl(Notice::Info) >= std::time::Duration::from_secs(3));
}

#[test]
fn modal_rect_fits_its_text_and_the_screen() {
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let short = vec![Line::from("Quit?"), Line::from("y / n")];
    let r = modal_rect(&short, area);
    assert_eq!(
        (r.width, r.height),
        (40, 4),
        "narrow text still gets a usable width"
    );
    let long = vec![Line::from("x".repeat(120))];
    assert_eq!(modal_rect(&long, area).width, 64, "wide text is capped");
    let tiny = modal_rect(&short, ratatui::layout::Rect::new(0, 0, 30, 3));
    assert!(
        tiny.width <= 30 && tiny.height <= 3,
        "never larger than the screen"
    );
}

#[test]
fn add_preview_name_matches_what_gets_registered() {
    let mk = |name: &str| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: vec!["x".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    let existing = vec![mk("node")];
    let none: Vec<String> = Vec::new();
    assert_eq!(
        preview_name(&existing, r#" "C:\work\my-app\" "#, Kind::Agent, &none),
        Some("my-app".into())
    );
    assert_eq!(preview_name(&existing, "", Kind::Agent, &none), None);
    assert_eq!(preview_name(&existing, ".", Kind::Agent, &none), None);
    let node = vec!["node".to_string(), "server.js".to_string()];
    assert_eq!(
        preview_name(&existing, "", Kind::Service, &node),
        Some("node-2".into()),
        "services get a unique program name"
    );
}

#[test]
fn prompt_summary_finds_the_question() {
    let screen = "\
 Edit file src/lib.rs
+ let x = 1;

\u{2502} Do you want to proceed? \u{2502}
\u{2502} \u{276f} 1. Yes \u{2502}
\u{2502}   2. No \u{2502}
";
    assert_eq!(
        prompt_summary(screen).as_deref(),
        Some("Do you want to proceed?")
    );
    assert_eq!(prompt_summary("only line\n").as_deref(), Some("only line"));
    assert_eq!(prompt_summary("  \n\n"), None);
}

#[test]
fn destructive_confirms_need_y_not_enter() {
    assert_eq!(confirm_destructive_key(KeyCode::Char('y')), Some(true));
    assert_eq!(confirm_destructive_key(KeyCode::Char('Y')), Some(true));
    assert_eq!(
        confirm_destructive_key(KeyCode::Enter),
        None,
        "Enter must not destroy a session or project"
    );
    assert_eq!(confirm_destructive_key(KeyCode::Esc), Some(false));
    // The non-destructive start-all keeps its Enter shortcut.
    assert_eq!(confirm_key(KeyCode::Enter), Some(true));
}

#[test]
fn fit_marks_truncation() {
    assert_eq!(fit("hello", 8), "hello");
    assert_eq!(fit("hello", 5), "hello");
    assert_eq!(fit("hello world", 5), "hell…");
    assert_eq!(fit("hello", 0), "");
}

#[test]
fn filter_matches_path_too() {
    let mk = |name: &str, path: &str| Project {
        name: name.into(),
        path: PathBuf::from(path),
        command: vec!["claude".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
    };
    let projects = vec![mk("web", "/work/my-api"), mk("other", "/work/site")];
    assert_eq!(apply_filter(&projects, "my-api"), vec![0]);
    assert_eq!(apply_filter(&projects, "web"), vec![0]);
}

#[test]
fn help_popup_survives_inside_clicks() {
    use ratatui::layout::Rect;
    let area = Rect::new(0, 0, 100, 30);
    let popup = help_popup(area);
    assert!(popup.width <= 66 && popup.height == help_lines().len() as u16 + 2);
    assert!(popup.contains(ratatui::layout::Position::new(popup.x + 2, popup.y + 2)));
}

#[test]
fn pane_usage_note_labels_numbers() {
    let u = Usage {
        cpu: 12.4,
        mem: 340 << 20,
    };
    assert_eq!(pane_usage_note(u), "CPU 12% · 340M");
    // The sidebar keeps its compact unlabeled form.
    assert_eq!(usage_label(u), "12% 340M");
}

#[test]
fn context_menu_offers_start_for_stopped() {
    let running = context_menu_items(true);
    assert_eq!(running.len(), 4);
    assert!(running.iter().any(|(_, l)| l.contains("Focus")));
    assert!(running.iter().any(|(_, l)| l.contains("Stop")));
    let stopped = context_menu_items(false);
    assert_eq!(stopped.len(), 3);
    assert!(stopped.iter().any(|(_, l)| l.contains("Start")));
    assert!(!stopped.iter().any(|(_, l)| l.contains("Stop")));
    // Number keys are 1-based in order with no gaps.
    assert_eq!(
        running.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        ['1', '2', '3', '4']
    );
}

#[test]
fn context_menu_lines_match_click_mapping() {
    // One title row, then one row per action: clicking row `1 + action`
    // (the mapping in `on_mouse`) must land on that action.
    for running in [true, false] {
        let lines = context_menu_lines("web", running);
        let items = context_menu_items(running);
        assert_eq!(lines.len(), items.len() + 3);
        for (i, (key, _)) in items.iter().enumerate() {
            let row: String = lines[i + 1]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert!(row.starts_with(&key.to_string()), "row {i} shows its key");
        }
        let hint: String = lines
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(hint.contains("Esc"), "last row tells how to cancel");
    }
}

#[test]
fn idle_note_flags_quiet_finished_agents_only() {
    use AgentPhase::*;
    assert_eq!(idle_note(Done, Some(300)).as_deref(), Some("idle 5m"));
    assert_eq!(idle_note(Idle, Some(60)).as_deref(), Some("idle 1m"));
    assert_eq!(
        idle_note(Done, Some(59)),
        None,
        "under a minute is not idle"
    );
    assert_eq!(idle_note(Done, None), None, "no output yet");
    assert_eq!(idle_note(Working, Some(900)), None);
    assert_eq!(
        idle_note(NeedsInput, Some(900)),
        None,
        "waiting is not idle"
    );
}

#[test]
fn startup_selection_prefers_explicit_then_last_then_first() {
    let mk = |name: &str, kind| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: vec!["x".into()],
        kind,
        autostart: false,
        ledger: false,
    };
    // Config order: svc, a1, a2. Sidebar order: agents first, so a1, a2, svc.
    let projects = vec![
        mk("svc", Kind::Service),
        mk("a1", Kind::Agent),
        mk("a2", Kind::Agent),
    ];
    let order = display_order(&projects);
    assert_eq!(start_selection(&projects, &order, None, None), 0);
    assert_eq!(start_selection(&projects, &order, None, Some("A2")), 1);
    assert_eq!(start_selection(&projects, &order, None, Some("svc")), 2);
    assert_eq!(
        start_selection(&projects, &order, Some(1), Some("svc")),
        0,
        "explicit wins"
    );
    assert_eq!(
        start_selection(&projects, &order, None, Some("gone")),
        0,
        "removed project"
    );
}

fn scratch_dir(tag: &str) -> PathBuf {
    let d = env::temp_dir().join(format!("squad-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn utc_stamp_formats_known_instants() {
    use crate::ledger::utc_stamp;
    assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
    assert_eq!(utc_stamp(1_000_000_000), "2001-09-09T01:46:40Z");
    assert_eq!(utc_stamp(1_709_164_800), "2024-02-29T00:00:00Z", "leap day");
    assert_eq!(utc_stamp(1_709_251_199), "2024-02-29T23:59:59Z");
}

#[test]
fn ledger_is_created_once_and_never_overwritten() {
    use crate::ledger;
    let dir = scratch_dir("ledger");
    assert!(ledger::ensure(&dir).unwrap(), "first call creates it");
    for f in ["README.md", "CONTEXT.md"] {
        assert!(dir.join(".squad").join(f).is_file(), "{f} exists");
    }
    assert!(dir.join(".squad").join("log").is_dir());
    // Someone edits the context; a later start must leave it alone.
    let ctx = dir.join(".squad").join("CONTEXT.md");
    std::fs::write(&ctx, "my notes").unwrap();
    assert!(!ledger::ensure(&dir).unwrap(), "second call is a no-op");
    assert_eq!(std::fs::read_to_string(&ctx).unwrap(), "my notes");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ledger_hides_itself_from_git_without_duplicating() {
    use crate::ledger;
    let dir = scratch_dir("ledger-git");
    std::fs::create_dir_all(dir.join(".git").join("info")).unwrap();
    std::fs::write(dir.join(".git").join("info").join("exclude"), "target/").unwrap();
    ledger::ensure(&dir).unwrap();
    let ex = std::fs::read_to_string(dir.join(".git").join("info").join("exclude")).unwrap();
    assert_eq!(
        ex,
        "target/
.squad/
",
        "existing lines kept, ours added"
    );
    // Re-creating after the folder is deleted must not add a second line.
    std::fs::remove_dir_all(dir.join(".squad")).unwrap();
    ledger::ensure(&dir).unwrap();
    let ex = std::fs::read_to_string(dir.join(".git").join("info").join("exclude")).unwrap();
    assert_eq!(ex.matches(".squad/").count(), 1);
    // A plain folder (no .git) just gets the ledger.
    let plain = scratch_dir("ledger-plain");
    assert!(ledger::ensure(&plain).unwrap());
    assert!(!plain.join(".git").exists());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&plain);
}

#[test]
fn ledger_events_append_only_when_a_ledger_exists() {
    use crate::ledger;
    let dir = scratch_dir("ledger-events");
    ledger::event(&dir, "ignored");
    assert!(!dir.join(".squad").exists(), "no ledger, nothing written");
    ledger::ensure(&dir).unwrap();
    ledger::event(&dir, "start demo");
    ledger::event(&dir, "stop");
    let log = std::fs::read_to_string(dir.join(".squad").join("events.log")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].ends_with("Z start demo") && lines[1].ends_with("Z stop"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ledger_session_records_start_and_exit() {
    let dir = scratch_dir("ledger-session");
    let p = Project {
        name: "led".into(),
        path: dir.clone(),
        command: vec!["cmd.exe".into(), "/d".into(), "/c".into(), "exit 3".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: true,
    };
    let mut s = Session::spawn(&p, 24, 80).unwrap();
    let (msg, _) = s.ledger_note.take().expect("creation is announced once");
    assert!(msg.contains("Operations log opened"), "{msg}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while s.status.is_running() && std::time::Instant::now() < deadline {
        s.update();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(s.status, SessionStatus::Exited(3));
    let log = std::fs::read_to_string(dir.join(".squad").join("events.log")).unwrap();
    assert!(log.contains("start led"), "{log}");
    assert!(log.contains("exit code=3"), "{log}");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn download_failures_say_why() {
    assert!(download_failure("504", Some(22)).contains("HTTP 504"));
    assert!(download_failure("504", Some(22)).contains("their side"));
    assert!(download_failure("503", Some(22)).contains("try again later"));
    assert!(download_failure("404", Some(22)).contains("does not exist"));
    assert!(download_failure("403", Some(22)).contains("HTTP 403"));
    assert!(download_failure("000", Some(28)).contains("timed out"));
    assert!(download_failure("000", Some(6)).contains("cannot reach"));
    assert!(download_failure("", Some(35)).contains("exit 35"));
    assert!(download_failure("", None).contains("interrupted"));
}
