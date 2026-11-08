# AgentSquad

[![ci](https://github.com/Hannoah/AgentSquad/actions/workflows/ci.yml/badge.svg)](https://github.com/Hannoah/AgentSquad/actions/workflows/ci.yml)

AgentSquad (`squad`) is a lightweight, project-based terminal session manager for Windows
(Windows only: CI, releases and tests target Windows).
Register your projects once, then open a single TUI manager to start, watch, and
interact with a terminal session per project — an ideal setup for running one
coding agent (or dev server) per project side by side.

Each project is either an **Agent** (an interactive terminal, e.g. `claude`) or a
**Service** (a long-running process, e.g. `npm run dev`). The sidebar groups them
accordingly, so running servers stay visible next to your agents.

## Install (Windows, no Rust required)

One line, straight from GitHub Releases. No repository checkout needed:

```powershell
irm https://raw.githubusercontent.com/Hannoah/AgentSquad/main/tools/install.ps1 | iex
```

What this does:

- Downloads `squad-windows-x86_64.zip` from the latest GitHub Release.
- Extracts `squad.exe` to `%LOCALAPPDATA%\AgentSquad\bin`.
- Adds that folder to your **user** `PATH`.

The installer also refreshes `PATH` for the current session, so verify right away:

```powershell
squad --version
```

To pin a version (default is latest):

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\install.ps1 -Version v0.7.2
```

To install elsewhere:

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\install.ps1 -InstallDir C:\tools\squad
```

## Updates

On launch (and every 15 minutes while open), `squad` checks GitHub for a newer release in the background (it
never delays startup). You can also run `squad update` (or `squad update --check`) at any time from your shell.
When a newer release exists, the manager asks whether to update:

- `y` downloads and installs it; `n` or `Esc` skips until the next launch.
  `Enter` deliberately does not accept, so a prompt that appears as you
  press it cannot start a download.
- The prompt waits while you are typing into a session or another input;
  the status bar shows `⬆ vX.Y.Z (U)` so you can press `U` in the manager whenever you are ready.
- Running sessions keep running. Restart `squad` to switch to the new build.
- Offline or blocked networks simply show no prompt. Set
  `AGENTSQUAD_NO_UPDATE_CHECK=1` to turn the check off.
- It uses the `curl.exe` and `tar.exe` that ship with Windows 10+, and
  refuses a download whose `--version` does not match the release tag.
  Debug builds (`cargo run`) never check or self-replace.

## Quickstart

```powershell
# Register an agent project (runs `claude` in that directory)
squad add C:\work\my-app -- claude

# Register a service project (runs a dev server)
squad add C:\work\my-api --kind service -- npm run dev

# Register a service without a path (runs where you are; named after the program)
squad add --kind service -- ollama serve

# Check what is registered
squad list

# Open the manager and start a project immediately
squad start my-app
```

Project names default to the directory name for agents, and to the program
name for services. Multiple services can share the same directory (automatically
disambiguated as `program`, `program-2`, etc.). Names are matched
case-insensitively by `remove` and `start`.

## The manager

`squad` with no arguments (or `squad start <name>`) opens the TUI manager:

- **Left:** project list, grouped under **Agents** and **Services** headers,
  each with its live status (`Running`, `Exited (code)`, `Stopped`, `Error: …`).
  Running rows spin while active and show uptime plus the session's CPU and
  memory (`Running 3m 12% 340M`). Usage covers the whole process tree
  (the agent and everything it spawned); CPU is a share of the entire
  machine, like Task Manager, and memory is the summed working set.
  Group headers show running/total counts (`Agents (1/2)`).
  An agent waiting for you (`?`) gets a yellow bar, and the question it is
  asking appears under its row.
  Service rows also show their command; agent rows show their program
  unless it is the default shell.
  The manager reopens on the project you had selected last time, and an
  agent that finished and sat quiet for a minute shows `idle 5m`.
- **Right:** the selected session's terminal output (full xterm emulation:
  colors, bold/italic/underline, cursor positioning).
- **Bottom:** a one-line status bar with hints, which doubles as the input
  line for send/filter mode and a popup host for help. Its right side shows
  whole-PC load (`CPU 23% · RAM 9.7G/16.0G`, yellow past 70%, red past 90%;
  hidden on terminals narrower than 70 columns) next to the running count.
  Usage is sampled every 2 seconds on a background thread, so it never
  slows typing or redraws.

### Keys

| Key | Action |
|-----|--------|
| `Enter` | Open the selected session: start it if stopped, then focus its terminal |
| Click | Click a project to select it; click a live terminal to focus it; right-click a row to open its quick actions |
| `j` / `k`, `↑` / `↓` | Move selection (`n` / `p` still work) |
| `1`–`9` | Jump to a visible row |
| `Home` / `End` | First / last project |
| `/` | Filter the project list by name, path, or command (`Enter` keeps, `Esc` clears; `Esc` in the manager drops a kept filter) |
| `a` | Add a project without leaving the manager (form previews the registered name and offers an autostart toggle) |
| `m` | Message: type a line and send it to the selected session (starts it if stopped) |
| `s` | Stop the selected session (asks to confirm with `y`; process is killed, files untouched; `x` also works) |
| `r` | Restart the selected session (asks to confirm with `y` when running) |
| `d` / `Delete` | Remove the selected project (asks to confirm with `y`; a running session is stopped, files untouched) |
| `S` | Start all stopped sessions |
| `U` | Open the update prompt when a new release has been detected |
| `PgUp` / `PgDn` | Scroll the selected terminal (`Shift+PgUp` / `Shift+PgDn` while inside it; `Shift+End` or typing returns to live) |
| Mouse wheel | Scroll the selected terminal (hold `Shift` to select text for copy) |
| `Ctrl+Q`, middle-click | Back to the manager (from inside a terminal; clicking the project list works too) |
| `Alt+1`–`Alt+9` | Switch to that project, also from inside a terminal (a stopped one lands on the manager) |
| `?` | Toggle the help popup |
| `q`, `Ctrl+C` | Quit (asks to confirm when sessions exist; all managed child processes are stopped; nothing runs detached) |

Send, filter, and add inputs support `←` / `→` / `Home` / `End` / `Delete`
for cursor-aware editing.

Inside a terminal, every key reaches the session, including `Esc` and
`Ctrl+G`, which agents such as Claude Code use to interrupt, rewind or open
an editor. squad keeps only `Ctrl+Q` (back to the manager) and
`Alt+1`–`Alt+9` (switch project) for itself.

## CLI reference

```
squad                                                              Open the manager
squad add [<path>] [--kind agent|service] [--autostart] [-- command args...]
                                                                   Register a project (path optional for services)
squad list                                                         List projects (name, kind, path, command)
squad remove <name>                                                Unregister (files are untouched)
squad start <name>                                                 Open the manager on a project and start it
squad update [--check]                                             Install the newest release (--check only looks)
squad --version
squad --help
```

Notes:

- Without `-- command`, the project opens a plain PowerShell session (`pwsh.exe` if installed, otherwise `powershell.exe`).
- `--autostart` makes the project start by itself whenever the manager opens.
- `--kind` defaults to `agent`. Existing config files without a kind keep working.
- Services may omit the path: they run in the current directory and are
  named after the program (e.g. `ollama` for `ollama serve`).
- `squad add <path> -- claude --dangerously-skip-permissions` style args are
  passed through verbatim after `--`.

## Configuration

Projects live in a single TOML file:

- `%LOCALAPPDATA%\AgentSquad\config.toml`, or
- wherever `AGENTSQUAD_CONFIG` points.

Example:

```toml
[[projects]]
name = "my-app"
path = "C:/work/my-app"
command = ["claude"]
kind = "agent"

[[projects]]
name = "my-api"
path = "C:/work/my-api"
command = ["npm", "run", "dev"]
kind = "service"
```

Optional settings:

- `autostart = true` on a project starts its session when the manager opens
  (`squad add --autostart` sets it too).
- `bell = false` at the top level silences the terminal bell. By default squad
  rings it once (at most every 3 seconds) when an agent you are not typing
  into starts waiting for input (`?`) or finishes (`✔`).
- `scrollback = 5000` sets the lines kept per session (default 2000).

The file is plain TOML — you can edit it by hand. Deleting it resets to empty.

## Building from source (developers)

You need the Rust stable toolchain plus a MinGW linker on Windows.
The helper script bootstraps a portable toolchain automatically (one-time
~200 MB download into `%TEMP%`) and forwards all arguments to cargo:

```powershell
powershell -ExecutionPolicy Bypass -File tools\cargo-windows.ps1 build
powershell -ExecutionPolicy Bypass -File tools\cargo-windows.ps1 test
```

Tests cover config round-trips, kind parsing, sidebar ordering, filtering,
group counts and filter-title counts, status colors and uptime labels, help
content, keyboard encoding including xterm modifiers, line editing,
stop/quit confirmations, session start helpers and config merging, truecolor
advertising, terminal parsing, and live PTY smoke tests (spawn output,
process-tree kill). Releases are built by GitHub Actions on every `v*` tag
(see [.github/workflows/release.yml](.github/workflows/release.yml)).

## Troubleshooting

- **`squad` not found after install:** the installer refreshes the
  current session's `PATH`; if it still fails, open a new terminal so the
  updated user `PATH` takes effect.
- **Session shows `Error: …`:** the start command failed — check that the
  binary (e.g. `claude`, `node`) is on your `PATH` and the project path exists.
- **`Exited (1)` right after start:** the process ran and exited; select it
  and read the terminal pane for its own error message.
- **Wrong project starts with `squad start`:** names match case-insensitively;
  make sure the name is unique.

## License

MIT. See [LICENSE](LICENSE).
