use super::*;

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
        reports_to: None,
        pinned: false,
    };
    assert_eq!(pane_title_name(&agent), "my-agent");

    let service = Project {
        name: "backend".into(),
        path: PathBuf::from("."),
        command: vec!["npm".into(), "run".into(), "dev".into()],
        kind: Kind::Service,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
    };
    assert_eq!(pane_title_name(&service), "npm run dev");
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
        reports_to: None,
        pinned: false,
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
        format!(
            "Ctrl+Q manager | {} switch | other keys go to the session",
            if cfg!(target_os = "macos") {
                "Opt+1-9"
            } else {
                "Alt+1-9"
            }
        )
    );
    assert_eq!(mode_hints(Mode::Send), "Enter sends | Esc cancels");
    assert_eq!(
        mode_hints(Mode::Filter),
        "type to narrow | Enter keeps | Esc clears"
    );
    assert_eq!(
        mode_hints(Mode::Add),
        "Tab complete path | ↑↓ pick folder | Enter save | Esc cancel"
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
fn shorten_middle_keeps_both_ends() {
    assert_eq!(shorten_middle("C:\\work", 20), "C:\\work");
    let s = shorten_middle("C:\\Users\\kornah\\Work\\AgentSquad", 16);
    assert_eq!(s.chars().count(), 16);
    assert!(s.starts_with("C:\\") && s.ends_with("Squad") && s.contains('…'));
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
        reports_to: None,
        pinned: false,
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
        reports_to: None,
        pinned: false,
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
fn fit_marks_truncation() {
    assert_eq!(fit("hello", 8), "hello");
    assert_eq!(fit("hello", 5), "hello");
    assert_eq!(fit("hello world", 5), "hell…");
    assert_eq!(fit("hello", 0), "");
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
    assert_eq!(running.len(), 5);
    assert!(running.iter().any(|(_, l)| l.contains("Focus")));
    assert!(running.iter().any(|(_, l)| l.contains("Stop")));
    let stopped = context_menu_items(false);
    assert_eq!(stopped.len(), 4);
    assert!(stopped.iter().any(|(_, l)| l.contains("Start")));
    assert!(!stopped.iter().any(|(_, l)| l.contains("Stop")));
    // Number keys are 1-based in order with no gaps.
    assert_eq!(
        running.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        ['1', '2', '3', '4', '5']
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
fn window_title_names_the_selected_project() {
    assert_eq!(window_title(Some("my-app"), 0), "squad - my-app");
    assert_eq!(window_title(None, 0), "squad");
    assert_eq!(window_title(Some("my-app"), 2), "(2) squad - my-app");
}
#[test]
fn console_title_reads_without_panicking() {
    // A test run may have no console; either answer is fine, a crash is not.
    let _ = console_title();
}
#[test]
fn confirm_apply_lines_say_what_a_restart_costs() {
    let text = |n| {
        confirm_apply_lines("v9.9.9", n)
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(text(0).contains("v9.9.9") && text(0).contains("No sessions are running"));
    assert!(text(2).contains("2 sessions will be stopped"));
}

#[test]
fn confirm_click_hits_yes_and_no() {
    let lines = confirm_stop_lines("app");
    let area = ratatui::layout::Rect::new(0, 0, 100, 40);
    let popup = modal_rect(&lines, area);
    let hint = popup.y + lines.len() as u16;
    let x = popup.x + 1;
    // "y confirms | n / Esc cancels"
    assert_eq!(confirm_click(&lines, popup, x, hint), Some(true));
    assert_eq!(confirm_click(&lines, popup, x + 9, hint), Some(true));
    assert_eq!(confirm_click(&lines, popup, x + 13, hint), Some(false));
    assert_eq!(confirm_click(&lines, popup, x, hint - 1), None, "blank row");
    assert_eq!(confirm_click(&lines, popup, popup.x, hint), None, "border");
}

#[test]
fn procs_list_rect_leaves_room_for_detail_and_footer() {
    use ratatui::layout::Rect;
    // Tall: border, 5-row detail box and footer come off the list.
    assert_eq!(
        procs_list_rect(Rect::new(0, 0, 40, 20)),
        Rect::new(1, 1, 38, 12)
    );
    // Short: no detail box, only the footer.
    assert_eq!(
        procs_list_rect(Rect::new(0, 0, 40, 8)),
        Rect::new(1, 1, 38, 5)
    );
}
