use super::*;
use crate::history::{
    append_entry, daily_header, daily_name, extract_claude_turns, is_auto, render_section, Task,
};
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
        prompts: vec!["add login".into(), "hunter2".into()],
        output: "> add login\ndone: added login\n\n".into(),
        transcript: None,
    }
}

#[test]
fn entry_says_what_happened_without_the_agent() {
    let t = task();
    let header = daily_header(&t.project, t.started + t.took, 0);
    assert_eq!(header, "# my-app - 2001-09-09\n");
    let text = render_section(
        &t,
        &["src/auth.rs".to_string()],
        &["abc1234 add login".to_string()],
        None,
        0,
    );
    assert!(
        text.contains("## 2001-09-09 01:48:45 · my-app (`claude`) · task complete (2m)"),
        "{text}"
    );
    assert!(text.contains("- Date: 2001-09-09"), "{text}");
    assert!(text.contains("- Time: 01:48:45 (took 2m)"), "{text}");
    assert!(text.contains("- Agent: my-app (`claude`)"), "{text}");
    assert!(text.contains("- src/auth.rs") && text.contains("- abc1234 add login"));
    assert!(text.contains("### User\n\nadd login"), "{text}");
    assert!(
        !text.contains("hunter2"),
        "input the screen never echoed (a password) stays out:\n{text}"
    );
    assert!(text.contains("done: added login\n```"), "output is trimmed");

    let quiet = render_section(
        &t,
        &[],
        &[],
        Some("### User\n\nhi\n\n### Assistant\n\nhello"),
        0,
    );
    assert!(!quiet.contains("### Files changed"));
    assert!(!quiet.contains("### Commits"));
    assert!(quiet.contains("### User\n\nhi\n\n### Assistant\n\nhello"));
    assert!(!quiet.contains("### Output"));

    let fenced = render_section(
        &Task {
            output: "```rust\nlet a = 1;\n```".into(),
            ..task()
        },
        &[],
        &[],
        None,
        0,
    );
    assert!(
        fenced.contains("````\n```rust\nlet a = 1;\n```\n````"),
        "outer fence expands beyond embedded backticks:\n{fenced}"
    );
}

#[test]
fn same_day_entries_accumulate_into_one_file_and_list_newest_first() {
    let dir = scratch_dir("history");
    let t1 = task();
    let t2 = Task {
        took: Duration::from_secs(300),
        prompts: vec!["second prompt".into()],
        output: "> second prompt\nsecond output".into(),
        ..task()
    };
    let a = append_entry(&dir, &t1, &[], &[], None, 0).unwrap();
    let b = append_entry(&dir, &t2, &[], &[], None, 0).unwrap();
    assert_eq!(a, b, "same UTC day appends to the same daily file");
    assert_eq!(a.file_name().unwrap().to_string_lossy(), "20010909-auto.md");
    let body = std::fs::read_to_string(&a).unwrap();
    assert_eq!(
        body.matches("# my-app - 2001-09-09").count(),
        1,
        "daily header written once:\n{body}"
    );
    assert!(body.contains("done: added login"), "{body}");
    assert!(body.contains("second prompt"), "{body}");
    assert!(body.contains("second output"), "{body}");

    let later = Task {
        started: SystemTime::now(),
        ..task()
    };
    let c = append_entry(&dir, &later, &[], &[], None, 0).unwrap();
    assert_ne!(c, a);
    assert_eq!(
        c.file_name().unwrap().to_string_lossy(),
        daily_name(later.started + later.took, 0)
    );
    let names: Vec<String> = list_entries(&dir).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names.len(), 2);
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
        restart_on_crash: false,
    };
    let agent = scratch_dir("ensure-agent");
    ensure_agent(&mk(&agent, Kind::Agent));
    assert!(agent.join(".squad").join("log").is_dir(), "agent gets one");
    assert!(
        agent.join(".claude").join("settings.local.json").is_file(),
        "and squad's Claude hooks from registration on"
    );
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
    assert!(!service.join(".claude").exists(), "services get no hooks");
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
    std::fs::write(dir.join("x"), "x").unwrap();
    git(&["add", "a.txt", "x"]);
    git(&["commit", "-q", "-m", "add a"]);
    std::fs::write(dir.join("b.txt"), "b").unwrap();
    let (files, commits) = gather(&dir, started);
    assert_eq!(files, ["b.txt"], "only what is still uncommitted");
    assert!(
        commits.len() == 1 && commits[0].ends_with("add a"),
        "{commits:?}"
    );
    // A staged rename with a short old name must not swallow the next entry.
    git(&["mv", "x", "renamed.txt"]);
    let (files, _) = gather(&dir, started);
    assert!(
        files.iter().any(|f| f == "renamed.txt") && files.iter().any(|f| f == "b.txt"),
        "{files:?}"
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

#[test]
fn auto_log_names_distinguish_daily_legacy_and_sitreps() {
    assert!(is_auto("20261125-auto.md"));
    assert!(is_auto("20261108T061200Z-auto.md"));
    assert!(is_auto("20261108T061200Z-auto-2.md"));
    assert!(
        !is_auto("20261108T061200Z-auto-login.md"),
        "an agent's slug"
    );
    assert!(!is_auto("20261125-my-sitrep.md"));
    assert!(!is_auto("20261108T061200Z-add-auth.md"));
    assert!(!is_auto("auto.md"));
}

#[test]
fn claude_transcript_delta_tracks_offset_without_duplicates() {
    let dir = scratch_dir("claude-jsonl");
    let jsonl = dir.join("session.jsonl");
    let initial = concat!(
        r#"{"type":"user","timestamp":"2026-11-24T13:00:00.000Z","message":{"role":"user","content":"<system-reminder>secret</system-reminder>Fix login bug"}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-11-24T13:00:05.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"src/auth.rs"}},{"type":"text","text":"Checking auth.rs now."}]}}"#,
        "\n",
        r#"{"type":"user","timestamp":"2026-11-24T13:00:06.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"file body"}]}}"#,
        "\n",
        r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-11-24T13:00:07.000Z","message":{"role":"assistant","content":[{"type":"text","text":"sidechain ignored"}]}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-11-24T13:00:08.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Fixed login validation."}]}}"#,
        "\n",
    );
    std::fs::write(&jsonl, initial).unwrap();
    let (md1, off1) = extract_claude_turns(&jsonl, None, None).unwrap();
    assert!(md1.contains("### User\n\nFix login bug"), "{md1}");
    assert!(!md1.contains("system-reminder"), "{md1}");
    assert!(!md1.contains("hmm"), "{md1}");
    assert!(!md1.contains("sidechain ignored"), "{md1}");
    assert!(
        md1.contains("- [Tool: Read] `src/auth.rs`"),
        "tool work included:\n{md1}"
    );
    assert!(
        md1.contains("### Assistant\n\n- [Tool: Read] `src/auth.rs`\n\nChecking auth.rs now.\n\nFixed login validation."),
        "{md1}"
    );

    // Reading again with no new lines yields empty markdown and identical offset.
    let (md_empty, off_same) = extract_claude_turns(&jsonl, Some(off1), None).unwrap();
    assert!(md_empty.is_empty(), "{md_empty}");
    assert_eq!(off_same, off1);

    // Appending a second turn extracts ONLY the new turn without duplicating the first.
    let next = concat!(
        r#"{"type":"user","timestamp":"2026-11-24T13:05:00.000Z","message":{"role":"user","content":"Also add tests"}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-11-24T13:05:10.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Added 3 unit tests."}]}}"#,
        "\n",
    );
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&jsonl)
        .unwrap();
    f.write_all(next.as_bytes()).unwrap();
    drop(f);

    // A line still being written is left for the next read, cursor included.
    let partial = r#"{"type":"user","timestamp":"2026-11-24T13:06:00.000Z","message":{"role":"user","content":"Half"#;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&jsonl)
        .unwrap();
    f.write_all(partial.as_bytes()).unwrap();
    drop(f);
    let (md2, off2) = extract_claude_turns(&jsonl, Some(off1), None).unwrap();
    assert!(off2 > off1);
    assert!(!md2.contains("Half"), "{md2}");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&jsonl)
        .unwrap();
    f.write_all(b" written\"}}\n").unwrap();
    drop(f);
    let (md3, _) = extract_claude_turns(&jsonl, Some(off2), None).unwrap();
    assert!(md3.contains("### User\n\nHalf written"), "{md3}");
    assert!(
        !md2.contains("Fix login bug"),
        "no duplicate turn 1:\n{md2}"
    );
    assert!(md2.contains("### User\n\nAlso add tests"), "{md2}");
    assert!(
        md2.contains("### Assistant\n\nAdded 3 unit tests."),
        "{md2}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
