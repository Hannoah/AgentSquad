//! Shell completion scripts: `squad completion <powershell|bash>` prints one.
//! Project names complete through `squad list --names` at completion time,
//! so the scripts stay correct as projects are added and removed.

/// The completion script for `shell` (`powershell`/`pwsh` or `bash`).
pub fn script_for(shell: &str) -> Option<&'static str> {
    match shell.to_ascii_lowercase().as_str() {
        "powershell" | "pwsh" => Some(powershell_script()),
        "bash" => Some(bash_script()),
        _ => None,
    }
}

/// PowerShell native completer. Load it from a profile with:
/// `squad completion powershell | Out-String | Invoke-Expression`
pub fn powershell_script() -> &'static str {
    r#"Register-ArgumentCompleter -Native -CommandName squad -ScriptBlock {
    param($wordToComplete, $commandAst, $cursorPosition)
    $cmds = @('add', 'completion', 'env', 'lessons', 'list', 'refs', 'remove', 'skills', 'start', 'tools', 'under', 'update')
    $args = @($commandAst.CommandElements | Select-Object -Skip 1 | ForEach-Object { $_.Extent.Text })
    if ($args.Count -eq 0 -or ($args.Count -eq 1 -and $wordToComplete -ne '')) {
        $cmds | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
            [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterName', $_)
        }
        return
    }
    if ($args[0] -in @('start', 'remove', 'under')) {
        try { $names = squad list --names 2>$null } catch { return }
        $names | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
            [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
        }
    }
}
"#
}

/// Bash completer. Load it with: `eval "$(squad completion bash)"`
pub fn bash_script() -> &'static str {
    r#"# squad completion for bash — load with: eval "$(squad completion bash)"
_squad_complete() {
    local cur prev cmds
    cmds="add completion env lessons list refs remove skills start tools under update"
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    case "$prev" in
        start|remove|under)
            COMPREPLY=($(compgen -W "$(squad list --names 2>/dev/null)" -- "$cur"))
            return 0
            ;;
        env|skills|tools|refs)
            COMPREPLY=($(compgen -W "list apply remove" -- "$cur"))
            return 0
            ;;
    esac
    if [ "$COMP_CWORD" -eq 1 ]; then
        COMPREPLY=($(compgen -W "$cmds" -- "$cur"))
    fi
}
complete -F _squad_complete squad
"#
}
