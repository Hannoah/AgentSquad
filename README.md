# AgentSquad

AgentSquad (`squad`) is a lightweight, project-based terminal session manager for Windows.
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

Open a **new** terminal afterwards, then verify:

```powershell
squad --version
```

To pin a version (default is latest):

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1 -Version v0.2.0
```

To install elsewhere:

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1 -InstallDir C:\tools\squad
```

## Quickstart

```powershell
# Register an agent project (runs `claude` in that directory)
squad add C:\work\my-app -- claude

# Register a service project (runs a dev server)
squad add C:\work\my-api --kind service -- npm run dev

# Check what is registered
squad list

# Open the manager and start a project immediately
squad start my-app
```

Project names default to the directory name. Names are matched
case-insensitively by `remove` and `start`.

## The manager

`squad` with no arguments (or `squad start <name>`) opens the TUI manager:

- **Left:** project list, grouped under **Agents** and **Services** headers,
  each with its live status (`Running`, `Exited (code)`, `Stopped`, `Error: …`).
  Service rows also show their command.
- **Right:** the selected session's terminal output (full xterm emulation:
  colors, bold/italic/underline, cursor positioning).
- **Bottom:** a one-line status bar with hints, which doubles as the input
  line for send/filter mode and a popup host for help.

### Keys

| Key | Action |
|-----|--------|
| `Enter` | Start the selected session / focus its terminal |
| `/` | Type a line and send it to the selected session (starts it if stopped) |
| `f` | Filter the project list by name or command (`Enter` keeps, `Esc` clears) |
| `r` | Restart the selected session |
| `n` / `p`, `↑` / `↓` | Move selection |
| `1`–`9` | Jump to a visible row |
| `a` | Add a project without leaving the manager |
| `x` | Stop the selected session (asks to confirm; process is killed, files untouched) |
| `Shift+PgUp` / `Shift+PgDn` | Scroll the selected terminal (`Shift+End` or typing returns to live) |
| `Ctrl+G` | Back to the manager (from inside a terminal) |
| `?` | Toggle the help popup |
| `q` | Quit (all managed child processes are stopped; nothing runs detached) |

Send, filter, and add inputs support `←` / `→` / `Home` / `End` / `Delete`
for cursor-aware editing.

Inside a terminal, `Ctrl+G` returns to the manager; pressing `g` right after
sends a literal `Ctrl+G` to the session instead.

## CLI reference

```
squad                                                              Open the manager
squad add <path> [--kind agent|service] [-- command args...]       Register a project
squad list                                                         List projects (name, kind, path, command)
squad remove <name>                                                Unregister (files are untouched)
squad start <name>                                                 Open the manager on a project and start it
squad --version
squad --help
```

Notes:

- Without `-- command`, the project opens a plain shell (`cmd.exe` on Windows).
- `--kind` defaults to `agent`. Existing config files without a kind keep working.
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

The file is plain TOML — you can edit it by hand. Deleting it resets to empty.

## Building from source (developers)

You need the Rust stable toolchain plus a MinGW linker on Windows.
The helper script bootstraps a portable toolchain automatically (one-time
~200 MB download into `%TEMP%`) and forwards all arguments to cargo:

```powershell
powershell -ExecutionPolicy Bypass -File tools\cargo-windows.ps1 build
powershell -ExecutionPolicy Bypass -File tools\cargo-windows.ps1 test
```

Tests cover config round-trips, kind parsing, sidebar ordering and filtering,
status colors, help content, keyboard encoding, terminal parsing, and a live
PTY smoke test. Releases are built by GitHub Actions on every `v*` tag
(see [.github/workflows/release.yml](.github/workflows/release.yml)).

## Troubleshooting

- **`squad` not found after install:** open a new terminal so the updated
  user `PATH` takes effect.
- **Session shows `Error: …`:** the start command failed — check that the
  binary (e.g. `claude`, `node`) is on your `PATH` and the project path exists.
- **`Exited (1)` right after start:** the process ran and exited; select it
  and read the terminal pane for its own error message.
- **Wrong project starts with `squad start`:** names match case-insensitively;
  make sure the name is unique.

## License

MIT. See [LICENSE](LICENSE).
