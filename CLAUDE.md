# AgentSquad Agent Instructions

## Build & Test

- Format check: `cargo fmt --check`
- Lint: `cargo clippy --all-targets -- -D warnings`
- Tests: `cargo test --locked` (Windows GNU environments without a local MinGW linker can use `powershell -ExecutionPolicy Bypass -File tools\cargo-windows.ps1 test`)
- Release build: `cargo build --release --locked`

## Mandatory Post-Modification Release & Deploy Pipeline

Whenever a feature, bug fix, or code change is completed and verified, **always** finish by running the release and deployment pipeline so the version is bumped, the local binary is updated, and GitHub Release CI runs automatically:

```powershell
powershell -ExecutionPolicy Bypass -File tools\release.ps1 -Summary "<short English summary>" -Notes "<Korean summary for wiki/releases.md>"
```

What `tools\release.ps1` does automatically:
1. Increments the version in `Cargo.toml`, `Cargo.lock`, `README.md`, and prepends a row to `wiki/releases.md`.
2. Runs `cargo fmt`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked`, and `cargo build --release --locked`.
3. Installs `target\release\squad.exe` to `%LOCALAPPDATA%\AgentSquad\bin\squad.exe` and verifies `squad --version`.
4. Commits all changes (`Release vX.Y.Z: ...`), creates tag `vX.Y.Z`, and pushes `main` + `vX.Y.Z` to `origin` (which triggers `.github/workflows/ci.yml` and `.github/workflows/release.yml`).
