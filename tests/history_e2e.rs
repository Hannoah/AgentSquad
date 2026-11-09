//! End to end: run the real `squad` in a pseudo-terminal with a fake agent and
//! check that finishing a task files a history entry in `.squad/log/` with no
//! help from the agent. Slow (~20 s) and timing based, so run it on purpose:
//! `cargo test --test history_e2e -- --ignored --nocapture`

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn auto_entries(project: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(project.join(".squad").join("log"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with("-auto.md"))
        .collect()
}

#[test]
#[ignore = "end to end, ~20 s, timing based"]
fn a_finished_task_is_recorded_without_the_agent() {
    let root = std::env::temp_dir().join(format!("squad-e2e-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let project = root.join("demo");
    fs::create_dir_all(&project).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&project)
        .status()
        .unwrap()
        .success());

    // The "agent": waits for a line, works for 7 s while writing a file, repeats.
    let script = root.join("agent.ps1");
    fs::write(
        &script,
        "while ($true) {\n  $null = [Console]::ReadLine()\n  1..7 | ForEach-Object { \"working step $_\"; Start-Sleep -Seconds 1 }\n  Set-Content -Path (Join-Path $PWD 'made.txt') -Value 'x'\n  'finished'\n}\n",
    )
    .unwrap();
    let config = root.join("config.toml");
    fs::write(
        &config,
        format!(
            "version = 1\n\n[[projects]]\nname = \"demo\"\npath = '{}'\nkind = \"agent\"\ncommand = ['powershell.exe', '-NoLogo', '-NoProfile', '-File', '{}']\n",
            project.display(),
            script.display()
        ),
    )
    .unwrap();

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_squad"));
    cmd.args(["start", "demo"]);
    cmd.env("AGENTSQUAD_CONFIG", &config);
    cmd.env("AGENTSQUAD_NO_UPDATE_CHECK", "1");
    let mut child = pty.slave.spawn_command(cmd).unwrap();
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().unwrap();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while reader.read(&mut buf).is_ok_and(|n| n > 0) {} // drain so the pty never blocks
    });
    let mut writer = pty.master.take_writer().unwrap();

    // squad creates the project's .squad/ by itself, before any session runs.
    thread::sleep(Duration::from_secs(4));
    assert!(
        project.join(".squad").join("log").is_dir(),
        "squad adds .squad/ to an agent project that has none"
    );

    writer.write_all(b"go\r").unwrap();
    writer.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut found = Vec::new();
    while Instant::now() < deadline {
        found = auto_entries(&project);
        if !found.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    let _ = child.kill();
    let text = found
        .first()
        .map(|p| fs::read_to_string(p).unwrap())
        .unwrap_or_default();
    println!("--- entry ---\n{text}");
    let _ = fs::remove_dir_all(&root);
    assert!(!found.is_empty(), "no history entry appeared");
    assert!(text.contains("# demo - "), "{text}");
    assert!(
        text.contains("made.txt"),
        "the file the agent wrote: {text}"
    );
    assert!(
        text.contains("working step"),
        "the screen at the end: {text}"
    );
}
