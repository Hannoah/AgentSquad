use anyhow::{bail, Context, Result};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const REPO: &str = "Hannoah/AgentSquad";
pub const ASSET: &str = "squad-windows-x86_64.zip";
/// "v0.5.4" or "0.5.4" as (major, minor, patch); anything else is None.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.trim().trim_start_matches('v').split('.');
    let v = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(v)
}
pub fn is_newer(current: &str, latest: &str) -> bool {
    matches!((parse_version(current), parse_version(latest)), (Some(c), Some(l)) if l > c)
}
/// Debug builds never check (so `cargo run` cannot overwrite itself), and
/// `AGENTSQUAD_NO_UPDATE_CHECK` turns it off for offline or locked-down use.
pub fn update_check_enabled() -> bool {
    !cfg!(debug_assertions) && env::var_os("AGENTSQUAD_NO_UPDATE_CHECK").is_none()
}
/// Windows' own copy of a tool. A bare `tar` can resolve to Git's GNU tar,
/// which cannot read zip files, so go through System32 explicitly.
pub fn system_tool(name: &str) -> PathBuf {
    let root = env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join(name)
}
/// A child process with no console window and no stdin.
pub fn quiet(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}
/// Newest release tag. `releases/latest` redirects to `releases/tag/<tag>`,
/// so the final URL is enough: no API call, no rate limit, no JSON.
pub fn latest_tag() -> Option<String> {
    let out = quiet(system_tool("curl.exe"))
        .args(["-sL", "-m", "10", "-o", "NUL", "-w", "%{url_effective}"])
        .arg(format!("https://github.com/{REPO}/releases/latest"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8(out.stdout).ok()?;
    let tag = url.trim().rsplit_once("/tag/")?.1;
    parse_version(tag).map(|_| tag.to_string())
}
/// Replace `current` with `new`. Windows lets a running exe be renamed but
/// not overwritten, so move it aside first and put it back if the copy fails.
pub fn swap_binary(new: &Path, current: &Path) -> Result<()> {
    let mut old = current.as_os_str().to_owned();
    old.push(format!(".old-{}", std::process::id()));
    let old = PathBuf::from(old);
    fs::rename(current, &old)
        .with_context(|| format!("Cannot move {} aside", current.display()))?;
    if let Err(e) = fs::copy(new, current) {
        let _ = fs::remove_file(current);
        let _ = fs::rename(&old, current);
        return Err(e).with_context(|| format!("Cannot write {}", current.display()));
    }
    Ok(())
}
/// Delete `squad.exe.old-*` left by earlier updates. One that is still
/// running (another squad window) simply fails to delete and stays.
pub fn cleanup_old_binaries() {
    let Ok(exe) = env::current_exe() else { return };
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name()) else {
        return;
    };
    let prefix = format!("{}.old-", name.to_string_lossy());
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = fs::remove_file(entry.path());
        }
    }
}
/// Download release `tag` into `work`, unpack it, and check that the binary
/// really is that version. Returns the path of the verified `squad.exe`.
pub fn fetch_release(tag: &str, work: &Path) -> Result<PathBuf> {
    let zip = work.join(ASSET);
    let ok = quiet(system_tool("curl.exe"))
        .args(["-fsSL", "-m", "180", "-o"])
        .arg(&zip)
        .arg(format!(
            "https://github.com/{REPO}/releases/download/{tag}/{ASSET}"
        ))
        .stdout(Stdio::null())
        .status()?
        .success();
    if !ok {
        bail!("Download failed");
    }

    let sha_asset = format!("{ASSET}.sha256");
    let sha_file = work.join(&sha_asset);
    let sha_downloaded = quiet(system_tool("curl.exe"))
        .args(["-fsSL", "-m", "15", "-o"])
        .arg(&sha_file)
        .arg(format!(
            "https://github.com/{REPO}/releases/download/{tag}/{sha_asset}"
        ))
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if sha_downloaded && sha_file.exists() {
        if let Ok(expected_raw) = fs::read_to_string(&sha_file) {
            let expected = expected_raw
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if expected.len() == 64 {
                if let Some(actual) = compute_sha256(&zip) {
                    if actual != expected {
                        bail!("SHA256 checksum mismatch: expected {expected}, got {actual}");
                    }
                }
            }
        }
    }
    let ok = quiet(system_tool("tar.exe"))
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(work)
        .stdout(Stdio::null())
        .status()?
        .success();
    if !ok {
        bail!("Could not unpack the download");
    }
    let new_exe = work.join("squad.exe");
    let said = quiet(&new_exe).arg("--version").output()?;
    let said = String::from_utf8_lossy(&said.stdout);
    let want = format!("squad {}", tag.trim_start_matches('v'));
    if said.trim() != want {
        bail!("Downloaded build says '{}', expected '{want}'", said.trim());
    }
    Ok(new_exe)
}
/// Fetch release `tag` and swap it in for the running binary.
/// Slow: call from a worker thread.
pub fn install_update(tag: &str) -> Result<()> {
    let exe = env::current_exe()?;
    let work = env::temp_dir().join(format!("squad-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;
    let result = fetch_release(tag, &work).and_then(|new_exe| swap_binary(&new_exe, &exe));
    let _ = fs::remove_dir_all(&work);
    result
}
/// Progress of the background update, reported to the UI thread.
pub enum UpdateMsg {
    Available(String),
    Installed(String),
    Failed(String),
}
/// Computes SHA256 hex string of a file using Windows certutil.exe.
pub fn compute_sha256(file: &Path) -> Option<String> {
    let certutil = system_tool("certutil.exe");
    let out = quiet(certutil)
        .arg("-hashfile")
        .arg(file)
        .arg("SHA256")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let trimmed = line.trim().replace(' ', "");
        if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(trimmed.to_ascii_lowercase());
        }
    }
    None
}
