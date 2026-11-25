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
#[cfg(unix)]
fn stop_kills_process_tree_unix() {
    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only probes for existence.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    let dir = scratch_dir("treekill-unix");
    let p = Project {
        name: "treekill".into(),
        path: dir.clone(),
        command: vec!["sh".into(), "-c".into(), "sleep 300 & wait".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
        restart_on_crash: false,
    };
    let s = Session::spawn(&p, 24, 80).unwrap();
    let pid = s.child.process_id().expect("pty child pid");
    let mut tree = Vec::new();
    for _ in 0..50 {
        tree = crate::sys::descendant_pids(pid);
        if tree.len() > 1 {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(tree.len() > 1, "grandchild never appeared");
    drop(s);
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(100));
        if tree.iter().all(|p| !alive(*p)) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    panic!("process tree survived stop: {tree:?}");
}
#[test]
#[cfg(unix)]
fn stop_kills_orphaned_grandchildren_unix() {
    // The shell exits after the child has had time to install its HUP trap
    // (exiting first would race the trap); its background child is reparented to launchd
    // and no longer a descendant, yet it must still die with the session.
    let dir = scratch_dir("orphan-unix");
    let pid_file = dir.join("pid");
    let p = Project {
        name: "orphankill".into(),
        path: dir.clone(),
        command: vec![
            "sh".into(),
            "-c".into(),
            format!(
                "(trap '' HUP; exec sleep 300) & echo $! > '{}'; sleep 0.5; exit 0",
                pid_file.display()
            ),
        ],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
        restart_on_crash: false,
    };
    let mut s = Session::spawn(&p, 24, 80).unwrap();
    for _ in 0..50 {
        s.update();
        if !s.status.is_running() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(!s.status.is_running(), "shell should have exited");
    let orphan: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only probes for existence.
    let alive = |pid: u32| unsafe { libc::kill(pid as i32, 0) == 0 };
    assert!(alive(orphan), "orphan should outlive its shell");
    drop(s);
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(100));
        if !alive(orphan) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
    }
    panic!("orphaned grandchild {orphan} survived stop");
}
#[test]
#[cfg(unix)]
fn stop_spares_a_stranger_that_reused_the_root_pid() {
    // kill_group must never touch a live process whose pid equals the group id.
    let mut stranger = std::process::Command::new("sleep")
        .arg("300")
        .spawn()
        .unwrap();
    crate::sys::kill_group(stranger.id());
    assert!(
        stranger.try_wait().unwrap().is_none(),
        "stranger was killed"
    );
    stranger.kill().unwrap();
    let _ = stranger.wait();
}
#[test]
#[cfg(windows)]
fn stop_kills_process_tree() {
    let dir = scratch_dir("treekill-win");
    let p = Project {
        name: "treekill".into(),
        path: dir.clone(),
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
        restart_on_crash: false,
    };
    let s = Session::spawn(&p, 24, 80).unwrap();
    let pid = s.child.process_id().expect("pty child pid");
    let mut tree = Vec::new();
    for _ in 0..100 {
        tree = descendant_pids(pid);
        if tree.iter().any(|p| *p != pid) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if !tree.contains(&pid) {
        tree.push(pid);
    }
    assert!(tree.len() > 1, "grandchild never appeared");
    let watched = Watched::new(&tree);
    assert_eq!(watched.len(), tree.len(), "the tree died before the stop");
    drop(s);
    let survivors = watched.survivors(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        survivors.is_empty(),
        "process tree survived stop: {survivors:?} of {tree:?}"
    );
}
#[test]
#[cfg(windows)]
fn stop_kills_orphaned_grandchildren() {
    // A `start`ed grandchild outlives its wrapper: the direct child is
    // already dead when the session drops, yet the orphan must still die.
    let dir = scratch_dir("orphankill-win");
    let p = Project {
        name: "orphankill".into(),
        path: dir.clone(),
        command: vec![
            "cmd.exe".into(),
            "/d".into(),
            "/c".into(),
            "start".into(),
            "/min".into(),
            "ping".into(),
            // Long-lived on purpose: a ping that ends by itself would fake a pass.
            "-n".into(),
            "300".into(),
            "127.0.0.1".into(),
        ],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
        restart_on_crash: false,
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
    let watched = Watched::new(&orphans);
    assert_eq!(
        watched.len(),
        orphans.len(),
        "the orphan died before the stop"
    );
    drop(s);
    let survivors = watched.survivors(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        survivors.is_empty(),
        "orphaned grandchild survived stop: {survivors:?}"
    );
}
#[test]
#[cfg(windows)]
fn session_sampling_tracks_a_live_tree() {
    let dir = scratch_dir("usage-win");
    let p = Project {
        name: "usage".into(),
        path: dir.clone(),
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
        restart_on_crash: false,
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
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn terminal_parser() {
    let mut p = vt100::Parser::new(NonZero::new(10).unwrap(), NonZero::new(40).unwrap(), 50);
    p.process(b"hello\r\nworld");
    assert!(p.screen().contents().contains("world"));
}
#[test]
fn pty_smoke() {
    let dir = scratch_dir("pty-smoke");
    let p = Project {
        name: "smoke".into(),
        path: dir.clone(),
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
        restart_on_crash: false,
    };
    let mut s = Session::spawn(&p, 24, 80).unwrap();
    for _ in 0..100 {
        s.update();
        if s.parser.screen().contents().contains("AGENTSQUAD_OK") {
            drop(s);
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    panic!("PTY output not received");
}
#[test]
fn tree_cpu_does_not_hide_a_waiting_prompt() {
    let dir = scratch_dir("tree-cpu");
    let p = Project {
        name: "cpu".into(),
        path: dir.clone(),
        command: shell(),
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
        restart_on_crash: false,
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
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
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
    // Stale verbs higher up in the response above an idle prompt do not trigger Working,
    // nor do middle-dot markdown bullets ending in `...`.
    assert!(!is_working_text(
        "Reading the configuration...\nDone.\nEverything is ready.\n> "
    ));
    assert!(!is_working_text("· item one...\n> "));
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
#[test]
fn missing_folder_is_named_in_the_error() {
    let p = Project {
        name: "gone".into(),
        path: scratch_dir("gone").join("nope"),
        command: vec!["sh".into()],
        kind: Kind::Service,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
        restart_on_crash: false,
    };
    let err = Session::spawn(&p, 24, 80)
        .err()
        .expect("no folder, no session");
    assert!(format!("{err:#}").contains("is missing"), "{err:#}");
}

#[test]
fn resize_repaint_is_not_agent_output() {
    use std::time::{Duration, Instant};
    let p = Project {
        name: "resize".into(),
        path: std::env::temp_dir(),
        command: crate::config::shell(),
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
        restart_on_crash: false,
    };
    let mut s = Session::spawn(&p, 30, 100).unwrap();
    let settle = |s: &mut Session, ms: u64| {
        let end = Instant::now() + Duration::from_millis(ms);
        let mut changed = false;
        while Instant::now() < end {
            changed |= s.update();
            std::thread::sleep(Duration::from_millis(10));
        }
        changed
    };
    settle(&mut s, 2000);
    let before = s.last_output;
    s.resize(30, 138).unwrap();
    let repainted = settle(&mut s, 400);
    assert_eq!(s.last_output, before, "the repaint counted as output");
    // Windows repaints on resize; without it this test would prove nothing.
    #[cfg(windows)]
    assert!(repainted, "no repaint arrived");
    let _ = repainted;
}

#[test]
fn scrollback_search_finds_ascii_and_hangul_matches_and_jumps() {
    let p = Project {
        name: "search".into(),
        path: std::env::temp_dir(),
        command: crate::config::shell(),
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
        restart_on_crash: false,
    };
    let mut s = Session::spawn(&p, 5, 40).unwrap();
    s.parser.process(
        "first 오류 line\r\nsecond\r\nthird\r\nfourth\r\nfifth\r\nsixth\r\nseventh ERROR\r\n"
            .as_bytes(),
    );
    let hits = s.search_scrollback("error");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].col_start, 8);
    assert_eq!(hits[0].col_end, 13);

    let kr_hits = s.search_scrollback("오류");
    assert_eq!(kr_hits.len(), 1);
    assert_eq!(kr_hits[0].col_start, 6);
    assert_eq!(kr_hits[0].col_end, 10);

    s.scroll_to_logical_line(kr_hits[0].line);
    assert!(s.scroll > 0);
    let vis = s.visible_search_hits("오류");
    assert_eq!(vis.len(), 1);
    assert_eq!((vis[0].2, vis[0].3), (6, 10));
}

#[test]
fn hook_reports_hold_until_the_user_types_or_work_shows() {
    use crate::session::{hook_verdict, parse_hook_report, parse_hook_state};
    use std::time::{Duration, Instant, UNIX_EPOCH};
    assert_eq!(
        parse_hook_state(
            "done
"
        ),
        Some(AgentPhase::Done)
    );
    assert_eq!(
        parse_hook_state("needs-input"),
        Some(AgentPhase::NeedsInput)
    );
    assert_eq!(parse_hook_state(""), None, "caught mid-write");
    assert_eq!(parse_hook_report("done\n"), Some((AgentPhase::Done, None)));
    assert_eq!(
        parse_hook_report("needs-input 1700000000123456789\n"),
        Some((
            AgentPhase::NeedsInput,
            Some(UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789))
        ))
    );
    assert_eq!(parse_hook_report("needs-input badts\n"), None);

    let typed = Instant::now();
    let later = typed + Duration::from_millis(50);
    let report = Some((AgentPhase::Done, later));
    // Reported after the last key: the hook decides.
    assert_eq!(
        hook_verdict(report, Some(typed), false),
        Some(AgentPhase::Done)
    );
    assert_eq!(hook_verdict(report, None, false), Some(AgentPhase::Done));
    let asking = Some((AgentPhase::NeedsInput, later));
    assert_eq!(
        hook_verdict(asking, Some(typed), false),
        Some(AgentPhase::NeedsInput)
    );
    // The user typed since (answered, interrupted): back to the screen.
    let after = later + Duration::from_millis(50);
    assert_eq!(hook_verdict(asking, Some(after), false), None);
    // Visible work wins too, and no report means no verdict.
    assert_eq!(hook_verdict(report, Some(typed), true), None);
    assert_eq!(hook_verdict(None, Some(typed), false), None);
}

#[test]
fn hook_needs_input_syncs_and_clears_prompt_hint() {
    let dir = scratch_dir("hook-prompt-hint");
    let state_file = dir.join("state");
    let p = Project {
        name: "hook-hint".into(),
        path: dir.clone(),
        command: shell(),
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
        restart_on_crash: false,
    };
    let mut s = Session::spawn(&p, 24, 80).unwrap();
    s.hook_file = Some(state_file.clone());
    s.parser
        .process(b"\x1b[20;1HAllow Read tool on C:\\secret.txt?\r\n");

    let body = crate::ledger::format_state_report(
        crate::ledger::STATE_ASKING,
        std::time::SystemTime::now(),
    );
    std::fs::write(&state_file, body).unwrap();
    assert!(s.read_hook_state());
    assert_eq!(s.phase(), AgentPhase::NeedsInput);
    assert_eq!(
        s.prompt_hint.as_deref(),
        Some("Allow Read tool on C:\\secret.txt?")
    );

    // Subsequent refresh_screen_flags keeps prompt_hint while hook says NeedsInput.
    s.refresh_screen_flags();
    assert_eq!(
        s.prompt_hint.as_deref(),
        Some("Allow Read tool on C:\\secret.txt?")
    );

    // Same-tick overwrite to Done is still detected and clears prompt_hint.
    // The second line carries the transcript path the hook was given.
    let done_body = crate::ledger::format_state_report(
        crate::ledger::STATE_DONE,
        std::time::SystemTime::now() + Duration::from_millis(1),
    ) + "C:\\Users\\me\\.claude\\projects\\x\\s 1.jsonl\n";
    std::fs::write(&state_file, done_body).unwrap();
    assert!(s.read_hook_state());
    assert_eq!(s.phase(), AgentPhase::Done);
    assert!(s.prompt_hint.is_none());
    assert_eq!(
        s.transcript.as_deref(),
        Some(std::path::Path::new(
            "C:\\Users\\me\\.claude\\projects\\x\\s 1.jsonl"
        ))
    );
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pty_capture_deduplicates_output_and_records_prompts() {
    use crate::session::diff_new_lines;
    let lines: Vec<String> = ["a", "b", "c", "d", "e"]
        .into_iter()
        .map(String::from)
        .collect();
    let anchor: Vec<String> = ["b", "c"].into_iter().map(String::from).collect();
    assert_eq!(diff_new_lines(&lines, &anchor), vec!["d", "e"]);
    assert!(diff_new_lines(&lines[..3], &anchor).is_empty());

    let dir = scratch_dir("capture-dedup");
    let p = Project {
        name: "capture".into(),
        path: dir.clone(),
        command: shell(),
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
        restart_on_crash: false,
    };
    let mut s = Session::spawn(&p, 8, 60).unwrap();
    s.input(b"\x1b[A").unwrap();
    s.input(b"fix the bug\r").unwrap();
    assert_eq!(s.take_task_prompts(), vec!["fix the bug"]);
    assert!(s.take_task_prompts().is_empty());

    s.parser.process(b"turn 1 line A\r\nturn 1 line B\r\n");
    let out1 = s.capture_new_output();
    assert!(
        out1.contains("turn 1 line A") && out1.contains("turn 1 line B"),
        "{out1}"
    );
    assert_eq!(s.capture_new_output(), "");

    s.parser.process(b"turn 2 line C\r\n");
    let out2 = s.capture_new_output();
    assert_eq!(out2, "turn 2 line C");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
