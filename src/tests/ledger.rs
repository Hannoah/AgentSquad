use super::*;

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
        command: if cfg!(windows) {
            vec!["cmd.exe".into(), "/d".into(), "/c".into(), "exit 3".into()]
        } else {
            vec!["sh".into(), "-c".into(), "exit 3".into()]
        },
        kind: Kind::Agent,
        autostart: false,
        ledger: true,
        reports_to: None,
        pinned: false,
        restart_on_crash: false,
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
fn finished_tasks_are_reported_to_the_commander_file() {
    let dir = scratch_dir("reports");
    // No ledger, no file.
    crate::ledger::report(&dir, &dir, "soldier", "platoon");
    assert!(!dir.join(".squad").exists());
    std::fs::create_dir_all(dir.join(".squad").join("log")).unwrap();
    std::fs::write(dir.join(".squad/log/20261108T010000Z-soldier-old.md"), "x").unwrap();
    std::fs::write(dir.join(".squad/log/20261108T020000Z-soldier-new.md"), "x").unwrap();
    crate::ledger::report(&dir, &dir, "soldier", "platoon");
    crate::ledger::report(&dir, &dir, "soldier-2", "platoon");
    let text = std::fs::read_to_string(dir.join(".squad/REPORTS.md")).unwrap();
    assert_eq!(text.matches("# Reports").count(), 1, "header written once");
    assert!(text.contains("**soldier -> platoon**"), "{text}");
    assert!(
        text.contains("log/20261108T020000Z-soldier-new.md"),
        "newest SITREP named"
    );
    assert!(!text.contains("soldier-old"));
}

#[test]
fn joining_a_commander_files_a_report_in_with_the_head_count() {
    let dir = scratch_dir("report-in");
    std::fs::create_dir_all(dir.join(".squad")).unwrap();
    let d = dir.to_str().unwrap();
    let mk = |name: &str, under: Option<&str>| Project {
        name: name.into(),
        path: PathBuf::from(d),
        command: vec!["claude".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: true,
        reports_to: under.map(Into::into),
        pinned: false,
        restart_on_crash: false,
    };
    let reports = || std::fs::read_to_string(dir.join(".squad/REPORTS.md")).unwrap_or_default();
    // Two agents join "lead": each report-in carries the new head count.
    let one = vec![mk("lead", None), mk("a", Some("lead"))];
    crate::ledger::report_transfer(&one, "a", None);
    let two = vec![
        mk("lead", None),
        mk("a", Some("lead")),
        mk("b", Some("lead")),
    ];
    crate::ledger::report_transfer(&two, "b", None);
    let text = reports();
    assert!(
        text.contains("**a -> lead**: reporting in. Now 1 directly under lead, 1 in all"),
        "{text}"
    );
    assert!(
        text.contains("**b -> lead**: reporting in. Now 2 directly under lead, 2 in all"),
        "{text}"
    );
    // b moves under a: lead hears it left, a hears it arrived.
    let moved = vec![mk("lead", None), mk("a", Some("lead")), mk("b", Some("a"))];
    crate::ledger::report_transfer(&moved, "b", Some("lead"));
    let text = reports();
    assert!(
        text.contains("**b** left lead. Now 1 directly under lead, 2 in all"),
        "{text}"
    );
    assert!(
        text.contains("**b -> a**: reporting in. Now 1 directly under a, 1 in all"),
        "{text}"
    );
    // No commander, no report; unknown names are ignored.
    let before = reports();
    crate::ledger::report_transfer(&one, "lead", None);
    crate::ledger::report_transfer(&one, "ghost", None);
    assert_eq!(reports(), before);
}

#[test]
fn report_in_opens_the_ledger_when_there_is_none() {
    let dir = scratch_dir("report-in-opens");
    let d = dir.to_str().unwrap();
    let mk = |name: &str, under: Option<&str>| Project {
        name: name.into(),
        path: PathBuf::from(d),
        command: vec!["claude".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: under.map(Into::into),
        pinned: false,
        restart_on_crash: false,
    };
    assert!(!dir.join(".squad").exists());
    crate::ledger::report_transfer(&[mk("lead", None), mk("a", Some("lead"))], "a", None);
    let text = std::fs::read_to_string(dir.join(".squad/REPORTS.md")).unwrap();
    assert!(text.contains("**a -> lead**: reporting in"), "{text}");
}

#[test]
fn stop_hook_sends_the_agent_back_until_it_files() {
    use crate::ledger;
    use std::{fs, thread::sleep, time::Duration};
    let dir = scratch_dir("hook");
    let ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git init");
    ledger::ensure(&dir).unwrap();
    ledger::event(&dir, "start t");
    assert!(
        ledger::stop_hook("{}", &dir).is_none(),
        "nothing changed yet"
    );

    sleep(Duration::from_millis(50));
    fs::write(dir.join("a.txt"), "work").unwrap();
    let out = ledger::stop_hook("{}", &dir).expect("changed files are blocked");
    assert!(out.contains(r#""decision":"block""#));
    // Auto history entries written by squad must NOT satisfy the Stop hook.
    sleep(Duration::from_millis(50));
    fs::write(
        dir.join(".squad")
            .join("log")
            .join("20261108T061200Z-auto.md"),
        "auto",
    )
    .unwrap();
    assert!(
        ledger::stop_hook("{}", &dir).is_some(),
        "auto history entry must not bypass Stop hook"
    );
    assert!(
        ledger::stop_hook(r#"{"stop_hook_active": true}"#, &dir).is_none(),
        "sent back once, never in a loop"
    );
    fs::create_dir(dir.join("sub")).unwrap();
    assert!(
        ledger::stop_hook("{}", &dir.join("sub")).is_some(),
        "found from a subfolder"
    );

    sleep(Duration::from_millis(50));
    fs::write(dir.join(".squad").join("log").join("x.md"), "sitrep").unwrap();
    assert!(
        ledger::stop_hook("{}", &dir).is_none(),
        "filed, so it may stop"
    );
    assert!(!ledger::unrecorded(&dir));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn stop_hook_ignores_folders_without_ledger_or_git() {
    use crate::ledger;
    let dir = scratch_dir("hook-none");
    assert!(ledger::stop_hook("{}", &dir).is_none(), "no ledger");
    ledger::ensure(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "x").unwrap();
    // Not a repository (scratch dir lives in temp): nothing to compare, so no block.
    assert!(ledger::stop_hook("{}", &dir).is_none(), "no git");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stop_hook_quoted_flag_does_not_disable_enforcement() {
    use crate::ledger;
    use std::{fs, thread::sleep, time::Duration};
    let dir = scratch_dir("hook-quote");
    let ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git init");
    ledger::ensure(&dir).unwrap();
    ledger::event(&dir, "start t");
    sleep(Duration::from_millis(50));
    fs::write(dir.join("a.txt"), "work").unwrap();
    // A transcript merely quoting the flag must not disable enforcement.
    let quoted = r#"{"transcript": "user wrote \"stop_hook_active\": true in chat"}"#;
    assert!(
        ledger::stop_hook(quoted, &dir).is_some(),
        "quoted flag must not bypass the block"
    );
    // ...while a real top-level boolean still lets the agent stop.
    assert!(ledger::stop_hook(r#"{"stop_hook_active": true}"#, &dir).is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn hook_settings_file_runs_squad_hook_stop() {
    use crate::ledger;
    let dir = scratch_dir("hook-file");
    ledger::ensure(&dir).unwrap();
    let file = ledger::write_hook_settings(&dir).unwrap();
    let text = std::fs::read_to_string(file).unwrap();
    let shell = if cfg!(windows) { "powershell" } else { "bash" };
    assert!(text.contains(r#""Stop""#) && text.contains(&format!(r#""shell":"{shell}""#)));
    assert!(text.contains("' hook stop"), "{text}");
    assert!(
        text.contains(r#""Notification""#) && text.contains("' hook notify"),
        "{text}"
    );
    assert!(
        text.contains(r#""PermissionRequest""#) && text.contains("' hook permission"),
        "{text}"
    );
    assert!(!text.contains(r"\U"), "forward slashes only: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_state_report_embeds_subsecond_unix_timestamp() {
    use crate::ledger::{format_state_report, STATE_ASKING, STATE_DONE};
    use std::time::{Duration, UNIX_EPOCH};
    let ts = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_700);
    assert_eq!(
        format_state_report(STATE_DONE, ts),
        "done 1700000000123456700\n"
    );
    assert_eq!(
        format_state_report(STATE_ASKING, ts),
        "needs-input 1700000000123456700\n"
    );
}

#[test]
fn sitreps_list_newest_first_with_readable_labels() {
    use crate::ledger::{sitrep_label, sitreps};
    assert_eq!(
        sitrep_label("20261108T061200Z-add-auth.md", 0),
        "11-08 06:12 add-auth"
    );
    assert_eq!(sitrep_label("20261125-auto.md", 9 * 3600), "11-25 auto");
    // Shown in local time: nine hours ahead, and over midnight into the next day.
    assert_eq!(
        sitrep_label("20261108T061200Z-add-auth.md", 9 * 3600),
        "11-08 15:12 add-auth"
    );
    assert_eq!(
        sitrep_label("20261231T200000Z-auto.md", 9 * 3600),
        "01-01 05:00 auto"
    );
    assert_eq!(sitrep_label("20261108T061200Z.md", 0), "11-08 06:12");
    assert_eq!(
        sitrep_label("notes.md", 9 * 3600),
        "notes",
        "unstamped names pass through"
    );
    let dir = scratch_dir("sitreps");
    assert!(sitreps(&dir).is_empty(), "no ledger, no entries");
    crate::ledger::ensure(&dir).unwrap();
    let log = dir.join(".squad").join("log");
    std::fs::write(log.join("20261108T061200Z-old.md"), "a").unwrap();
    std::fs::write(log.join("20261109T010000Z-new.md"), "b").unwrap();
    let names: Vec<String> = sitreps(&dir).into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        ["20261109T010000Z-new.md", "20261108T061200Z-old.md"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn history_filter_matches_entry_names_case_insensitively() {
    use crate::ledger::filter_sitrep_names;
    use std::path::PathBuf;
    let items = vec![
        ("20261108T061200Z-auto.md".to_string(), PathBuf::from("a")),
        (
            "20261109T010000Z-Add-Auth.md".to_string(),
            PathBuf::from("b"),
        ),
    ];
    assert_eq!(filter_sitrep_names(&items, ""), vec![0, 1]);
    assert_eq!(filter_sitrep_names(&items, "auto"), vec![0]);
    assert_eq!(filter_sitrep_names(&items, "ADD-auth"), vec![1]);
    assert!(filter_sitrep_names(&items, "zzz").is_empty());
}

#[test]
fn exclude_reaches_the_shared_exclude_file_from_a_worktree() {
    use crate::ledger;
    let base = scratch_dir("exclude-worktree");
    let (main, wt) = (base.join("main"), base.join("wt"));
    fs::create_dir_all(&main).unwrap();
    let git = |dir: &std::path::Path, args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&main, &["init", "-q"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "x"]);
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    assert!(wt.join(".git").is_file(), "a worktree's .git is a file");

    ledger::exclude_entry_from_git(&wt, ".env").unwrap();
    fs::write(wt.join(".env"), "K=v\n").unwrap();
    assert!(ledger::git(&wt, &["check-ignore", "-q", ".env"]).is_some());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn only_permission_and_question_notifications_mean_waiting() {
    use crate::ledger::notification_asks;
    assert!(notification_asks(
        r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"}"#
    ));
    assert!(notification_asks(
        r#"{"notification_type":"elicitation_dialog","message":"MCP server asks"}"#
    ));
    // The idle reminder a minute after a finished turn is not a question.
    assert!(!notification_asks(
        r#"{"notification_type":"idle_prompt","message":"Claude is waiting for your input"}"#
    ));
    // Older Claude Code: the message alone decides.
    assert!(notification_asks(
        r#"{"message":"Claude needs your permission to use Write"}"#
    ));
    assert!(!notification_asks(
        r#"{"message":"Claude is waiting for your input"}"#
    ));
    assert!(!notification_asks("not json"));
}
