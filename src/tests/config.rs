use super::*;

#[test]
fn config_roundtrip() {
    let c = Config {
        projects: vec![Project {
            name: "demo".into(),
            path: PathBuf::from("demo"),
            command: shell(),
            kind: Kind::Agent,
            autostart: false,
            ledger: false,
            reports_to: None,
        }],
        scrollback: None,
        ..Default::default()
    };
    let decoded: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
    assert_eq!(decoded.projects[0].name, "demo");
}
#[test]
fn kind_defaults_to_agent() {
    let c: Config =
        toml::from_str("[[projects]]\nname = \"demo\"\npath = \"demo\"\ncommand = [\"sh\"]")
            .unwrap();
    assert_eq!(c.projects[0].kind, Kind::Agent);
}
#[test]
fn kind_parses_service() {
    let c: Config = toml::from_str("[[projects]]\nname = \"api\"\npath = \"api\"\ncommand = [\"node\", \"server.js\"]\nkind = \"service\"").unwrap();
    assert_eq!(c.projects[0].kind, Kind::Service);
}
#[test]
fn kind_from_str() {
    assert_eq!("Service".parse::<Kind>().unwrap(), Kind::Service);
    assert!("bot".parse::<Kind>().is_err());
}
#[test]
fn order_groups_agents_first() {
    let mk = |kind| Project {
        name: "x".into(),
        path: PathBuf::from("x"),
        command: vec!["sh".into()],
        kind,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    let ps = vec![mk(Kind::Service), mk(Kind::Agent), mk(Kind::Service)];
    assert_eq!(display_order(&ps), vec![1, 0, 2]);
}
#[test]
fn build_project_validates() {
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_string).collect() };
    let dir = env::temp_dir();
    let dir_str = dir.to_str().unwrap();
    let p = build_project(&[], dir_str, Kind::Service, parts("node server.js")).unwrap();
    assert_eq!(p.kind, Kind::Service);
    assert_eq!(p.name, "node");
    assert_eq!(p.command, vec!["node".to_string(), "server.js".to_string()]);
    // A second agent in the same directory gets a unique name:
    let agent = build_project(&[], dir_str, Kind::Agent, parts("")).unwrap();
    let existing = vec![agent.clone()];
    let second = build_project(&existing, dir_str, Kind::Agent, parts("")).unwrap();
    assert_eq!(second.name, format!("{}-2", agent.name));
    assert_eq!(second.path, agent.path);
    assert!(build_project(&[], "", Kind::Agent, parts("")).is_err());
    assert!(build_project(&[], "C:/definitely/not/here/xyz", Kind::Agent, parts("")).is_err());
    let shell_p = build_project(&[], dir_str, Kind::Agent, parts("   ")).unwrap();
    assert_eq!(shell_p.command, shell());
    // Quoted path (e.g. from Windows "Copy as path") is accepted cleanly:
    let quoted_dir = format!("\"{dir_str}\"");
    let qp = build_project(&[], &quoted_dir, Kind::Agent, parts("")).unwrap();
    assert_eq!(qp.path, shell_p.path);
}
#[test]
fn services_allow_multiple_in_same_directory() {
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_string).collect() };
    let dir = env::temp_dir();
    let dir_str = dir.to_str().unwrap();
    let s1 = build_project(&[], dir_str, Kind::Service, parts("npm run dev")).unwrap();
    assert_eq!(s1.name, "npm");
    assert_eq!(s1.path, plain_path(fs::canonicalize(&dir).unwrap()));
    let existing = [s1.clone()];
    let s2 = build_project(&existing, dir_str, Kind::Service, parts("npm run server")).unwrap();
    assert_eq!(s2.name, "npm-2");
    assert_eq!(s2.path, s1.path);
    let existing_both = [s1.clone(), s2];
    let s3 = build_project(
        &existing_both,
        dir_str,
        Kind::Service,
        parts("python app.py"),
    )
    .unwrap();
    assert_eq!(s3.name, "python");
    assert_eq!(s3.path, s1.path);
}
#[test]
fn service_without_path_uses_current_dir() {
    // Services may skip the path: they run where squad runs, and the
    // project takes its name from the program being served.
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_string).collect() };
    let p = build_project(&[], "", Kind::Service, parts("node server.js")).unwrap();
    assert_eq!(p.kind, Kind::Service);
    assert_eq!(p.name, "node");
    assert_eq!(p.command, vec!["node".to_string(), "server.js".to_string()]);
    assert_eq!(
        p.path,
        plain_path(fs::canonicalize(env::current_dir().unwrap()).unwrap())
    );
    assert!(!p.path.to_string_lossy().starts_with(r"\\?\"));
    // An .exe program names the project without its extension.
    let q = build_project(&[], "", Kind::Service, parts("ollama.exe serve")).unwrap();
    assert_eq!(q.name, "ollama");
    // Agents still require a path.
    assert!(build_project(&[], "", Kind::Agent, parts("claude")).is_err());
}
#[test]
fn filter_matches_name_and_command() {
    let mk = |name: &str, cmd: &[&str], kind| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        kind,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    let ps = vec![
        mk("api", &["node", "server.js"], Kind::Service),
        mk("web", &["npm", "run", "dev"], Kind::Agent),
        mk("agent-alpha", &["claude"], Kind::Agent),
    ];
    assert_eq!(apply_filter(&ps, ""), vec![1, 2, 0]);
    assert_eq!(apply_filter(&ps, "api"), vec![0]);
    assert_eq!(apply_filter(&ps, "node"), vec![0]);
    assert_eq!(apply_filter(&ps, "AGENT"), vec![2]);
    assert!(apply_filter(&ps, "zzz").is_empty());
}
#[test]
fn plain_path_strips_verbatim_prefix() {
    assert_eq!(
        plain_path(PathBuf::from(r"\\?\C:\work\app")),
        PathBuf::from(r"C:\work\app")
    );
    // UNC shares and ordinary paths are left alone.
    let unc = PathBuf::from(r"\\?\UNC\srv\share");
    assert_eq!(plain_path(unc.clone()), unc);
    assert_eq!(plain_path(PathBuf::from("rel")), PathBuf::from("rel"));
}
#[test]
fn load_from_normalizes_stored_verbatim_paths() {
    let dir = env::temp_dir().join(format!("squad-test-norm-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("config.toml");
    let toml_content = r#"
[[projects]]
name = "legacy"
path = "\\\\?\\C:\\work\\app"
command = ["cmd.exe"]
kind = "agent"

[[projects]]
name = "cmd"
path = "C:\\Users\\kornah"
command = ["cmd.exe"]
kind = "service"
"#;
    fs::write(&path, toml_content).unwrap();
    let loaded = load_from(&path).unwrap();
    assert_eq!(loaded.projects[0].path, PathBuf::from(r"C:\work\app"));
    if cfg!(windows) {
        assert_eq!(loaded.projects[0].command, shell());
        assert_eq!(loaded.projects[1].command, shell());
        assert_eq!(loaded.projects[1].name, "kornah");
    }
    let _ = fs::remove_dir_all(&dir);
}
#[test]
fn split_command_honors_quotes() {
    assert_eq!(split_command("node  server.js"), ["node", "server.js"]);
    assert_eq!(
        split_command(r#""C:\Program Files\x.exe" --flag "a b""#),
        [r"C:\Program Files\x.exe", "--flag", "a b"]
    );
    assert_eq!(split_command(r#"echo """#), ["echo", ""]);
    assert!(split_command("   ").is_empty());
}
#[test]
fn persist_helpers_keep_other_writers_projects() {
    let file = env::temp_dir().join(format!("agentsquad-persist-{}.toml", std::process::id()));
    let mk = |name: &str| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: vec!["sh".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    // Single-threaded section: AGENTSQUAD_CONFIG is process-global, and
    // no other test reads it.
    env::set_var("AGENTSQUAD_CONFIG", &file);
    save_to(
        &Config {
            projects: vec![mk("a"), mk("b")],
            scrollback: None,
            ..Default::default()
        },
        &file,
    )
    .unwrap();
    // Another process registers "c" after we loaded; our remove of "a"
    // and add of "d" must keep it.
    let mut other = load_from(&file).unwrap();
    other.projects.push(mk("c"));
    save_to(&other, &file).unwrap();
    persist_remove("A").unwrap();
    persist_add(&mk("d")).unwrap();
    persist_add(&mk("D")).unwrap();
    let names: Vec<String> = load_from(&file)
        .unwrap()
        .projects
        .into_iter()
        .map(|p| p.name)
        .collect();
    env::remove_var("AGENTSQUAD_CONFIG");
    std::fs::remove_file(&file).ok();
    assert_eq!(names, ["b", "c", "d"]);
}
#[test]
fn remove_unregisters_project() {
    // `Delete` removes the selected project with confirmation, whether
    // or not it holds a session.
    let mk = |name: &str| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: vec!["sh".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    let mut projects = vec![mk("a"), mk("b")];
    let mut slots: Vec<Option<u8>> = vec![None, Some(1)];
    assert_eq!(
        remove_project(&mut projects, &mut slots, 1).map(|p| p.name),
        Some("b".to_string())
    );
    assert_eq!(projects.len(), 1);
    assert_eq!(slots.len(), 1);
    assert!(remove_project(&mut projects, &mut slots, 5).is_none());
    assert_eq!(projects.len(), 1);
    let text: String = confirm_remove_lines("b")
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(text.contains('b'), "remove modal names the project");
    assert!(text.contains('y') && text.contains("removes"));
}
#[test]
fn remove_persists_to_config_file() {
    // A TUI removal must survive a restart: write with the real saver,
    // read back with the real loader, and confirm the project is gone.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let file = std::env::temp_dir().join(format!(
        "agentsquad-remove-test-{}-{}.toml",
        std::process::id(),
        n
    ));
    let mk = |name: &str| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: vec!["sh".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    let config = Config {
        projects: vec![mk("a"), mk("b")],
        scrollback: None,
        ..Default::default()
    };
    save_to(&config, &file).unwrap();
    let mut loaded = load_from(&file).unwrap();
    assert_eq!(loaded.projects.len(), 2);
    let mut slots: Vec<Option<u8>> = vec![None, None];
    let removed = remove_project(&mut loaded.projects, &mut slots, 0).unwrap();
    assert_eq!(removed.name, "a");
    save_to(&loaded, &file).unwrap();
    let reread = load_from(&file).unwrap();
    assert_eq!(reread.projects.len(), 1);
    assert_eq!(reread.projects[0].name, "b");
    std::fs::remove_file(&file).ok();
}
#[test]
fn merge_appends_missing_projects() {
    let mk = |name: &str| Project {
        name: name.into(),
        path: PathBuf::from("/tmp"),
        command: vec!["sh".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    let mut c = Config {
        projects: vec![mk("a")],
        scrollback: None,
        ..Default::default()
    };
    let mut sessions: Vec<Option<Session>> = vec![None];
    merge_projects(
        &mut c,
        &mut sessions,
        Config {
            projects: vec![mk("a"), mk("B")],
            scrollback: None,
            ..Default::default()
        },
    );
    assert_eq!(c.projects.len(), 2);
    assert_eq!(sessions.len(), 2);
    assert_eq!(c.projects[1].name, "B");
    // Idempotent: a second merge adds nothing.
    merge_projects(
        &mut c,
        &mut sessions,
        Config {
            projects: vec![mk("A"), mk("b")],
            scrollback: None,
            ..Default::default()
        },
    );
    assert_eq!(c.projects.len(), 2);
    assert_eq!(sessions.len(), 2);
}

#[test]
fn shell_defaults_to_powershell_on_windows() {
    let sh = shell();
    assert!(!sh.is_empty());
    if cfg!(windows) {
        let lower = sh[0].to_ascii_lowercase();
        assert!(
            lower == "pwsh.exe" || lower == "powershell.exe",
            "expected PowerShell default on Windows, got {sh:?}"
        );
        assert!(is_bare_shell(&sh));
    }
    assert!(is_shell_program("cmd.exe"));
    assert!(is_shell_program("powershell.exe"));
    assert!(is_shell_program("pwsh"));
    assert!(!is_shell_program("jarvis.exe"));
    assert!(!is_shell_program("claude"));
}

#[test]
fn service_naming_looks_through_shells_and_live_foreground() {
    let parts = |s: &str| -> Vec<String> { split_command(s) };
    // `cmd /c jarvis --serve` extracts `jarvis` instead of `cmd`.
    assert_eq!(
        program_name(&parts("cmd.exe /c jarvis --serve")).as_deref(),
        Some("jarvis")
    );
    assert_eq!(
        program_name(&parts("powershell.exe -NoLogo -Command \"jarvis start\"")).as_deref(),
        Some("jarvis")
    );
    // A bare shell has no program name, so a service falls back to directory name
    // until a foreground process is launched inside it.
    assert_eq!(program_name(&parts("cmd.exe")), None);
    assert_eq!(program_name(&parts("powershell.exe -NoLogo")), None);

    let p = Project {
        name: "my-dir".into(),
        path: PathBuf::from("C:/work/my-dir"),
        command: vec!["powershell.exe".into(), "-NoLogo".into()],
        kind: Kind::Service,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    assert_eq!(live_name(&p, None), "my-dir");
    assert_eq!(live_name(&p, Some("jarvis")), "jarvis");
    assert_eq!(live_pane_title(&p, Some("jarvis")), "jarvis");

    // Foreground process tree lookup skips shells/conhost and picks the real app.
    let mut table = ProcTable::default();
    table.children.insert(100, vec![101, 102]);
    table.children.insert(102, vec![103]);
    table.names.insert(100, "powershell.exe".into());
    table.names.insert(101, "conhost.exe".into());
    table.names.insert(102, "cmd.exe".into());
    table.names.insert(103, "jarvis.exe".into());
    assert_eq!(foreground_name(100, &table).as_deref(), Some("jarvis"));
}

#[test]
fn legacy_migrations_run_once() {
    let dir = env::temp_dir().join(format!("squad-test-once-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("config.toml");
    let body = r#"
[[projects]]
name = "keep"
path = "work/keep"
command = ["cmd.exe"]
kind = "agent"
"#;
    // Unversioned file: migrated and stamped, so the stamp survives a save.
    fs::write(&path, body).unwrap();
    let old = load_from(&path).unwrap();
    assert_eq!(old.version, CONFIG_VERSION);
    if cfg!(windows) {
        assert_eq!(old.projects[0].command, shell());
    }
    // Stamped file: a deliberate `cmd.exe` is left alone.
    fs::write(&path, format!("version = {CONFIG_VERSION}\n{body}")).unwrap();
    let kept = load_from(&path).unwrap();
    assert_eq!(kept.projects[0].command, ["cmd.exe"]);
    save_to(&kept, &path).unwrap();
    assert_eq!(load_from(&path).unwrap().projects[0].command, ["cmd.exe"]);
    let _ = fs::remove_dir_all(&dir);
}
#[test]
fn interpreters_are_named_by_their_script() {
    let node = r#""C:\Program Files\nodejs\node.exe" C:\Users\me\AppData\Roaming\npm/node_modules/jarvis-cli/dist/cli.js start"#;
    assert_eq!(script_label("node", node).as_deref(), Some("jarvis-cli"));
    let scoped = r"node C:\x\node_modules\@acme\tool\bin.js";
    assert_eq!(script_label("node", scoped).as_deref(), Some("tool"));
    assert_eq!(
        script_label("node", "node --inspect server.js").as_deref(),
        Some("server")
    );
    assert_eq!(
        script_label("python", "python -u app.py").as_deref(),
        Some("app")
    );
    // Real programs keep their own name; a bare interpreter has no script.
    assert_eq!(script_label("ollama", "ollama serve"), None);
    assert_eq!(script_label("node", "node"), None);
}
#[test]
#[cfg(all(windows, target_pointer_width = "64"))]
fn command_line_reads_a_live_process() {
    let me = command_line(std::process::id()).expect("own command line");
    assert!(me.to_ascii_lowercase().contains("squad"), "got {me}");
}
#[test]
fn empty_command_errors_instead_of_panicking() {
    let p = Project {
        name: "x".into(),
        path: env::current_dir().unwrap(),
        command: vec![],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    let err = Session::spawn(&p, 24, 80)
        .err()
        .expect("must fail")
        .to_string();
    assert!(err.contains("empty command"), "{err}");
}
#[test]
fn save_to_leaves_no_temp_file() {
    let dir = env::temp_dir().join(format!("squad-save-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("config.toml");
    save_to(&Config::default(), &file).unwrap();
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(names, ["config.toml"]);
}

#[test]
fn autostart_and_bell_config_roundtrip() {
    let text = r#"
bell = false
[[projects]]
name = "a"
path = "."
command = ["x"]
autostart = true
[[projects]]
name = "b"
path = "."
command = ["x"]
"#;
    let c: Config = toml::from_str(text).unwrap();
    assert_eq!(c.bell, Some(false));
    assert!(c.projects[0].autostart && !c.projects[1].autostart);
    let out = toml::to_string(&c).unwrap();
    assert_eq!(out.matches("autostart").count(), 1, "false is not written");
    let again: Config = toml::from_str(&out).unwrap();
    assert!(again.projects[0].autostart && again.bell == Some(false));
    assert!(Config::default().bell.is_none());
}

#[test]
fn add_preview_name_matches_what_gets_registered() {
    let mk = |name: &str| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: vec!["x".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    let existing = vec![mk("node")];
    let none: Vec<String> = Vec::new();
    assert_eq!(
        preview_name(&existing, r#" "C:\work\my-app\" "#, Kind::Agent, &none),
        Some("my-app".into())
    );
    assert_eq!(preview_name(&existing, "", Kind::Agent, &none), None);
    assert_eq!(preview_name(&existing, ".", Kind::Agent, &none), None);
    let node = vec!["node".to_string(), "server.js".to_string()];
    assert_eq!(
        preview_name(&existing, "", Kind::Service, &node),
        Some("node-2".into()),
        "services get a unique program name"
    );
}

#[test]
fn filter_matches_path_too() {
    let mk = |name: &str, path: &str| Project {
        name: name.into(),
        path: PathBuf::from(path),
        command: vec!["claude".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    let projects = vec![mk("web", "/work/my-api"), mk("other", "/work/site")];
    assert_eq!(apply_filter(&projects, "my-api"), vec![0]);
    assert_eq!(apply_filter(&projects, "web"), vec![0]);
}

#[test]
fn startup_selection_prefers_explicit_then_last_then_first() {
    let mk = |name: &str, kind| Project {
        name: name.into(),
        path: PathBuf::from("x"),
        command: vec!["x".into()],
        kind,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    // Config order: svc, a1, a2. Sidebar order: agents first, so a1, a2, svc.
    let projects = vec![
        mk("svc", Kind::Service),
        mk("a1", Kind::Agent),
        mk("a2", Kind::Agent),
    ];
    let order = display_order(&projects);
    assert_eq!(start_selection(&projects, &order, None, None), 0);
    assert_eq!(start_selection(&projects, &order, None, Some("A2")), 1);
    assert_eq!(start_selection(&projects, &order, None, Some("svc")), 2);
    assert_eq!(
        start_selection(&projects, &order, Some(1), Some("svc")),
        0,
        "explicit wins"
    );
    assert_eq!(
        start_selection(&projects, &order, None, Some("gone")),
        0,
        "removed project"
    );
}

#[test]
fn chain_of_command_orders_ranks_and_briefs() {
    // One folder, so all of them share the same default commander.
    let mk = |name: &str, under: Option<&str>| Project {
        name: name.into(),
        path: PathBuf::from("/w"),
        command: vec!["claude".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: under.map(Into::into),
    };
    // Registered out of rank order: company, soldier, platoon.
    let p = vec![
        mk("company", None),
        mk("soldier", Some("platoon")),
        mk("platoon", Some("company")),
    ];
    assert_eq!(display_order(&p), vec![0, 2, 1], "commander before reports");
    assert_eq!(rank_prefix(&p, 0), "★ ");
    assert_eq!(rank_prefix(&p, 2), "└─★ ");
    assert_eq!(rank_prefix(&p, 1), "  └── ");
    assert_eq!(superior(&p, 1), Some(2));
    assert_eq!(subordinates(&p, 0), vec![2]);
    assert!(check_superior(&p, Kind::Agent, "COMPANY").is_ok());
    assert!(check_superior(&p, Kind::Service, "company").is_err());
    assert!(check_superior(&p, Kind::Agent, "nobody").is_err());
    // A hand-edited loop neither hangs nor drops agents.
    let looped = vec![mk("a", Some("b")), mk("b", Some("a"))];
    assert_eq!(display_order(&looped).len(), 2);
    let _ = rank_prefix(&looped, 0);
    let line = crate::ledger::brief_line("platoon", Some("company"), &["soldier"]);
    assert!(line.contains("commander is \"company\"") && line.contains("soldier"));
    assert!(!line.contains('\n'));
}

#[test]
fn orders_reach_every_depth_below_a_commander() {
    let mk = |name: &str, under: Option<&str>| Project {
        name: name.into(),
        path: PathBuf::from("/tmp"),
        command: vec!["claude".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: under.map(Into::into),
    };
    let p = vec![
        mk("company", None),
        mk("platoon", Some("company")),
        mk("soldier", Some("platoon")),
        mk("other", None),
    ];
    assert_eq!(all_subordinates(&p, 0), vec![1, 2]);
    assert_eq!(all_subordinates(&p, 1), vec![2]);
    assert!(all_subordinates(&p, 3).is_empty());
    let looped = vec![mk("a", Some("b")), mk("b", Some("a"))];
    assert_eq!(all_subordinates(&looped, 0), vec![1]);
}

fn agent_in(name: &str, dir: &str, under: Option<&str>) -> Project {
    Project {
        name: name.into(),
        path: PathBuf::from(dir),
        command: vec!["claude".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: under.map(Into::into),
    }
}

#[test]
fn an_agent_becomes_a_commander_when_another_attaches() {
    let mut p = vec![
        agent_in("lead", "/work/app", None),
        agent_in("helper", "/work/app", None),
        agent_in("solo", "/work/other", None),
    ];
    assert!(!is_commander(&p, 0), "alone, nobody is under it");
    assert!(!is_commander(&p, 2));
    p[1].reports_to = Some("lead".into());
    assert!(is_commander(&p, 0), "a commander once an agent attaches");
    assert!(!is_commander(&p, 1));
    assert!(!is_commander(&p, 2), "other folders are unaffected");
    // Later agents default to the folder's first agent; it defaults to nobody.
    let new = agent_in("third", "/work/app", None);
    assert_eq!(default_superior(&p, &new).as_deref(), Some("lead"));
    assert_eq!(default_superior(&[], &new), None);
    let mut svc = new.clone();
    svc.kind = Kind::Service;
    assert_eq!(default_superior(&p, &svc), None);
}

#[test]
fn second_agent_in_a_folder_reports_to_the_first() {
    let dir = scratch_dir("founder");
    let d = dir.to_str().unwrap();
    let first = build_project(&[], d, Kind::Agent, vec![]).unwrap();
    assert_eq!(first.reports_to, None);
    let second = build_project(std::slice::from_ref(&first), d, Kind::Agent, vec![]).unwrap();
    assert_eq!(second.reports_to.as_deref(), Some(first.name.as_str()));
}

#[test]
fn reassignment_refuses_loops_and_non_agents() {
    let p = vec![
        agent_in("a", "/a", None),
        agent_in("b", "/b", Some("a")),
        agent_in("c", "/c", Some("b")),
    ];
    assert_eq!(check_reassign(&p, "c", "A").as_deref(), Ok("a"));
    assert!(
        check_reassign(&p, "a", "c").is_err(),
        "a under its own subordinate"
    );
    assert!(check_reassign(&p, "b", "b").is_err(), "under itself");
    assert!(check_reassign(&p, "c", "nobody").is_err());
    assert!(check_reassign(&p, "ghost", "a").is_err());
}

#[test]
fn rank_prefix_draws_branches_and_pipes() {
    let p = vec![
        agent_in("boss", "/w", None),
        agent_in("one", "/w", Some("boss")),
        agent_in("kid", "/w", Some("one")),
        agent_in("two", "/w", Some("boss")),
    ];
    assert_eq!(rank_prefix(&p, 0), "★ ");
    assert_eq!(rank_prefix(&p, 1), "├─★ ");
    assert_eq!(
        rank_prefix(&p, 2),
        "│ └── ",
        "pipe while a later sibling exists"
    );
    assert_eq!(rank_prefix(&p, 3), "└── ");
}

#[test]
fn roster_lists_the_chain_of_command() {
    let dir = scratch_dir("roster");
    std::fs::create_dir_all(dir.join(".squad")).unwrap();
    let d = dir.to_str().unwrap();
    let p = vec![
        agent_in("lead", d, None),
        agent_in("helper", d, Some("lead")),
    ];
    crate::ledger::write_rosters(&p);
    let text = std::fs::read_to_string(dir.join(".squad/ROSTER.md")).unwrap();
    assert!(
        text.contains("- **lead** (commander), commands helper"),
        "{text}"
    );
    assert!(text.contains("- **helper**, reports to lead"), "{text}");
    // A folder without a ledger gets no file.
    let bare = scratch_dir("roster-bare");
    crate::ledger::write_rosters(&[agent_in("x", bare.to_str().unwrap(), None)]);
    assert!(!bare.join(".squad").exists());
}

#[test]
fn joining_a_commander_turns_the_ledger_on_for_both() {
    let mut p = vec![
        agent_in("lead", "/w", None),
        agent_in("a", "/w", Some("lead")),
        agent_in("loner", "/x", None),
    ];
    enable_chain_ledger(&mut p, 1);
    assert!(p[0].ledger && p[1].ledger, "agent and its commander");
    assert!(!p[2].ledger, "others keep their setting");
    // A commander-less agent changes nothing, and nothing is ever turned off.
    enable_chain_ledger(&mut p, 2);
    assert!(!p[2].ledger);
    p[1].reports_to = None;
    enable_chain_ledger(&mut p, 1);
    assert!(p[0].ledger && p[1].ledger);
}
