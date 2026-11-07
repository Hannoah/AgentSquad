use anyhow::{bail, Context, Result};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

pub const REPO: &str = "Hannoah/AgentSquad";
pub const ASSET: &str = "squad-windows-x86_64.zip";
const CHECK_COOLDOWN_SECS: u64 = 3600; // 1 hour between network update checks

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
fn update_cache_path() -> Option<PathBuf> {
    let base = env::var_os("LOCALAPPDATA")
        .or_else(|| env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")))?;
    Some(base.join("AgentSquad").join("update-check.cache"))
}
fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
/// Cached wrapper around `latest_tag()` so repeated launches do not spawn
/// `curl.exe` on every startup (avoiding AV behavioral false positives).
pub fn latest_tag_cached() -> Option<String> {
    let now = now_epoch_secs();
    if let Some(cache_file) = update_cache_path() {
        if let Ok(content) = fs::read_to_string(&cache_file) {
            let mut parts = content.split_whitespace();
            if let (Some(ts_str), Some(tag)) = (parts.next(), parts.next()) {
                if let Ok(ts) = ts_str.parse::<u64>() {
                    if now.saturating_sub(ts) < CHECK_COOLDOWN_SECS && parse_version(tag).is_some()
                    {
                        return Some(tag.to_string());
                    }
                }
            }
        }
    }
    let tag = latest_tag()?;
    if let Some(cache_file) = update_cache_path() {
        if let Some(parent) = cache_file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&cache_file, format!("{now} {tag}\n"));
    }
    Some(tag)
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
/// Computes SHA256 hex string of a file in pure Rust (no external `certutil.exe` subprocess).
pub fn compute_sha256(file: &Path) -> Option<String> {
    let data = fs::read(file).ok()?;
    Some(sha256_hex(&data))
}

/// Pure-Rust FIPS 180-4 SHA-256 implementation.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 72) & !63);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    format!(
        "{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
        h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]
    )
}
