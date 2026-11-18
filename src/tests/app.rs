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

    // 2. Service Add form skips Reports-to (field 4) on Tab/Down and reaches Env (field 5).
    empty.add_kind = Kind::Service;
    empty.add_field = 3; // Autostart
    empty.on_key(key(KeyCode::Tab), 24, 54);
    assert_eq!(
        empty.add_field, 5,
        "Tab from Autostart on Service skips Reports-to and goes to Env"
    );
    empty.on_key(key(KeyCode::Tab), 24, 54);
    assert_eq!(empty.add_field, 0, "Tab from Env wraps to Path");
    empty.on_key(key(KeyCode::BackTab), 24, 54);
    assert_eq!(empty.add_field, 5, "BackTab from Path wraps to Env");
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
    assert_eq!(app.context_sel, 7, "Up wraps to last stopped menu item");
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
    assert!(app.env_modal.is_some());
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
    assert!(app.env_modal.is_none());
    let dotenv_a = fs::read_to_string(proj_a.join(".env")).unwrap();
    assert!(
        dotenv_a.contains("SHARED_API_KEY=secret-12345"),
        "{dotenv_a}"
    );

    // Switch to agent-b, open `E`, verify shared entry is reusable without retyping, pick with Space, apply.
    app.selected = 1;
    app.on_key(key(KeyCode::Char('E')), 27, 70);
    assert!(app.env_modal.as_ref().unwrap().editor.is_none());
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    let dotenv_b = fs::read_to_string(proj_b.join(".env")).unwrap();
    assert!(
        dotenv_b.contains("SHARED_API_KEY=secret-12345"),
        "{dotenv_b}"
    );

    // Also test selecting from Add modal's Env (.env) field (field 5).
    app.on_key(key(KeyCode::Char('a')), 27, 70);
    assert_eq!(app.mode, Mode::Add);
    app.add_field = 5;
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    assert!(app.env_modal.is_some());
    app.on_key(key(KeyCode::Char(' ')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert!(app.env_modal.is_none());
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
    assert!(before > 0 && app.env_modal.as_ref().unwrap().editor.is_none());
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
        app.env_modal.as_ref().unwrap().editor.is_some(),
        "Enter on the row opens a blank form instead of writing .env"
    );
    for c in "EXTRA_KEY".chars() {
        app.on_key(key(KeyCode::Char(c)), 27, 70);
    }
    app.on_key(key(KeyCode::Tab), 27, 70);
    app.on_key(key(KeyCode::Char('1')), 27, 70);
    app.on_key(key(KeyCode::Enter), 27, 70);
    assert_eq!(app.env_vault.vars.len(), before + 1);

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
    // Down past the last project steps onto Management, then wraps to the top.
    app.on_key(key(KeyCode::Down), 24, 54);
    assert_eq!((app.selected, app.mgmt_sel), (1, None));
    app.on_key(key(KeyCode::Down), 24, 54);
    assert_eq!(app.mgmt_sel, Some(0));
    app.on_key(key(KeyCode::Char('ㅓ')), 24, 54); // Hangul j
    assert_eq!(app.mgmt_sel, Some(1));
    app.on_key(key(KeyCode::Down), 24, 54);
    assert_eq!((app.selected, app.mgmt_sel), (0, None));
    app.on_key(key(KeyCode::Up), 24, 54);
    assert_eq!(app.mgmt_sel, Some(1), "Up from the top reaches Env vault");
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
    app.on_key(key(KeyCode::Esc), 24, 54);
    assert!(app.env_info.is_none());
    let _ = fs::remove_dir_all(&dir);
}
