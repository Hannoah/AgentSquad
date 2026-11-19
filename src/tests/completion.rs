#[test]
fn completion_scripts_cover_subcommands_and_project_names() {
    for shell in ["powershell", "pwsh", "bash", "BASH"] {
        let script = crate::completion::script_for(shell)
            .unwrap_or_else(|| panic!("{shell} should have a script"));
        for cmd in [
            "add",
            "completion",
            "env",
            "list",
            "refs",
            "remove",
            "skills",
            "start",
            "tools",
            "under",
            "update",
        ] {
            assert!(script.contains(cmd), "{shell} completion misses `{cmd}`");
        }
        assert!(
            script.contains("squad list --names"),
            "{shell} completion should complete live project names"
        );
    }
    assert!(crate::completion::script_for("fish").is_none());
    assert!(crate::completion::script_for("").is_none());
}
