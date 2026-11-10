# AgentSquad

[![ci](https://github.com/Hannoah/AgentSquad/actions/workflows/ci.yml/badge.svg)](https://github.com/Hannoah/AgentSquad/actions/workflows/ci.yml)

AgentSquad (`squad`) is a lightweight, project-based terminal session manager for Windows and macOS
(CI, releases and tests cover both).
Register your projects once, then open a single TUI manager to start, watch, and
interact with a terminal session per project — an ideal setup for running one
coding agent (or dev server) per project side by side.

Each project is either an **Agent** (an interactive terminal, e.g. `claude`) or a
**Service** (a long-running process, e.g. `npm run dev`). The sidebar groups them
accordingly, so running servers stay visible next to your agents.

## Install (macOS, no Rust required)

One line, straight from GitHub Releases (Apple Silicon and Intel are detected automatically):

```bash
curl -fsSL https://raw.githubusercontent.com/Hannoah/AgentSquad/main/tools/install.sh | bash
```

What this does:

- Downloads `squad-macos-aarch64.tar.gz` (Apple Silicon) or `squad-macos-x86_64.tar.gz` (Intel) from the latest GitHub Release.
- Verifies its SHA256 checksum and installs nothing if it does not match.
- Installs `squad` to `~/.local/bin` and tells you if that folder is not on your `PATH`.

```bash
squad --version
```

To pin a version or install elsewhere:

```bash
curl -fsSL https://raw.githubusercontent.com/Hannoah/AgentSquad/main/tools/install.sh | SQUAD_VERSION=v0.15.4 SQUAD_INSTALL_DIR=/usr/local/bin bash
```

On macOS, new sessions default to your `$SHELL`. Config lives in `~/.config/AgentSquad/config.toml`.
Process-tree CPU/RAM, the status bar load and the copy-to-clipboard action work on macOS too (`pbcopy`).

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
powershell -ExecutionPolicy Bypass -File .\tools\install.ps1 -Version v0.14.5
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
  `Enter` accepts too, so a prompt that appears as you press it starts the download.
- The prompt waits while you are typing into a session or another input;
  the status bar shows `⬆ vX.Y.Z (U)` so you can press `U` in the manager whenever you are ready.
- When the install finishes, squad asks whether to restart into the new build
  now (`y`) or later (`n` / `Esc`). A restart stops the running sessions (the
  prompt says how many), starts the new build on the project you had selected
  and starts every session that was running again (conversations of agents
  that cannot resume are lost). Choosing later keeps everything running; the
  new build is used the next time you start squad.
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

A path that does not exist is an error unless you pass `--create`, which makes the
folder (parents included). In the add form, `Enter` on a missing folder asks once
(`Enter` again creates it and saves; editing the path cancels).

Project names default to the directory name for agents, and to the program
name for services. Multiple agents and services can share the same directory
(automatically disambiguated as `my-app`, `my-app-2`, etc.), so two agents can
work on one project side by side. Names are matched
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
| Click | Click a project to select it; click a live terminal to focus it; right-click a row to open its quick actions (including **Set superior…**) |
| `j` / `k`, `↑` / `↓` | Move selection (`n` / `p` still work) |
| `1`–`9` | Jump to a visible row |
| `Home` / `End` | First / last project |
| `/` | Filter the project list by name, path, or command (`Enter` keeps, `Esc` clears; `Esc` in the manager drops a kept filter) |
| `a` | Add a project without leaving the manager (form checks the path as you type and previews the registered name; the Path field lists matching subfolders to pick with `↑`/`↓` + `Enter`, `Tab` completes) |
| `m` | Message: type a line and send it to the selected session (starts it if stopped) |
| `h` | History: read the selected project's work history, squad's own task entries and any SITREPs (`↑`/`↓` pick, `PgUp`/`PgDn` scroll, `Esc` closes) |
| `M` | Memory monitor: the 50 processes holding the most RAM on the machine (`↑`/`↓` pick, `r` refresh, `x` kill after a `y` confirm, `Esc` closes). Killing takes the process's children too; squad refuses system processes and the shell or terminal it runs in |
| `e` | Edit the selected project's command and autostart (path, kind and name stay; `u` sets whom it reports to). A running session keeps the old command until you restart it |
| `f` | Pin / unpin the selected project: pinned ones list first in their group (Agents or Services) and show a `*` after the name |
| `o` | Order: type a line into every agent under the selected commander |
| `u` | Set whom the selected agent reports to (empty clears it) |
| `s` | Stop the selected session (asks to confirm with `y`; process is killed, files untouched; `x` also works) |
| `r` | Restart the selected session (asks to confirm with `y` when running) |
| `d` / `Delete` | Remove the selected project (asks to confirm with `y`; a running session is stopped, files untouched) |
| `S` | Start all stopped sessions |
| `U` | Open the update prompt when a new release has been detected |
| `PgUp` / `PgDn` | Scroll the selected terminal (`Shift+PgUp` / `Shift+PgDn` while inside it; `Shift+End` or typing returns to live) |
| Drag | Select text in a terminal pane; it is copied to the clipboard when you let go (`Ctrl+C` copies a selection too) |
| Mouse wheel | Scroll the selected terminal |
| `Ctrl+Q`, middle-click | Back to the manager (from inside a terminal; clicking the project list works too) |
| `Alt+1`–`Alt+9` (macOS: `Option+1`–`Option+9`) | Switch to that project, also from inside a terminal (a stopped one lands on the manager) |
| `?` | Toggle the help popup |
| `q`, `Ctrl+C` | Quit (asks to confirm when sessions exist; all managed child processes are stopped; nothing runs detached) |

Send, filter, and add inputs support `←` / `→` / `Home` / `End` / `Delete`
for cursor-aware editing.

**Picking a path in the add form.** The folders under the Path field are
listed as you type: the subfolders matching the last segment (a trailing `\`
lists everything inside), or the folders your projects already live in while
the field is empty. `↓` / `↑` highlight a row, `Enter` (or a click) fills it in
and lists *its* subfolders, so you can walk down the tree; past the last row
`↓` moves to the next field, and `Enter` with nothing highlighted saves.
`Tab` completes the last segment against the disk, case-insensitively, up to
the part all matches share (or takes the highlighted row). The list sits right
under the Path row with a header naming the folder it lists (the part you typed
is bold), and the popup keeps a fixed height while you type. `✓ folder`,
`✗ not found` or `✗ not a folder` shows at the right end of the Path row right
away; a long path is shortened in the middle when you are not editing it.

**Selecting and copying terminal text.** Drag in a terminal pane (no `Shift`
needed): the text is highlighted and copied when you release the button, and
the status bar says how much was copied. Any key or click drops the highlight.
`Ctrl+C` with a selection copies it instead of reaching the session; with
nothing selected it goes to the session as usual. A click without a drag still
focuses the session, and clicking a stopped session restarts it only if you do
not drag, so you can select text from a finished session's output first. The
terminal's own `Shift`+drag selection still works too.

The terminal window title follows the selected project (`squad - my-app`) and
is restored when squad exits.

Inside a terminal, every key reaches the session, including `Esc` and
`Ctrl+G`, which agents such as Claude Code use to interrupt, rewind or open
an editor. squad keeps only `Ctrl+Q` (back to the manager) and
`Alt+1`–`Alt+9` (switch project) for itself.

On macOS the Alt key is Option, and a terminal normally sends the character
Option+digit types (`¡™£¢∞§¶•ª` on a US keyboard) instead of an Alt modifier.
squad reads those characters as `Option+1`–`Option+9`, so the switch works with
the default settings; the cost is that those nine characters cannot be typed
into a focused session. Turning on "Use Option as Meta key" (Terminal: Settings >
Profiles > Keyboard; iTerm2: Profiles > Keys > Left Option key = Esc+) makes
`Option+digit` arrive as a real Alt chord and frees the characters again. On
other keyboard layouts, use that setting or `Ctrl+Q` then the digit.

## CLI reference

```
squad                                                              Open the manager
squad add [<path>] [--kind agent|service] [--autostart] [--create] [--under <commander>] [-- command args...]
                                                                   Register a project (path optional for services)
squad list                                                         List projects (name, kind, path, command)
squad remove <name>                                                Unregister (files are untouched)
squad under <name> [<commander>]                                   Set whom an agent reports to (none clears it)
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
  rings it (at most once every 10 seconds) only when an agent you are not
  typing into starts waiting for input (`?`). A finished task never rings; it
  shows up as a count in the window title instead, e.g. `(2) squad - my-app`
  (agents waiting plus finished tasks you have not opened yet), which you can
  see on the tab or the taskbar. To keep the bell but make it silent, set
  Windows Terminal's `bellStyle` to `["window", "taskbar"]`: it then flashes
  instead of beeping.
- `mem_limit_mb = 4096` (default; `0` turns it off) flags a session whose process tree holds more RAM:
  the sidebar usage gets a `!` and the status bar warns once until it drops back.
- `scrollback = 5000` sets the lines kept per session (default 2000).

The file is plain TOML — you can edit it by hand. Deleting it resets to empty.

## Work history (automatic)

Every agent project gets a work history without any setup and without the
agent's help. Each time an agent finishes a task (or stops to ask you something,
or its session ends mid-task) squad files one entry: when it ended, how long it
took, the folder and command, the files changed and the commits made during the
task (from git), and the last lines on the agent's screen. Tasks shorter than
5 seconds are skipped as typing, not work. Press `h` to read them.

Entries are markdown files named `<UTC time>-auto.md` in the project's
`.squad/log/`, next to any SITREPs the agent files, so an agent that reads the
log at the start of its next session sees what was done before it. They include
what was on the screen, so treat them like terminal scrollback. Outside a git
repository the entry has no file or commit lists, only the screen. Services are
not recorded.

## Operations log (automatic for agents)

Agents forget everything when a session ends. The operations log gives a
project a memory. Every agent project gets one: squad creates the `.squad/`
folder when you register the project, when it opens the manager, and when the
session starts, wherever it is missing. An existing `.squad/` is never touched,
and a service only gets one if you set `ledger = true` on it
(`squad add --ledger`). The folder:

```
.squad/
  README.md    the protocol agents follow
  CONTEXT.md   the mission briefing: goal, decisions, standing orders, hazards
  log/         one file per piece of work: squad's task entries (`*-auto.md`)
               and the SITREPs agents file
  events.log   kept by squad: starts, stops, exit codes
```

An existing `.squad/` is never touched. In a git repository squad adds
`.squad/` to `.git/info/exclude` (local only; delete that line to commit
it). Press `b` on a running agent to type the order to read the log into it;
squad never does that by itself, because typing at the wrong moment could
answer a trust or permission dialog.

**Filing is enforced for Claude Code.** When a project has a ledger and its
command is `claude`, squad starts it with a Stop hook (`.squad/hook.json`,
passed with `--settings`; your own Claude settings are untouched). If the agent
tries to stop after changing files (as `git status` sees them) without filing a
SITREP since, the hook sends it back once to file one. It never loops, and
outside a git repository nothing is enforced. Agents' SITREPs are a bonus: the
work history below is recorded by squad whether or not an agent files anything.

Agents are told to treat `.squad/` as data, not authority: a SITREP is only an
order when it comes from the agent's own commander. Delete a `.squad/` that
arrived with a repository you did not write before you brief an agent in it.

### Chain of command

Agents can report to other agents. An agent with agents under it is a
commander (`★`): it becomes one the moment another agent attaches. Agents
sharing a folder are independent by default; name a commander with
`squad add C:\work\my-app --under my-app-2 -- claude`, the add form's
"Reports to" field, `u`, or right-click > **Set superior…**. Commanders can report to commanders, so the chain can
grow as deep as you need. The sidebar and `squad list` draw it as a tree:

```
★ my-app
├─★ my-app-2
│ └── my-app-4
└── my-app-3
```

Change it later with `u` on an agent in the manager, or from the shell:
`squad under my-app-4 my-app-3` (no commander clears it). Loops, such as a
commander reporting to its own agent, are refused.

- **Roster:** squad keeps `.squad/ROSTER.md` (who commands whom) current
  whenever the chain changes, so agents can read it at any time.
- **Briefing:** `b` points an agent at the log and the roster and tells it
  its name, its commander and its agents. It is a one-time prompt: press it
  after starting or changing agents.
- **Reporting in:** when an agent is registered under a commander, or moved
  there, squad files its transfer in the commander's `.squad/REPORTS.md`
  with the new head count ("Now 2 directly under app, 3 in all"); a
  commander that loses an agent hears that too. Joining a commander also
  turns the operations log on (`ledger = true`) for the agent and the
  commander and opens their `.squad/` right away; it is never turned off.
- **Reports:** when an agent finishes a task, squad appends a line to the
  same file (who, when, and their newest SITREP). Squad is the
  only writer and types nothing into any terminal. Commanders read the file.
- **Orders:** `o` on a commander types one line, prefixed `Order from "<name>":`,
  into every running agent under it, at any depth. An agent that is showing a
  question (`?`) is skipped, so the Enter cannot answer it.
- Give a commander and its agents the same project folder: that is where
  `.squad/` lives.

In the pane title an agent reads as `Engaged` (working), `Awaiting orders`
(needs input), `Task complete` or `Standby`.

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
- **Antivirus blocks `powershell.exe` when a session starts** (AhnLab V3 /
  Safe Transaction reports `Execution/MDP.Powershell.M1201`, "process
  creation"): a default-shell session is `powershell.exe` started by
  `squad.exe`, and behavior rules distrust an unsigned program that launches
  PowerShell. Every release is a new unknown file, so it can return after an
  update. Add the install folder `%LOCALAPPDATA%\AgentSquadin` (or
  `squad.exe`) to the product's trusted/exclusion list **by path**, not by
  hash, so updates stay covered, and report the false positive to the vendor.
  squad does not try to hide its child processes from the scanner.
- **Wrong project starts with `squad start`:** names match case-insensitively;
  make sure the name is unique.

## License

MIT. See [LICENSE](LICENSE).
