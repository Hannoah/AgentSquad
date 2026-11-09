use super::*;

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
        reports_to: None,
        pinned: false,
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
        reports_to: None,
        pinned: false,
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
        reports_to: None,
        pinned: false,
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
        reports_to: None,
        pinned: false,
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
fn tree_cpu_does_not_hide_a_waiting_prompt() {
    let p = Project {
        name: "cpu".into(),
        path: env::current_dir().unwrap(),
        command: shell(),
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
    };
    let mut s = Session::spawn(&p, 24, 80).unwrap();
    s.usage = Some(Usage { cpu: 50.0, mem: 0 });
    assert_eq!(s.phase(), AgentPhase::Working, "busy tree, no prompt");
    s.cached_needs_input = true;
    assert_eq!(
        s.phase(),
        AgentPhase::NeedsInput,
        "quiet prompt wins over tree CPU"
    );
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
fn exit_notice_kind_ranks_outcomes() {
    assert_eq!(exit_notice_kind("Error: boom"), Notice::Error);
    assert_eq!(exit_notice_kind("Exited (1)"), Notice::Warn);
    assert_eq!(exit_notice_kind("Exited (137)"), Notice::Warn);
    assert_eq!(exit_notice_kind("Exited (0)"), Notice::Info);
    assert_eq!(exit_notice_kind("Stopped"), Notice::Info);
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
            reports_to: None,
            pinned: false,
        },
        Project {
            name: "bad".into(),
            path: dir,
            command: vec!["agentsquad-no-such-binary-xyz".into()],
            kind: Kind::Service,
            autostart: false,
            ledger: false,
            reports_to: None,
            pinned: false,
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
fn bell_rings_for_attention_not_for_noise() {
    use AgentPhase::*;
    assert!(rings_bell(NeedsInput));
    assert!(
        !rings_bell(Done),
        "a finished task is silent: title and sidebar show it"
    );
    assert!(!rings_bell(Working));
    assert!(!rings_bell(Idle));
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
