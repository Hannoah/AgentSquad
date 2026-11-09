use super::*;
use crate::history::{render, write_entry, Task};
use crate::ledger::list_entries;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn task() -> Task {
    Task {
        project: "my-app".into(),
        path: PathBuf::from("C:/work/my-app"),
        command: "claude".into(),
        started: UNIX_EPOCH + Duration::from_secs(1_000_000_000),
        took: Duration::from_secs(125),
        outcome: "task complete".into(),
        tail: "done: added login\n\n".into(),
    }
}

#[test]
fn entry_says_what_happened_without_the_agent() {
    let text = render(
        &task(),
        &["src/auth.rs".to_string()],
        &["abc1234 add login".to_string()],
    );
    assert!(text.starts_with("# my-app - task complete"));
    assert!(text.contains("- Ended: 2001-09-09T01:48:45Z"), "{text}");
    assert!(text.contains("- Took: 2m"));
    assert!(text.contains("- src/auth.rs") && text.contains("- abc1234 add login"));
    assert!(text.contains("done: added login\n```"), "tail is trimmed");
    let empty = render(&task(), &[], &[]);
    assert_eq!(empty.matches("(none seen)").count(), 2);
}

#[test]
fn entries_never_overwrite_and_list_newest_first() {
    let dir = scratch_dir("history");
    let t = task();
    let a = write_entry(&dir, &t, &[], &[]).unwrap();
    let b = write_entry(&dir, &t, &[], &[]).unwrap();
    assert_ne!(a, b, "same second, second file");
    assert!(a
        .file_name()
        .unwrap()
        .to_string_lossy()
        .ends_with("-auto.md"));
    let later = Task {
        started: SystemTime::now(),
        ..task()
    };
    write_entry(&dir, &later, &[], &[]).unwrap();
    let names: Vec<String> = list_entries(&dir).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names.len(), 3);
    assert!(names[0] > names[1], "newest first: {names:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_agent_project_gets_a_squad_folder_when_it_has_none() {
    use crate::ledger::ensure_agent;
    let mk = |dir: &Path, kind| Project {
        name: "p".into(),
        path: dir.to_path_buf(),
        command: vec!["sh".into()],
        kind,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
    };
    let agent = scratch_dir("ensure-agent");
    ensure_agent(&mk(&agent, Kind::Agent));
    assert!(agent.join(".squad").join("log").is_dir(), "agent gets one");
    // An existing folder is never touched.
    std::fs::write(agent.join(".squad").join("CONTEXT.md"), "mine").unwrap();
    ensure_agent(&mk(&agent, Kind::Agent));
    assert_eq!(
        std::fs::read_to_string(agent.join(".squad").join("CONTEXT.md")).unwrap(),
        "mine"
    );
    let service = scratch_dir("ensure-service");
    ensure_agent(&mk(&service, Kind::Service));
    assert!(!service.join(".squad").exists(), "nothing to record there");
    // A folder that is gone is not recreated just to hold a ledger.
    let gone = agent.join("deleted");
    ensure_agent(&mk(&gone, Kind::Agent));
    assert!(!gone.exists());
    let _ = std::fs::remove_dir_all(&agent);
    let _ = std::fs::remove_dir_all(&service);
}

#[test]
fn gather_reports_commits_and_changed_files_from_git() {
    use crate::history::gather;
    let dir = scratch_dir("history-git");
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
    let started = SystemTime::now() - Duration::from_secs(60);
    std::fs::write(dir.join("a.txt"), "a").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-q", "-m", "add a"]);
    std::fs::write(dir.join("b.txt"), "b").unwrap();
    let (files, commits) = gather(&dir, started);
    assert_eq!(files, ["b.txt"], "only what is still uncommitted");
    assert!(
        commits.len() == 1 && commits[0].ends_with("add a"),
        "{commits:?}"
    );
    // Not a repository: empty, never an error.
    let plain = scratch_dir("history-plain");
    assert_eq!(gather(&plain, started), (vec![], vec![]));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&plain);
}

#[test]
fn flush_waits_for_entries_still_being_written() {
    let dir = scratch_dir("history-flush");
    let t = Task {
        path: dir.clone(),
        ..task()
    };
    crate::history::record(t);
    crate::history::flush(Duration::from_secs(10));
    let log = dir.join(crate::ledger::DIR).join("log");
    assert_eq!(
        list_entries(&log).len(),
        1,
        "entry exists once flush returns"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
