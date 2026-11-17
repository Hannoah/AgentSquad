use super::*;
use crate::app::App;
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
    app.sessions[0] = Some(s2);
    app.stop_slot(0, "stopped by user");
    crate::history::flush(Duration::from_secs(3));
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
    drop(app);
    let _ = fs::remove_dir_all(&hist_dir);
}
