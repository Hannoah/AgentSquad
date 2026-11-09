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
        command: vec!["cmd.exe".into(), "/d".into(), "/c".into(), "exit 3".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: true,
        reports_to: None,
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
fn hook_settings_file_runs_squad_hook_stop() {
    use crate::ledger;
    let dir = scratch_dir("hook-file");
    ledger::ensure(&dir).unwrap();
    let file = ledger::write_hook_settings(&dir).unwrap();
    let text = std::fs::read_to_string(file).unwrap();
    assert!(text.contains(r#""Stop""#) && text.contains(r#""shell":"powershell""#));
    assert!(text.contains("' hook stop"), "{text}");
    assert!(!text.contains(r"\U"), "forward slashes only: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}
