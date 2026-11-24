use super::*;
use crate::app::{
    auto_restart_due, crash_log_needs_rotation, App, GHit, GSearch, VaultKindState, VaultModal,
    VaultTarget, AUTO_RESTART_MAX_TRIES, CRASH_LOG_MAX_BYTES,
};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{backend::TestBackend, layout::Size};

fn proj(name: &str, path: &str, cmd: &[&str]) -> Project {
    Project {
        name: name.into(),
        path: PathBuf::from(path),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
        restart_on_crash: false,
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn manager_navigation_filter_preserves_selection_and_renders() {
    let cfg = Config {
        projects: vec![
            proj("alpha", "/tmp/a", &["claude"]),
            proj("beta", "/tmp/b", &["claude"]),
            proj("gamma", "/tmp/c", &["claude"]),
        ],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();

    // Move down to `beta`, jump with `3` to `gamma`, filter to `beta`, then Esc keeps `beta`.
    app.on_key(key(KeyCode::Char('j')), 27, 70);
    assert_eq!(app.selected, 1);
    app.on_key(key(KeyCode::Char('3')), 27, 70);
    assert_eq!(app.selected, 2);

    app.on_key(key(KeyCode::Char('/')), 27, 70);
    assert_eq!(app.mode, Mode::Filter);
    for c in "beta".chars() {
        app.on_key(key(KeyCode::Char(c)), 27, 70);
    }
    assert_eq!(app.order, vec![1]);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(app.mode, Mode::Manager);

    app.on_key(key(KeyCode::Esc), 27, 70);
    assert!(app.filter_buf.is_empty());
    assert_eq!(app.order, vec![0, 1, 2]);
    assert_eq!(app.selected, 1, "dropping filter keeps selected project");

    // Draw in Help mode as well to ensure no layout panics.
    app.on_key(key(KeyCode::Char('?')), 27, 70);
    assert_eq!(app.mode, Mode::Help);
    term.draw(|f| app.draw(f)).unwrap();
}

#[test]
fn add_form_error_keeps_existing_filter() {
    let cfg = Config {
        projects: vec![
            proj("alpha", "/tmp/a", &["claude"]),
            proj("beta", "/tmp/b", &["claude"]),
        ],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    app.on_key(key(KeyCode::Char('/')), 27, 70);
    for c in "beta".chars() {
        app.on_key(key(KeyCode::Char(c)), 27, 70);
    }
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(app.order, vec![1]);

    // Open Add, press Enter on blank path (invalid for Agent), then Esc.
    app.on_key(key(KeyCode::Char('a')), 27, 70);
    assert_eq!(app.mode, Mode::Add);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(!app.add_err.is_empty());
    app.on_key(key(KeyCode::Esc), 27, 70);
    assert_eq!(app.mode, Mode::Manager);
    assert_eq!(app.filter_buf.as_str(), "beta");
    assert_eq!(app.order, vec![1]);
}

#[test]
fn edit_form_preserves_custom_bare_shell_command_and_blocks_move_while_starting() {
    let custom_shell = if cfg!(windows) {
        vec!["cmd.exe".into(), "/k".into()]
    } else {
        vec!["bash".into(), "-l".into()]
    };
    let mut p = proj("demo", "/tmp/demo", &[]);
    p.command = custom_shell.clone();
    let cfg = Config {
        projects: vec![p],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    app.open_edit();
    assert_eq!(
        app.add_cmd.as_str(),
        join_command(&custom_shell),
        "custom bare-shell flags are kept instead of replaced by default shell"
    );

    // Moving a project whose session is starting is blocked cleanly.
    let dir = scratch_dir("edit-starting");
    app.add_path.set(dir.to_str().unwrap());
    app.add_state = crate::pathpick::PathState::Folder;
    app.starting.insert("demo".into(), Vec::new());
    app.save_edit();
    assert!(app.add_err.contains("still starting"), "{}", app.add_err);
    assert_eq!(app.mode, Mode::Add);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn confirm_update_outside_click_dismisses_and_stays_closed() {
    let mut app = App::new(Config::default(), None);
    app.update_offer = Some("v9.9.9".into());
    app.pump();
    assert!(app.confirm_update);

    let area = Size::new(100, 30);
    let mut term = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();

    // Click top-left corner outside the centered modal.
    app.on_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
        area,
        27,
        70,
    );
    assert!(!app.confirm_update);
    assert!(app.update_offer.is_none());

    app.pump();
    assert!(!app.confirm_update, "must not reopen on next pump");
}

#[test]
fn zoom_mode_defers_pane_focus_until_mouseup_so_drag_stays_aligned() {
    let dir = scratch_dir("zoom-drag");
    let mut p = proj("demo", dir.to_str().unwrap(), &[]);
    p.command = shell();
    let cfg = Config {
        projects: vec![p],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let s = Session::spawn(&app.cfg.projects[0], 24, 70).unwrap();
    app.sessions[0] = Some(s);
    app.zoom = true;

    let area = Size::new(100, 30);
    let mut term = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let orig_pane = app.pane_rect;
    let col = orig_pane.x + 2;
    let row = orig_pane.y + 2;

    // Drag inside pane while zoom is enabled in Manager mode: Focus waits until Up (and only if no drag).
    app.on_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), col, row),
        area,
        27,
        70,
    );
    assert_eq!(app.mode, Mode::Manager);
    assert!(app.sel.is_some());
    term.draw(|f| app.draw(f)).unwrap();
    assert_eq!(app.pane_rect, orig_pane);

    app.on_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), col + 4, row),
        area,
        27,
        70,
    );
    assert_eq!(app.mode, Mode::Manager);

    // A plain click (no drag) focuses the session on MouseUp.
    app.sel = None;
    app.on_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), col, row),
        area,
        27,
        70,
    );
    app.on_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), col, row),
        area,
        27,
        70,
    );
    assert_eq!(app.mode, Mode::Focus);

    drop(app);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn manual_stop_records_work_history_and_timer_phase_change_marks_dirty() {
    let dir = scratch_dir("stop-history");
    let mut p = proj("demo", dir.to_str().unwrap(), &[]);
    p.command = if cfg!(windows) {
        vec![
            "cmd.exe".into(),
            "/d".into(),
            "/c".into(),
            "timeout /t 20 /nobreak".into(),
        ]
    } else {
        vec!["sh".into(), "-c".into(), "sleep 20".into()]
    };
    let cfg = Config {
        projects: vec![p],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let mut s = Session::spawn(&app.cfg.projects[0], 24, 80).unwrap();
    s.work_started = Some(std::time::SystemTime::now() - Duration::from_secs(10));
    s.last_phase = AgentPhase::Working;
    app.sessions[0] = Some(s);

    // Timer expiry (e.g. phase returns to Idle/Done) marks dirty even when no new bytes arrive.
    app.dirty = false;
    app.pump();
    assert!(app.dirty, "phase change alone must mark dirty");

    // Stop mid-task writes a history entry.
    let mut s2 = Session::spawn(&app.cfg.projects[0], 24, 80).unwrap();
    s2.work_started = Some(std::time::SystemTime::now() - Duration::from_secs(10));
    s2.parser.process(b"working on task\r\n");
    app.sessions[0] = Some(s2);
    app.stop_slot(0, "stopped by user");
    crate::history::flush(Duration::from_secs(10));
    let entries = crate::ledger::sitreps(&dir);
    assert!(
        entries.iter().any(|(n, _)| crate::history::is_auto(n)),
        "stop_slot recorded work history: {entries:?}"
    );
    drop(app);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn stopped_project_card_zoom_badge_and_tab_complete_work() {
    let mut lead = proj("lead", "/tmp/lead", &["claude"]);
    let mut sub = proj("worker", "/tmp/worker", &["claude"]);
    sub.reports_to = Some("lead".into());
    let mut peer = proj("reviewer", "/tmp/rev", &["claude"]);
    peer.autostart = true;
    let cfg = Config {
        projects: vec![lead.clone(), sub, peer],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();

    // Stopped project renders its info card in the right pane.
    let buf_str = |t: &Terminal<TestBackend>| -> String {
        let b = t.backend().buffer();
        (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let screen = buf_str(&term);
    assert!(screen.contains("Folder"), "{screen}");
    assert!(screen.contains("Command"), "{screen}");

    // Zoom badge shows in Focus mode when zoom is toggled on.
    app.zoom = true;
    app.mode = Mode::Focus;
    term.draw(|f| app.draw(f)).unwrap();
    assert!(buf_str(&term).contains("ZOOM"));

    // Tab-completing superior excludes self and subordinates (no loops) and cycles valid candidates.
    app.mode = Mode::Manager;
    app.selected = 0; // lead (has subordinate `worker`)
    app.on_key(key(KeyCode::Char('u')), 27, 70);
    assert_eq!(app.mode, Mode::Send);
    app.on_key(key(KeyCode::Tab), 27, 70);
    assert_eq!(
        app.send_buf.as_str(),
        "reviewer",
        "only non-looping agent `reviewer` is valid for `lead`"
    );

    // Tab completion also works on Reports to in the Add form.
    app.on_key(key(KeyCode::Esc), 27, 70);
    app.on_key(key(KeyCode::Char('a')), 27, 70);
    app.add_field = 4;
    app.add_under.set("wo");
    app.on_key(key(KeyCode::Tab), 27, 70);
    assert_eq!(app.add_under.as_str(), "worker");

    // Long add_err wraps inside the modal without panicking.
    app.add_err = "x".repeat(120);
    term.draw(|f| app.draw(f)).unwrap();

    // Memory monitor & History popups draw cleanly.
    app.on_key(key(KeyCode::Esc), 27, 70);
    app.on_key(key(KeyCode::Char('M')), 27, 70);
    assert!(app.procs.is_some());
    term.draw(|f| app.draw(f)).unwrap();
    let mem_screen = buf_str(&term);
    assert!(mem_screen.contains("RAM") && mem_screen.contains("Process"));

    // Press `r` to refresh memory monitor while keeping selection intact.
    app.on_key(key(KeyCode::Char('r')), 27, 70);
    assert!(app.procs.is_some());
    app.on_key(key(KeyCode::Esc), 27, 70);

    // History popup renders both auto and SITREP entries cleanly.
    let hist_dir = scratch_dir("hist-ui");
    crate::ledger::ensure(&hist_dir).unwrap();
    fs::write(
        hist_dir.join(".squad/log/20261108T061200Z-auto.md"),
        "auto body",
    )
    .unwrap();
    fs::write(
        hist_dir.join(".squad/log/20261108T061300Z-sitrep.md"),
        "sitrep body",
    )
    .unwrap();
    lead.path = hist_dir.clone();
    app.cfg.projects[0] = lead;
    app.selected = 0;
    app.on_key(key(KeyCode::Char('h')), 27, 70);
    assert!(app.history.is_some());
    term.draw(|f| app.draw(f)).unwrap();
    let hist_screen = buf_str(&term);
    assert!(hist_screen.contains("History: lead (2)"));

    // `/` filters the entry list: typing narrows, Enter keeps, Esc clears.
    app.on_key(key(KeyCode::Char('/')), 27, 70);
    for c in "auto".chars() {
        app.on_key(key(KeyCode::Char(c)), 27, 70);
    }
    term.draw(|f| app.draw(f)).unwrap();
    let narrowed = buf_str(&term);
    assert!(
        narrowed.contains("(1/2"),
        "filtered count shows: {narrowed:?}"
    );
    assert!(
        !narrowed.contains("sitrep"),
        "other entries hide: {narrowed:?}"
    );
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(app.history.is_some(), "Enter keeps the kept filter");
    // Reopening `/` and escaping restores the full list; Esc then closes.
    app.on_key(key(KeyCode::Char('/')), 27, 70);
    for c in "qqq".chars() {
        app.on_key(key(KeyCode::Char(c)), 27, 70);
    }
    term.draw(|f| app.draw(f)).unwrap();
    assert!(buf_str(&term).contains("(0/2"), "no entry matches `qqq`");
    app.on_key(key(KeyCode::Esc), 27, 70);
    term.draw(|f| app.draw(f)).unwrap();
    assert!(buf_str(&term).contains("History: lead (2)"));
    app.on_key(key(KeyCode::Esc), 27, 70);
    assert!(app.history.is_none(), "idle Esc closes the popup");
    drop(app);
    let _ = fs::remove_dir_all(&hist_dir);
}

#[test]
fn crash_log_rotates_past_its_cap() {
    assert!(!crash_log_needs_rotation(0));
    assert!(!crash_log_needs_rotation(CRASH_LOG_MAX_BYTES));
    assert!(crash_log_needs_rotation(CRASH_LOG_MAX_BYTES + 1));
}

#[test]
fn global_search_lists_sessions_and_jumps() {
    let cfg = Config {
        projects: vec![
            proj("alpha", "/tmp/a", &["claude"]),
            proj("beta", "/tmp/b", &["claude"]),
        ],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let screen = |t: &Terminal<TestBackend>| -> String {
        let b = t.backend().buffer();
        (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    // `G` opens an empty overlay explaining itself; Esc closes it.
    app.on_key(key(KeyCode::Char('G')), 27, 70);
    assert!(app.gsearch.is_some());
    term.draw(|f| app.draw(f)).unwrap();
    assert!(screen(&term).contains("Find in all sessions"));
    // Typing with no sessions running stays empty without panicking.
    for c in "x".chars() {
        app.on_key(key(KeyCode::Char(c)), 27, 70);
    }
    term.draw(|f| app.draw(f)).unwrap();
    assert!(screen(&term).contains("No matches anywhere"));
    // Enter with no hits keeps the popup open; Esc closes it.
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(app.gsearch.is_some());
    app.on_key(key(KeyCode::Esc), 27, 70);
    assert!(app.gsearch.is_none());

    // Fabricated hits render with counts; clicking a new row picks it,
    // clicking the picked row jumps with the query loaded.
    app.on_key(key(KeyCode::Char('G')), 27, 70);
    app.gsearch = Some(GSearch {
        query: "boom".into(),
        hits: vec![GHit { pi: 0, count: 1 }, GHit { pi: 1, count: 2 }],
        sel: 0,
    });
    term.draw(|f| app.draw(f)).unwrap();
    let rendered = screen(&term);
    assert!(rendered.contains("boom"), "query shows: {rendered:?}");
    assert!(rendered.contains("2 matches"), "counts show: {rendered:?}");
    let area = Size::new(100, 30);
    let pop = gsearch_popup(area.into(), 2);
    // Second hit row is one row below the first.
    app.on_mouse(
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            pop.x + 2,
            pop.y + 2,
        ),
        area,
        27,
        70,
    );
    assert!(app.gsearch.is_some(), "picking a new row keeps it open");
    assert_eq!(app.gsearch.as_ref().unwrap().sel, 1);
    app.on_mouse(
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            pop.x + 2,
            pop.y + 2,
        ),
        area,
        27,
        70,
    );
    assert!(app.gsearch.is_none(), "clicking the picked row jumps");
    assert_eq!(app.selected, 1, "jump selects `beta`");
}

#[test]
fn crashed_services_restart_up_to_five_times_then_stop() {
    use std::time::Duration;
    assert!(auto_restart_due(0, Duration::from_secs(6)));
    assert!(!auto_restart_due(0, Duration::from_secs(4)));
    assert!(!auto_restart_due(
        AUTO_RESTART_MAX_TRIES,
        Duration::from_secs(600)
    ));

    fn crashed(name: &str, kind: Kind, flag: bool, dir: &std::path::Path) -> (Config, Session) {
        let command: Vec<String> = if cfg!(windows) {
            vec!["cmd".into(), "/c".into(), "exit".into(), "3".into()]
        } else {
            vec!["sh".into(), "-c".into(), "exit 3".into()]
        };
        let p = Project {
            name: name.into(),
            path: dir.to_path_buf(),
            command,
            kind,
            autostart: false,
            ledger: false,
            reports_to: None,
            pinned: false,
            restart_on_crash: flag,
        };
        let mut s = Session::spawn(&p, 24, 80).unwrap();
        for _ in 0..100 {
            s.update();
            if !s.status.is_running() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            matches!(s.status, SessionStatus::Exited(_)) && s.status.needs_attention(),
            "the fixture session must crash, got {}",
            s.status
        );
        (
            Config {
                projects: vec![p],
                ..Default::default()
            },
            s,
        )
    }
    let dir = scratch_dir("autorestart");

    // A cooled-down streak schedules a restart on the next pump.
    let (cfg, s) = crashed("flaky", Kind::Service, true, &dir);
    let mut app = App::new(cfg, None);
    app.sessions[0] = Some(s);
    app.auto_restarts.insert(
        "flaky".into(),
        (0, Instant::now() - Duration::from_secs(30)),
    );
    app.pane = (24, 80);
    app.pump();
    assert!(app.starting.contains_key("flaky"), "crash restarts");
    assert_eq!(app.auto_restarts.get("flaky").map(|(t, _)| *t), Some(1));
    assert!(
        app.message.0.contains("auto-restart #1"),
        "{}",
        app.message.0
    );
    drop(app);

    // A capped streak stays dead and says so once.
    let (cfg, s) = crashed("flaky", Kind::Service, true, &dir);
    let mut app = App::new(cfg, None);
    app.sessions[0] = Some(s);
    app.auto_restarts.insert(
        "flaky".into(),
        (
            AUTO_RESTART_MAX_TRIES,
            Instant::now() - Duration::from_secs(30),
        ),
    );
    app.pane = (24, 80);
    app.pump();
    assert!(
        !app.starting.contains_key("flaky"),
        "capped streak stays dead"
    );
    assert!(
        app.message.0.contains("Gave up"),
        "says why: {}",
        app.message.0
    );
    drop(app);

    // Opted-out services never restart.
    let (cfg, s) = crashed("plain", Kind::Service, false, &dir);
    let mut app = App::new(cfg, None);
    app.sessions[0] = Some(s);
    app.auto_restarts.insert(
        "plain".into(),
        (0, Instant::now() - Duration::from_secs(30)),
    );
    app.pane = (24, 80);
    app.pump();
    assert!(!app.starting.contains_key("plain"));
    drop(app);

    // Agents never restart, even when flagged.
    let (cfg, s) = crashed("helper", Kind::Agent, true, &dir);
    let mut app = App::new(cfg, None);
    app.sessions[0] = Some(s);
    app.auto_restarts.insert(
        "helper".into(),
        (0, Instant::now() - Duration::from_secs(30)),
    );
    app.pane = (24, 80);
    app.pump();
    assert!(!app.starting.contains_key("helper"));
    drop(app);

    // A removed project's capped streak goes with it: it never lands on
    // the project that takes its place in the list.
    let (cfg, s) = crashed("flaky", Kind::Service, true, &dir);
    let mut app = App::new(cfg, None);
    app.sessions[0] = Some(s);
    app.auto_restarts
        .insert("removed".into(), (AUTO_RESTART_MAX_TRIES, Instant::now()));
    app.pane = (24, 80);
    app.pump();
    assert!(!app.auto_restarts.contains_key("removed"));
    assert_eq!(app.auto_restarts.get("flaky").map(|(t, _)| *t), Some(0));
    assert!(!app.message.0.contains("Gave up"), "{}", app.message.0);
    drop(app);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ui_ux_context_menu_nav_attention_jump_service_add_and_80col_pane() {
    // 1. Empty pane left-click opens Add form.
    let mut empty = App::new(Config::default(), None);
    let mut term80 = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let area80 = Size::new(80, 24);
    term80.draw(|f| empty.draw(f)).unwrap();
    empty.on_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 45, 10),
        area80,
        24,
        54,
    );
    assert_eq!(empty.mode, Mode::Add);

    // 2. Service Add form skips Reports-to (field 4) on Tab/Down and reaches Env (5), Skills (6), Tools (7).
    empty.add_kind = Kind::Service;
    empty.add_field = 3; // Autostart
    empty.on_key(key(KeyCode::Tab), 24, 54);
    assert_eq!(
        empty.add_field, 5,
        "Tab from Autostart on Service skips Reports-to and goes to Env"
    );
    empty.on_key(key(KeyCode::Tab), 24, 54);
    assert_eq!(empty.add_field, 6, "Tab from Env goes to Skills");
    empty.on_key(key(KeyCode::Tab), 24, 54);
    assert_eq!(empty.add_field, 7, "Tab from Skills goes to Tools");
    empty.on_key(key(KeyCode::Tab), 24, 54);
    assert_eq!(empty.add_field, 8, "Tab from Tools goes to References");
    empty.on_key(key(KeyCode::Tab), 24, 54);
    assert_eq!(empty.add_field, 0, "Tab from References wraps to Path");
    empty.on_key(key(KeyCode::BackTab), 24, 54);
    assert_eq!(empty.add_field, 8, "BackTab from Path wraps to References");
    empty.on_key(key(KeyCode::BackTab), 24, 54);
    assert_eq!(empty.add_field, 7, "BackTab from References goes to Tools");
    empty.on_key(key(KeyCode::BackTab), 24, 54);
    assert_eq!(empty.add_field, 6, "BackTab from Tools goes to Skills");
    empty.on_key(key(KeyCode::BackTab), 24, 54);
    assert_eq!(empty.add_field, 5, "BackTab from Skills goes to Env");
    empty.on_key(key(KeyCode::BackTab), 24, 54);
    assert_eq!(
        empty.add_field, 3,
        "BackTab from Env on Service skips Reports-to to Autostart"
    );
    term80.draw(|f| empty.draw(f)).unwrap();

    // 3. Context menu keyboard navigation (Down/j/Up/k wrapping + Enter executes selection).
    let dir = scratch_dir("ux-suite");
    crate::ledger::ensure(&dir).unwrap();
    fs::write(dir.join(".squad/log/20261108T061300Z-sitrep.md"), "sitrep").unwrap();
    let mut p0 = proj("alpha", dir.to_str().unwrap(), &["claude"]);
    p0.ledger = true;
    let p1 = proj("beta", dir.to_str().unwrap(), &["claude"]);
    let cfg = Config {
        projects: vec![p0, p1],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    app.context_menu = Some(0);
    app.context_sel = 0;
    app.on_key(key(KeyCode::Up), 24, 54);
    assert_eq!(
        app.context_sel, 10,
        "Up wraps to last stopped menu item (References)"
    );
    app.on_key(key(KeyCode::Char('j')), 24, 54);
    assert_eq!(app.context_sel, 0, "j wraps back to first item");
    // Item index 3 on stopped menu is View history.
    for _ in 0..3 {
        app.on_key(key(KeyCode::Down), 24, 54);
    }
    assert_eq!(app.context_sel, 3);
    app.on_key(key(KeyCode::Enter), 24, 54);
    assert!(app.context_menu.is_none());
    assert!(
        app.history.is_some(),
        "Enter executed selected item (View history)"
    );
    app.on_key(key(KeyCode::Esc), 24, 54);

    // 4. Filter mode allows left-clicking a sidebar row to exit Filter and select it.
    app.selected = 0;
    app.on_key(key(KeyCode::Char('/')), 24, 54);
    assert_eq!(app.mode, Mode::Filter);
    term80.draw(|f| app.draw(f)).unwrap();
    app.on_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 4, 3),
        area80,
        24,
        54,
    );
    assert_eq!(app.mode, Mode::Manager);
    assert_eq!(app.selected, 1);

    // 5. Tab in Manager mode jumps to project needing attention (failed or running).
    let mut bad = Session::spawn(
        &Project {
            command: if cfg!(windows) {
                vec!["cmd.exe".into(), "/c".into(), "exit 1".into()]
            } else {
                vec!["sh".into(), "-c".into(), "exit 1".into()]
            },
            ..app.cfg.projects[0].clone()
        },
        24,
        54,
    )
    .unwrap();
    bad.status = crate::session::SessionStatus::Exited(1);
    app.sessions[0] = Some(bad);
    app.selected = 1;
    app.on_key(key(KeyCode::Tab), 24, 54);
    assert_eq!(app.selected, 0, "Tab jumps to Failed project");

    // 6. 80x24 pane renders scroll_note in split view and ZOOM when zoomed.
    app.mode = Mode::Focus;
    app.zoom = false;
    app.selected = 0;
    if let Some(s) = app.sessions[0].as_mut() {
        s.status = crate::session::SessionStatus::Running;
        for i in 0..40 {
            s.parser.process(format!("line {i}\r\n").as_bytes());
        }
        s.parser.screen_mut().set_scrollback(4);
    }
    term80.draw(|f| app.draw(f)).unwrap();
    let buf_to_string = |t: &Terminal<TestBackend>| -> String {
        let b = t.backend().buffer();
        (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let screen80 = buf_to_string(&term80);
    assert!(
        screen80.contains("history 4"),
        "80-col split terminal shows scroll_note in bottom border:\n{screen80}"
    );
    app.zoom = true;
    term80.draw(|f| app.draw(f)).unwrap();
    let screen80_zoom = buf_to_string(&term80);
    assert!(
        screen80_zoom.contains("ZOOM"),
        "80-col terminal shows ZOOM:\n{screen80_zoom}"
    );

    drop(app);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn env_vault_registers_shares_and_applies_to_project_dotenv() {
    let dir = scratch_dir("env-modal-e2e");
    let proj_a = dir.join("agent-a");
    let proj_b = dir.join("agent-b");
    fs::create_dir_all(&proj_a).unwrap();
    fs::create_dir_all(&proj_b).unwrap();
    let env_file = dir.join("env.toml");

    // Isolate the vault path from the user's real env.toml without touching AGENTSQUAD_CONFIG.
    env::set_var("AGENTSQUAD_ENV", &env_file);

    let cfg = Config {
        projects: vec![
            proj("agent-a", proj_a.to_str().unwrap(), &["claude"]),
            proj("agent-b", proj_b.to_str().unwrap(), &["claude"]),
        ],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();

    // Open Shared Env Vault via `E` on agent-a.
    app.selected = 0;
    app.on_key(key(KeyCode::Char('E')), 27, 70);
    assert!(app.vault_modal.as_ref().is_some_and(VaultModal::is_env));
    // Vault is empty, so inline editor opens on Key automatically.
    for ch in "SHARED_API_KEY".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in "secret-12345".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(app.env_vault.vars.len(), 1);
    assert_eq!(app.env_vault.vars[0].key, "SHARED_API_KEY");
    // Newly created entry is persisted & auto-checked; Enter writes agent-a/.env.
    term.draw(|f| app.draw(f)).unwrap();
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(app.vault_modal.is_none());
    let dotenv_a = fs::read_to_string(proj_a.join(".env")).unwrap();
    assert!(
        dotenv_a.contains("SHARED_API_KEY=secret-12345"),
        "{dotenv_a}"
    );

    // Switch to agent-b, open `E`, verify shared entry is reusable without retyping, pick with Space, apply.
    app.selected = 1;
    app.on_key(key(KeyCode::Char('E')), 27, 70);
    assert!(!app.vault_modal.as_ref().unwrap().has_editor());
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    let dotenv_b = fs::read_to_string(proj_b.join(".env")).unwrap();
    assert!(
        dotenv_b.contains("SHARED_API_KEY=secret-12345"),
        "{dotenv_b}"
    );

    // Reopening shows assigned keys checked; unchecking removes only that
    // key, leaving vars that are not in the vault alone.
    fs::write(proj_b.join(".env"), format!("{dotenv_b}LOCAL_ONLY=1\n")).unwrap();
    app.on_key(key(KeyCode::Char('E')), 27, 70);
    assert!(app
        .vault_modal
        .as_ref()
        .unwrap()
        .picked
        .contains("SHARED_API_KEY"));
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    term.draw(|f| app.draw(f)).unwrap();
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(app.vault_modal.is_none());
    let unassigned = fs::read_to_string(proj_b.join(".env")).unwrap();
    assert_eq!(unassigned, "LOCAL_ONLY=1\n");
    assert!(app.message.0.contains("-1 removed"), "{}", app.message.0);
    // Enter with nothing changed writes nothing.
    app.on_key(key(KeyCode::Char('E')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(
        fs::read_to_string(proj_b.join(".env")).unwrap(),
        "LOCAL_ONLY=1\n"
    );

    // Also test selecting from Add modal's Env (.env) field (field 5).
    app.on_key(key(KeyCode::Char('a')), 27, 70);
    assert_eq!(app.mode, Mode::Add);
    app.add_field = 5;
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    assert!(app.vault_modal.as_ref().is_some_and(VaultModal::is_env));
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(app.vault_modal.is_none());
    assert!(app.add_env.contains("SHARED_API_KEY"));
    term.draw(|f| app.draw(f)).unwrap();
    let proj_c = dir.join("agent-c");
    fs::create_dir_all(&proj_c).unwrap();
    assert_eq!(app.apply_add_env(&proj_c), Ok(Some((1, 0))));
    let dotenv_c = fs::read_to_string(proj_c.join(".env")).unwrap();
    assert!(
        dotenv_c.contains("SHARED_API_KEY=secret-12345"),
        "{dotenv_c}"
    );

    // Verify wants_ascii_ime toggles accurately across shortcut vs text-entry modes.
    app.mode = Mode::Manager;
    assert!(app.wants_ascii_ime());
    app.mode = Mode::Focus;
    assert!(!app.wants_ascii_ime());
    app.mode = Mode::Send;
    assert!(!app.wants_ascii_ime());
    app.mode = Mode::Filter;
    assert!(!app.wants_ascii_ime());
    app.mode = Mode::Add;
    app.add_field = 0; // Path text field
    assert!(!app.wants_ascii_ime());
    app.add_field = 1; // Kind toggle
    assert!(app.wants_ascii_ime());
    app.add_field = 5; // Env picker field
    assert!(app.wants_ascii_ime());
    app.on_key(key(KeyCode::Esc), 27, 70);
    app.on_key(key(KeyCode::Char('ㅁ')), 27, 70); // Hangul 'ㅁ' -> 'a' (Add)
    assert_eq!(app.mode, Mode::Add);

    // Once the vault has entries, adding more is a visible row below the list.
    app.on_key(key(KeyCode::Esc), 27, 70);
    app.on_key(key(KeyCode::Char('E')), 27, 70);
    let before = app.env_vault.vars.len();
    assert!(before > 0 && !app.vault_modal.as_ref().unwrap().has_editor());
    term.draw(|f| app.draw(f)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(screen.contains("+ New variable"), "add row is shown");
    app.on_key(key(KeyCode::Up), 27, 70); // wraps from the top onto the row
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(
        app.vault_modal.as_ref().unwrap().has_editor(),
        "Enter on the row opens a blank form instead of writing .env"
    );
    for c in "EXTRA_KEY".chars() {
        app.on_key(key(KeyCode::Char(c)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    app.on_key(key(KeyCode::Char('1')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(app.env_vault.vars.len(), before + 1);

    // `a` inside VaultModal (Env) opens the Add variable editor (`A` toggles all);
    // local-only `.env` keys also render in the modal list.
    app.on_key(key(KeyCode::Esc), 27, 70);
    app.selected = 1; // agent-b has LOCAL_ONLY=1 in .env
    app.on_key(key(KeyCode::Char('E')), 27, 70);
    term.draw(|f| app.draw(f)).unwrap();
    let modal_screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        modal_screen.contains("LOCAL_ONLY") && modal_screen.contains("local"),
        "local-only .env entry shows in EnvModal"
    );
    app.on_key(key(KeyCode::Char('a')), 27, 70);
    assert!(app.vault_modal.as_ref().unwrap().has_editor());
    app.on_key(key(KeyCode::Esc), 27, 70);
    app.on_key(key(KeyCode::Char('A')), 27, 70);
    assert_eq!(
        app.vault_modal.as_ref().unwrap().picked.len(),
        app.env_vault.vars.len()
    );
    app.on_key(key(KeyCode::Esc), 27, 70);

    // Edit project (`e`) loads existing `.env` vault keys into `add_env`,
    // and Enter on the Env (.env) row opens the picker with `.env` badges.
    app.selected = 0; // agent-a has SHARED_API_KEY
    app.open_edit();
    assert!(app.add_env.contains("SHARED_API_KEY"));
    app.add_field = 5;
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(app.vault_modal.as_ref().is_some_and(VaultModal::is_env));
    assert!(app
        .vault_modal
        .as_ref()
        .unwrap()
        .project_dotenv()
        .unwrap()
        .contains_key("SHARED_API_KEY"));
    app.on_key(key(KeyCode::Esc), 27, 70);
    app.on_key(key(KeyCode::Esc), 27, 70);

    // sync_project_dotenv removes unchecked vault keys while keeping local-only keys.
    let cur_b = crate::envvault::read_dotenv(&proj_b);
    let mut keep_none = std::collections::BTreeSet::new();
    keep_none.insert("SHARED_API_KEY".to_string());
    assert_eq!(
        app.sync_project_dotenv(&proj_b, &keep_none, &cur_b),
        Ok((1, 0, 0))
    );
    let cur_b2 = crate::envvault::read_dotenv(&proj_b);
    assert_eq!(
        app.sync_project_dotenv(&proj_b, &std::collections::BTreeSet::new(), &cur_b2),
        Ok((0, 0, 1))
    );
    assert_eq!(
        fs::read_to_string(proj_b.join(".env")).unwrap(),
        "LOCAL_ONLY=1\n"
    );

    drop(app);
    env::remove_var("AGENTSQUAD_ENV");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn arrows_reach_management_and_right_click_takes_focus_with_env_info() {
    let dir = scratch_dir("mgmt-nav");
    fs::write(dir.join(".env"), "API_KEY=supersecret\nPORT=8080\n").unwrap();
    let path = dir.to_str().unwrap();
    let cfg = Config {
        projects: vec![
            proj("alpha", path, &["claude"]),
            proj("beta", path, &["claude"]),
        ],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    // Down past the last project steps onto Management (Memory, Env, Skills, Tools, Refs), then wraps to top.
    app.on_key(key(KeyCode::Down), 24, 54);
    assert_eq!((app.selected, app.mgmt_sel), (1, None));
    app.on_key(key(KeyCode::Down), 24, 54);
    assert_eq!(app.mgmt_sel, Some(0));
    app.on_key(key(KeyCode::Char('ㅓ')), 24, 54); // Hangul j -> Env vault
    assert_eq!(app.mgmt_sel, Some(1));
    app.on_key(key(KeyCode::Down), 24, 54); // Skills
    assert_eq!(app.mgmt_sel, Some(2));
    app.on_key(key(KeyCode::Down), 24, 54); // Tools
    assert_eq!(app.mgmt_sel, Some(3));
    app.on_key(key(KeyCode::Down), 24, 54); // References
    assert_eq!(app.mgmt_sel, Some(4));
    app.on_key(key(KeyCode::Down), 24, 54);
    assert_eq!((app.selected, app.mgmt_sel), (0, None));
    app.on_key(key(KeyCode::Up), 24, 54);
    assert_eq!(app.mgmt_sel, Some(4), "Up from the top reaches References");
    app.on_key(key(KeyCode::Up), 24, 54);
    assert_eq!(app.mgmt_sel, Some(3), "Up reaches Tools");
    app.on_key(key(KeyCode::Up), 24, 54);
    assert_eq!(app.mgmt_sel, Some(2), "Up reaches Skills");
    app.on_key(key(KeyCode::Up), 24, 54);
    assert_eq!(app.mgmt_sel, Some(1), "Up reaches Env vault");
    app.on_key(key(KeyCode::Up), 24, 54);
    assert_eq!(app.mgmt_sel, Some(0));
    // Project actions have no target on a Management row.
    app.on_key(key(KeyCode::Char('d')), 24, 54);
    assert!(app.confirm_remove.is_none());
    app.on_key(key(KeyCode::Enter), 24, 54);
    assert!(app.procs.is_some(), "Enter opens the Memory monitor");
    app.procs = None;
    app.on_key(key(KeyCode::Esc), 24, 54);
    assert_eq!(app.mgmt_sel, None);

    // Right-click on a sidebar row while a terminal has focus: the manager
    // takes the keyboard and the row's menu opens.
    let area = Size::new(80, 24);
    let mut term = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    app.mode = Mode::Focus;
    term.draw(|f| app.draw(f)).unwrap();
    app.on_mouse(
        mouse(MouseEventKind::Down(MouseButton::Right), 4, 3),
        area,
        24,
        54,
    );
    assert_eq!(app.mode, Mode::Manager);
    assert_eq!(app.context_menu, Some(1));
    assert_eq!(app.selected, 1);

    // Last stopped-menu row shows the agent's environment, masked.
    app.on_key(key(KeyCode::Char('8')), 24, 54);
    assert!(app.context_menu.is_none());
    let text = |app: &App| {
        let info = app.env_info.as_ref().unwrap();
        env_info_lines(&app.cfg.projects[info.pi], &info.dotenv, info.reveal)
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let shown = text(&app);
    assert!(
        shown.contains("API_KEY") && shown.contains("PORT"),
        "{shown}"
    );
    assert!(!shown.contains("supersecret"), "values masked by default");
    term.draw(|f| app.draw(f)).unwrap();
    app.on_key(key(KeyCode::Char('v')), 24, 54);
    assert!(text(&app).contains("supersecret"), "v reveals");
    // Pressing `a` inside Environment variables jumps straight to adding an env var.
    app.on_key(key(KeyCode::Char('a')), 24, 54);
    assert!(app.env_info.is_none());
    assert!(app
        .vault_modal
        .as_ref()
        .is_some_and(|m| m.is_env() && m.has_editor()));
    app.vault_modal = None;
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collapsible_mini_sidebar_keyboard_and_mouse_interactions() {
    let mut svc = proj("api", "/tmp/api", &["npm", "run", "dev"]);
    svc.kind = Kind::Service;
    let cfg = Config {
        projects: vec![
            proj("alpha", "/tmp/a", &["claude"]),
            proj("beta", "/tmp/b", &["claude"]),
            svc,
        ],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let area = Size::new(100, 30);
    let mut term = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    let buf_str = |t: &Terminal<TestBackend>| -> String {
        let b = t.backend().buffer();
        (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    term.draw(|f| app.draw(f)).unwrap();
    assert!(!app.sidebar_collapsed);
    assert_eq!(app.side_rect.width, sidebar_width(area.width));
    assert_eq!(app.pane_rect.width, area.width - sidebar_width(area.width));
    assert!(buf_str(&term).contains("[<]"));

    // Keyboard toggle: `[` / `]` and `<` / `>`.
    app.on_key(key(KeyCode::Char('[')), 27, 92);
    assert!(app.sidebar_collapsed);
    term.draw(|f| app.draw(f)).unwrap();
    assert_eq!(app.side_rect.width, COLLAPSED_SIDEBAR_WIDTH);
    assert_eq!(app.pane_rect.width, area.width - COLLAPSED_SIDEBAR_WIDTH);
    let collapsed_screen = buf_str(&term);
    assert!(collapsed_screen.contains("[>]"));
    assert!(collapsed_screen.contains("A 0/2"));
    assert!(collapsed_screen.contains("S 0/1"));
    assert!(collapsed_screen.contains("Mgmt"));

    app.on_key(key(KeyCode::Char(']')), 27, 75);
    assert!(!app.sidebar_collapsed);
    app.on_key(key(KeyCode::Char('<')), 27, 92);
    assert!(app.sidebar_collapsed);
    app.on_key(key(KeyCode::Char('>')), 27, 75);
    assert!(!app.sidebar_collapsed);
    term.draw(|f| app.draw(f)).unwrap();
    assert_eq!(app.side_rect.width, sidebar_width(area.width));

    // Click `[<]` on top border to collapse.
    app.on_mouse(
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            app.side_rect.right().saturating_sub(2),
            app.side_rect.y,
        ),
        area,
        27,
        75,
    );
    assert!(app.sidebar_collapsed);
    term.draw(|f| app.draw(f)).unwrap();
    assert_eq!(app.side_rect.width, COLLAPSED_SIDEBAR_WIDTH);

    // Clicking project row 2 (row 3: border 0, A header 1, alpha 2, beta 3) selects beta.
    app.on_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 2, 3),
        area,
        27,
        92,
    );
    assert_eq!(app.selected, 1);

    // Right-clicking project row 1 (row 2) opens its context menu.
    app.on_mouse(
        mouse(MouseEventKind::Down(MouseButton::Right), 2, 2),
        area,
        27,
        92,
    );
    assert_eq!(app.selected, 0);
    assert_eq!(app.context_menu, Some(0));
    app.on_key(key(KeyCode::Esc), 27, 92);
    assert!(app.context_menu.is_none());

    // Clicking `[>]` while in Focus mode expands sidebar without dropping terminal focus.
    app.mode = Mode::Focus;
    app.on_mouse(
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            app.side_rect.x + 2,
            app.side_rect.y,
        ),
        area,
        27,
        92,
    );
    assert!(!app.sidebar_collapsed);
    assert_eq!(app.mode, Mode::Focus);
    term.draw(|f| app.draw(f)).unwrap();
    assert_eq!(app.side_rect.width, sidebar_width(area.width));
}

#[test]
fn quick_reply_opens_via_badge_and_cycles_waiting_agents() {
    let dir = scratch_dir("quick-reply");
    let mut p1 = proj("agent-a", dir.to_str().unwrap(), &[]);
    p1.command = shell();
    let mut p2 = proj("agent-b", dir.to_str().unwrap(), &[]);
    p2.command = shell();
    let cfg = Config {
        projects: vec![p1, p2],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let mut s1 = Session::spawn(&app.cfg.projects[0], 24, 70).unwrap();
    let mut s2 = Session::spawn(&app.cfg.projects[1], 24, 70).unwrap();
    s1.parser.process(b"Allow edit? [y/n]\r\n");
    s2.parser.process(b"Do you want to proceed?\r\n1. Yes\r\n");
    s1.cached_needs_input = true;
    s2.cached_needs_input = true;
    s1.prompt_hint = Some("Allow edit? [y/n]".into());
    s2.prompt_hint = Some("Do you want to proceed?".into());
    s1.last_output = Some(Instant::now() - Duration::from_secs(2));
    s2.last_output = Some(Instant::now() - Duration::from_secs(2));
    app.sessions[0] = Some(s1);
    app.sessions[1] = Some(s2);

    let area = Size::new(100, 30);
    let mut term = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    app.mode = Mode::Focus;
    term.draw(|f| app.draw(f)).unwrap();
    assert!(app.wait_badge_rect.width > 0);

    // Clicking `? 2` in the status bar opens Quick Reply even from Focus mode.
    app.on_mouse(
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            app.wait_badge_rect.x + 1,
            app.wait_badge_rect.y,
        ),
        area,
        27,
        75,
    );
    assert!(app.quick_reply.is_some());
    assert_eq!(app.quick_reply.as_ref().unwrap().pi, 0);
    assert!(app.wants_ascii_ime());
    term.draw(|f| app.draw(f)).unwrap();

    // Tab cycles to the second waiting agent.
    app.on_key(key(KeyCode::Tab), 27, 75);
    assert_eq!(app.quick_reply.as_ref().unwrap().pi, 1);

    // Pressing `y` replies and auto-advances to the remaining waiting agent (0).
    app.on_key(key(KeyCode::Char('y')), 27, 75);
    assert_eq!(app.quick_reply.as_ref().map(|q| q.pi), Some(0));

    // Pressing `t` switches to custom reply input (allowing Hangul IME), Enter sends & closes.
    app.on_key(key(KeyCode::Char('t')), 27, 75);
    assert!(!app.wants_ascii_ime());
    // Not "yes": these are real shells, and `yes` would flood the pty forever.
    for c in "ok".chars() {
        app.on_key(key(KeyCode::Char(c)), 27, 75);
    }
    app.on_key(key(KeyCode::Enter), 27, 75);
    assert!(app.quick_reply.is_none());

    drop(app);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn git_view_lists_changes_and_renders_diff() {
    let dir = scratch_dir("git-modal");
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    fs::write(dir.join("tracked.txt"), "one\n").unwrap();
    git(&["add", "tracked.txt"]);
    git(&["commit", "-q", "-m", "init"]);
    fs::write(dir.join("tracked.txt"), "one\ntwo\n").unwrap();
    fs::write(dir.join("untracked.txt"), "hello\n").unwrap();

    let cfg = Config {
        projects: vec![proj("repo", dir.to_str().unwrap(), &["claude"])],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();

    // git runs on the viewer's worker: opening returns at once, the list follows.
    let settle = |app: &mut App| {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while app.git_view.as_ref().is_some_and(|g| g.busy()) && std::time::Instant::now() < end {
            app.pump();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    app.on_key(key(KeyCode::Char('g')), 27, 75);
    assert!(app.git_view.as_ref().expect("git view opens").busy());
    settle(&mut app);
    let gv = app.git_view.as_ref().unwrap();
    assert_eq!(gv.files.len(), 2);
    let tracked_at = gv
        .files
        .iter()
        .position(|(_, f)| f.ends_with("tracked.txt") && !f.ends_with("untracked.txt"))
        .unwrap();
    app.git_view.as_mut().unwrap().select(tracked_at);
    settle(&mut app);
    assert!(app.git_view.as_ref().unwrap().diff.contains("+two"));
    term.draw(|f| app.draw(f)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(screen.contains("Git: repo"));
    assert!(screen.contains("tracked.txt"));
    assert!(screen.contains("untracked.txt"));

    app.on_key(key(KeyCode::Home), 27, 75);
    app.on_key(key(KeyCode::Down), 27, 75);
    assert_eq!(app.git_view.as_ref().unwrap().sel, 1);
    app.on_key(key(KeyCode::Esc), 27, 75);
    assert!(app.git_view.is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collapsed_hover_tooltip_and_terminal_scrollback_search() {
    let dir = scratch_dir("hover-search");
    let mut p1 = proj("alpha", dir.to_str().unwrap(), &[]);
    p1.command = shell();
    let mut p2 = proj("beta", dir.to_str().unwrap(), &[]);
    p2.command = shell();
    let cfg = Config {
        projects: vec![p1, p2],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let mut s = Session::spawn(&app.cfg.projects[0], 5, 60).unwrap();
    for i in 0..10 {
        let line = if i == 1 || i == 8 {
            format!("row {i} needle-here\r\n")
        } else {
            format!("row {i}\r\n")
        };
        s.parser.process(line.as_bytes());
    }
    app.sessions[0] = Some(s);

    let area = Size::new(100, 30);
    let mut term = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();

    // Collapse sidebar and hover row 2 (`beta` at row 3).
    app.on_key(key(KeyCode::Char('[')), 27, 92);
    app.message.0 = app.hints.clone();
    term.draw(|f| app.draw(f)).unwrap();
    app.on_mouse(mouse(MouseEventKind::Moved, 2, 3), area, 27, 92);
    assert_eq!(app.hover_row, Some(1));
    term.draw(|f| app.draw(f)).unwrap();
    let buf_text = |t: &Terminal<TestBackend>| -> String {
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    };
    let rendered = buf_text(&term);
    assert!(
        rendered.contains("beta"),
        "hover tooltip shows project name"
    );
    assert!(
        rendered.contains("[1] alpha"),
        "collapsed status bar shows selected project name"
    );

    // Search terminal scrollback with `F`, step matches, keep with Enter, clear with Esc.
    app.on_key(key(KeyCode::Char('F')), 27, 92);
    assert_eq!(app.mode, Mode::Search);
    assert!(!app.wants_ascii_ime());
    for c in "needle".chars() {
        app.on_key(key(KeyCode::Char(c)), 27, 92);
    }
    assert_eq!(app.search_matches.len(), 2);
    assert_eq!(app.search_idx, 1);
    app.on_key(key(KeyCode::Up), 27, 92);
    assert_eq!(app.search_idx, 0);
    assert!(app.sessions[0].as_ref().unwrap().scroll > 0);
    app.on_key(key(KeyCode::Enter), 27, 92);
    assert_eq!(app.mode, Mode::Manager);
    assert!(app.search_active);
    term.draw(|f| app.draw(f)).unwrap();
    assert!(buf_text(&term).contains("Find \"needle\": 1/2"));
    app.on_key(key(KeyCode::Esc), 27, 92);
    assert!(!app.search_active);

    drop(app);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn skills_modal_asks_before_replacing_a_projects_own_skill() {
    let dir = scratch_dir("skills-modal-ask");
    let proj_a = dir.join("agent-a");
    let own = proj_a.join(".claude/skills/review");
    fs::create_dir_all(&own).unwrap();
    fs::write(own.join("SKILL.md"), "---\nname: review\n---\nMine.\n").unwrap();
    let cfg = Config {
        projects: vec![proj("agent-a", proj_a.to_str().unwrap(), &["claude"])],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    app.skills_vault = Default::default();
    crate::skills::upsert_skill(
        &mut app.skills_vault,
        crate::skills::SkillEntry {
            name: "review".into(),
            description: "Vault review".into(),
            source_path: None,
            instructions: "Vault version.".into(),
        },
    )
    .unwrap();
    // Built by hand: `open_skill_modal` reloads the vault from a file path
    // other tests point elsewhere at the same time.
    let open = |app: &mut App| {
        let state = crate::skills::read_project_skills(&proj_a, &app.skills_vault);
        app.vault_modal = Some(VaultModal {
            target: VaultTarget::Project(0),
            sel: 0,
            top: Default::default(),
            // Opened from the project, its own `review` reads as picked.
            picked: state.vault_state.keys().cloned().collect(),
            confirm_delete: None,
            kind: VaultKindState::Skill {
                project_state: state,
                confirm_overwrite: None,
                editor: None,
            },
        });
    };
    let mine = || fs::read_to_string(own.join("SKILL.md")).unwrap();

    // Enter asks first; Esc goes back without touching anything.
    open(&mut app);
    app.on_key(key(KeyCode::Enter), 27, 70);
    let m = app.vault_modal.as_ref().expect("still open, asking");
    assert_eq!(m.skill_overwrite(), Some(&vec!["review".to_string()]));
    let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(screen.contains("differ from the vault"), "{screen}");
    app.on_key(key(KeyCode::Esc), 27, 70);
    assert!(app
        .vault_modal
        .as_ref()
        .unwrap()
        .skill_overwrite()
        .is_none());
    assert!(mine().contains("Mine."));

    // n keeps the project's own skill.
    app.on_key(key(KeyCode::Enter), 27, 70);
    app.on_key(key(KeyCode::Char('n')), 27, 70);
    assert!(app.vault_modal.is_none());
    assert!(mine().contains("Mine."));
    assert!(!proj_a.join(".agents/skills/review").exists());

    // y replaces it with the vault version.
    open(&mut app);
    app.on_key(key(KeyCode::Enter), 27, 70);
    app.on_key(key(KeyCode::Char('y')), 27, 70);
    assert!(app.vault_modal.is_none());
    assert!(mine().contains("Vault version."));
    assert!(proj_a.join(".agents/skills/review/SKILL.md").is_file());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn skills_vault_modal_registers_syncs_and_attaches_in_add_edit() {
    let dir = scratch_dir("skills-modal-e2e");
    let proj_a = dir.join("agent-a");
    let proj_b = dir.join("agent-b");
    fs::create_dir_all(proj_a.join(".agents/skills/local-skill")).unwrap();
    fs::write(
        proj_a.join(".agents/skills/local-skill/SKILL.md"),
        "---\nname: local-skill\ndescription: local\n---\n",
    )
    .unwrap();
    fs::create_dir_all(&proj_b).unwrap();
    let skills_file = dir.join("skills.toml");
    env::set_var("AGENTSQUAD_SKILLS", &skills_file);

    let cfg = Config {
        projects: vec![
            proj("agent-a", proj_a.to_str().unwrap(), &["claude"]),
            proj("agent-b", proj_b.to_str().unwrap(), &["codex"]),
        ],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();

    // `K` opens Skills modal; empty vault opens inline editor automatically.
    app.selected = 0;
    app.on_key(key(KeyCode::Char('K')), 27, 70);
    assert!(app.vault_modal.as_ref().is_some_and(VaultModal::is_skill));
    assert!(app.vault_modal.as_ref().unwrap().has_editor());
    assert!(!app.wants_ascii_ime());

    for ch in "code-review".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in "Review PR diffs carefully".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in "Check correctness, tests, and edge cases.".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(app.skills_vault.skills.len(), 1);
    assert_eq!(app.skills_vault.skills[0].name, "code-review");
    assert!(app.wants_ascii_ime());

    term.draw(|f| app.draw(f)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(screen.contains("code-review"), "{screen}");
    assert!(screen.contains("local-skill"), "{screen}");
    assert!(screen.contains("+ New skill"), "{screen}");

    // Enter syncs to both .agents/skills and .claude/skills on agent-a.
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(app.vault_modal.is_none());
    assert!(proj_a.join(".agents/skills/code-review/SKILL.md").is_file());
    assert!(proj_a.join(".claude/skills/code-review/SKILL.md").is_file());

    // Edit project (`e`) loads installed skills into add_skills, and Add form field 6 picks skills.
    app.open_edit();
    assert!(app.add_skills.contains("code-review"));
    app.add_field = 6;
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(app.vault_modal.as_ref().is_some_and(VaultModal::is_skill));
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(!app.add_skills.contains("code-review"));
    app.on_key(key(KeyCode::Esc), 27, 70);

    // Right-click context menu `'K'` opens Skills modal for the selected project.
    app.selected = 1;
    app.context_menu = Some(1);
    app.on_key(key(KeyCode::Char('K')), 27, 70);
    assert!(app.context_menu.is_none());
    assert!(app.vault_modal.as_ref().is_some_and(VaultModal::is_skill));
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(proj_b.join(".agents/skills/code-review/SKILL.md").is_file());
    assert!(proj_b.join(".claude/skills/code-review/SKILL.md").is_file());

    // Verify apply_add_skills installs picked skills onto a new project folder.
    app.on_key(key(KeyCode::Char('a')), 27, 70);
    app.add_field = 6;
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    let proj_c = dir.join("agent-c");
    fs::create_dir_all(&proj_c).unwrap();
    assert_eq!(app.apply_add_skills(&proj_c), Ok(Some((1, 0, Vec::new()))));
    assert!(proj_c.join(".agents/skills/code-review/SKILL.md").is_file());
    assert!(proj_c.join(".claude/skills/code-review/SKILL.md").is_file());

    drop(app);
    env::remove_var("AGENTSQUAD_SKILLS");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tools_vault_modal_registers_syncs_mcp_and_cli_and_attaches_in_add_edit() {
    let dir = scratch_dir("tools-modal-e2e");
    let proj_a = dir.join("agent-a");
    let proj_b = dir.join("agent-b");
    fs::create_dir_all(&proj_a).unwrap();
    fs::create_dir_all(&proj_b).unwrap();
    let tools_file = dir.join("tools.toml");
    env::set_var("AGENTSQUAD_TOOLS", &tools_file);

    let cfg = Config {
        projects: vec![
            proj("agent-a", proj_a.to_str().unwrap(), &["agy"]),
            proj("agent-b", proj_b.to_str().unwrap(), &["muse"]),
        ],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();

    // `T` opens Tools modal; empty vault opens inline ToolEditor on Kind (field 0).
    app.selected = 0;
    app.on_key(key(KeyCode::Char('T')), 27, 70);
    assert!(app.vault_modal.as_ref().is_some_and(VaultModal::is_tool));
    assert!(app.vault_modal.as_ref().unwrap().has_editor());
    assert!(
        app.wants_ascii_ime(),
        "field 0 Kind toggle accepts ASCII Space"
    );

    // Register an MCP server tool first.
    app.on_key(key(KeyCode::Tab), 27, 70);
    assert!(!app.wants_ascii_ime());
    for ch in "github".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in "npx -y @modelcontextprotocol/server-github".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in "GitHub API".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(app.tools_vault.tools.len(), 1);
    assert_eq!(app.tools_vault.tools[0].kind, crate::tools::ToolKind::Mcp);

    // Add a second tool as CLI (`a`, toggle Kind with Space, fill Name/Cmd/Desc).
    app.on_key(key(KeyCode::Char('a')), 27, 70);
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    assert_eq!(
        app.vault_modal.as_ref().unwrap().tool_kind(),
        Some(crate::tools::ToolKind::Cli)
    );
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in "ripgrep".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in "rg --hidden".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in "Fast workspace search".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(app.tools_vault.tools.len(), 2);

    term.draw(|f| app.draw(f)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(screen.contains("github"), "{screen}");
    assert!(screen.contains("ripgrep"), "{screen}");
    assert!(screen.contains("+ New tool"), "{screen}");

    // Enter syncs both tools to agent-a: .mcp.json + .agents/mcp_config.json + .codex/config.toml + .squad/TOOLS.md.
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(app.vault_modal.is_none());
    assert!(proj_a.join(".mcp.json").is_file());
    assert!(proj_a.join(".agents/mcp_config.json").is_file());
    assert!(proj_a.join(".codex/config.toml").is_file());
    let tools_md = fs::read_to_string(proj_a.join(".squad/TOOLS.md")).unwrap();
    assert!(tools_md.contains("### ripgrep"), "{tools_md}");

    // Right-click context menu `'T'` opens Tools modal on agent-b and apply_add_tools works from Add form.
    app.selected = 1;
    app.context_menu = Some(1);
    app.on_key(key(KeyCode::Char('T')), 27, 70);
    assert!(app.context_menu.is_none());
    assert!(app.vault_modal.as_ref().is_some_and(VaultModal::is_tool));
    app.on_key(key(KeyCode::Char('A')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(proj_b.join(".mcp.json").is_file());
    assert!(proj_b.join(".agents/mcp_config.json").is_file());
    assert!(proj_b.join(".codex/config.toml").is_file());
    assert!(proj_b.join(".squad/TOOLS.md").is_file());

    app.on_key(key(KeyCode::Char('a')), 27, 70);
    app.add_field = 7;
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Char('A')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    let proj_c = dir.join("agent-c");
    fs::create_dir_all(&proj_c).unwrap();
    assert_eq!(app.apply_add_tools(&proj_c), Ok(Some((2, 0))));
    assert!(proj_c.join(".mcp.json").is_file());
    assert!(proj_c.join(".squad/TOOLS.md").is_file());

    drop(app);
    env::remove_var("AGENTSQUAD_TOOLS");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn refs_vault_modal_registers_appends_and_removes_marker_section_cleanly() {
    let dir = scratch_dir("refs-modal-e2e");
    let proj_a = dir.join("agent-a");
    let proj_b = dir.join("agent-b");
    let proj_c = dir.join("agent-c");
    fs::create_dir_all(&proj_a).unwrap();
    fs::create_dir_all(&proj_b).unwrap();
    fs::create_dir_all(&proj_c).unwrap();
    let existing_claude = "# Agent A Notes\n\nExisting project rules stay intact.\n";
    fs::write(proj_a.join("CLAUDE.md"), existing_claude).unwrap();
    fs::write(proj_b.join("CLAUDE.md"), "# Agent B Notes\n").unwrap();
    fs::write(proj_b.join("GEMINI.md"), "# Gemini B\n").unwrap();

    let refs_file = dir.join("references.toml");
    env::set_var("AGENTSQUAD_REFS", &refs_file);

    let cfg = Config {
        projects: vec![
            proj("agent-a", proj_a.to_str().unwrap(), &["claude"]),
            proj("agent-b", proj_b.to_str().unwrap(), &["agy"]),
        ],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();

    // `R` opens References modal; empty vault opens inline RefEditor on URL (field 1).
    app.selected = 0;
    app.on_key(key(KeyCode::Char('R')), 27, 70);
    assert!(app.vault_modal.as_ref().is_some_and(VaultModal::is_ref));
    assert_eq!(app.vault_modal.as_ref().unwrap().ref_field(), Some(1));
    assert!(!app.wants_ascii_ime());
    term.draw(|f| app.draw(f)).unwrap();

    // Add first GitHub reference without typing Name: auto-derives `react` from URL.
    for ch in "https://github.com/facebook/react".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in "React UI library".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(app.refs_vault.references.len(), 1);
    assert_eq!(app.refs_vault.references[0].name, "react");
    assert_eq!(app.refs_vault.references[0].description, "React UI library");

    // Add second reference with explicit Name via BackTab -> field 0.
    app.on_key(key(KeyCode::Char('a')), 27, 70);
    app.on_key(key(KeyCode::BackTab), 27, 70);
    for ch in "ratatui-rs".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in "https://github.com/ratatui/ratatui".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in "TUI framework".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(app.refs_vault.references.len(), 2);
    term.draw(|f| app.draw(f)).unwrap();

    // Enter commits picked refs into CLAUDE.md (appended) and AGENTS.md.
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(app.vault_modal.is_none());
    let claude_a = fs::read_to_string(proj_a.join("CLAUDE.md")).unwrap();
    assert!(claude_a.starts_with("# Agent A Notes\n\nExisting project rules stay intact."));
    assert!(claude_a.contains("<!-- agentsquad:references:start -->"));
    assert!(claude_a.contains("**react** (https://github.com/facebook/react): React UI library"));
    assert!(claude_a.contains("**ratatui-rs** (https://github.com/ratatui/ratatui): TUI framework"));
    assert!(proj_a.join("AGENTS.md").is_file());

    // Editing an already-assigned reference inside `R` modal marks it Differs and updates on Enter.
    app.on_key(key(KeyCode::Char('R')), 27, 70);
    app.vault_modal.as_mut().unwrap().sel = 0;
    app.on_key(key(KeyCode::Char('e')), 27, 70);
    app.on_key(key(KeyCode::Tab), 27, 70);
    for ch in " v2".chars() {
        app.on_key(key(KeyCode::Char(ch)), 27, 70);
    }
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(
        app.vault_modal
            .as_ref()
            .unwrap()
            .ref_state()
            .unwrap()
            .vault_state
            .get("react"),
        Some(&crate::config::SyncState::Differs)
    );
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(fs::read_to_string(proj_a.join("CLAUDE.md"))
        .unwrap()
        .contains("React UI library v2"));

    // Unassign one via Space + Enter: keeps remaining entry and user content.
    app.on_key(key(KeyCode::Char('R')), 27, 70);
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    let partial_a = fs::read_to_string(proj_a.join("CLAUDE.md")).unwrap();
    assert!(!partial_a.contains("react"));
    assert!(partial_a.contains("ratatui-rs"));
    assert!(partial_a.contains("Existing project rules stay intact."));

    // Unassign the last reference: removes ONLY managed block from CLAUDE.md & deletes AGENTS.md.
    app.on_key(key(KeyCode::Char('R')), 27, 70);
    app.on_key(key(KeyCode::Down), 27, 70);
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(
        fs::read_to_string(proj_a.join("CLAUDE.md")).unwrap(),
        existing_claude
    );
    assert!(!proj_a.join("AGENTS.md").exists());

    // Context menu `'R'` opens References modal on agent-b (updating CLAUDE.md, AGENTS.md, GEMINI.md).
    app.selected = 1;
    app.context_menu = Some(1);
    app.on_key(key(KeyCode::Char('R')), 27, 70);
    assert!(app.context_menu.is_none());
    assert!(app.vault_modal.as_ref().is_some_and(VaultModal::is_ref));
    app.on_key(key(KeyCode::Char('A')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(fs::read_to_string(proj_b.join("CLAUDE.md"))
        .unwrap()
        .starts_with("# Agent B Notes"));
    assert!(fs::read_to_string(proj_b.join("GEMINI.md"))
        .unwrap()
        .contains("https://github.com/facebook/react"));

    // Add form field 8 + apply_add_refs attaches picked refs to a new project.
    app.on_key(key(KeyCode::Char('a')), 27, 70);
    app.add_field = 8;
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Char('A')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(app.apply_add_refs(&proj_c), Ok(Some((2, 0))));
    assert!(proj_c.join("CLAUDE.md").is_file());
    assert!(proj_c.join("AGENTS.md").is_file());

    drop(app);
    env::remove_var("AGENTSQUAD_REFS");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn unified_vault_modal_mouse_wheel_paste_and_outside_click() {
    let dir = scratch_dir("vault-mouse-paste");
    let proj_a = dir.join("agent-a");
    fs::create_dir_all(&proj_a).unwrap();
    let cfg = Config {
        projects: vec![proj("agent-a", proj_a.to_str().unwrap(), &["claude"])],
        ..Default::default()
    };
    let mut app = App::new(cfg, None);
    app.tools_vault.tools.push(crate::tools::ToolEntry {
        name: "gh".into(),
        kind: crate::tools::ToolKind::Cli,
        command: "gh status".into(),
        description: String::new(),
        env: Default::default(),
    });
    app.vault_modal = Some(VaultModal {
        target: VaultTarget::Project(0),
        sel: 0,
        top: Default::default(),
        picked: Default::default(),
        confirm_delete: None,
        kind: VaultKindState::Tool {
            project_state: Default::default(),
            editor: None,
        },
    });
    let area = Size::new(100, 30);

    // Wheel moves down to "+ New tool" row (index 1), then back up to 0.
    app.on_mouse(mouse(MouseEventKind::ScrollDown, 50, 15), area, 27, 70);
    assert_eq!(app.vault_modal.as_ref().unwrap().sel, 1);
    app.on_mouse(mouse(MouseEventKind::ScrollUp, 50, 15), area, 27, 70);
    assert_eq!(app.vault_modal.as_ref().unwrap().sel, 0);

    // Paste into Tool editor skips field 0 (Kind toggle) and inserts into field 1 (Name).
    app.on_key(key(KeyCode::Char('a')), 27, 70);
    assert!(app.vault_modal.as_ref().unwrap().has_editor());
    app.on_paste("ignored-on-kind".into());
    app.on_key(key(KeyCode::Tab), 27, 70);
    app.on_paste("pasted-tool\n".into());
    let modal = app.vault_modal.as_ref().unwrap();
    match &modal.kind {
        VaultKindState::Tool {
            editor: Some(ed), ..
        } => {
            assert_eq!(ed.name.as_str(), "pasted-tool");
        }
        _ => panic!("expected tool editor"),
    }

    // Left click outside the popup closes the vault modal.
    app.on_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
        area,
        27,
        70,
    );
    assert!(app.vault_modal.is_none());
    let _ = fs::remove_dir_all(&dir);
}
