//! `treetop update`: installs a release over the running binary in one step,
//! checked against its published SHA-256 as `install.sh` checks it, then
//! restarts `treetop serve`'s service so the phone gets it too. A treetop
//! already open notices its binary was replaced and offers to restart in
//! place (see `is_replaced`).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ureq::ResponseExt as _;

const REPO: &str = "anthonyb8/treetop";

/// How long one download may take.
const LIMIT: Duration = Duration::from_secs(120);

/// The largest archive accepted; a release is a few megabytes.
const LARGEST: u64 = 64 * 1024 * 1024;

/// Installs `version`, such as `v0.1.52`, else the latest release, over the
/// binary that is running, saying what it does as it goes.
pub fn run(version: Option<&str>) -> Result<()> {
    println!("{}", install(version, &mut |line| println!("{line}"))?);
    Ok(())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(LIMIT))
        .build()
        .into()
}

/// The running version, as its tag is written.
pub fn current() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// The latest release's tag, or why it could not be read.
pub fn latest_release() -> Result<String> {
    latest(&agent())
}

/// Whether release `tag` is newer than the one running, comparing their
/// numbers, so an older or unreadable tag never reads as an update.
pub fn is_newer(tag: &str) -> bool {
    let numbers = |tag: &str| -> Option<Vec<u64>> {
        tag.strip_prefix('v')?
            .split('.')
            .map(|part| part.parse().ok())
            .collect()
    };
    numbers(tag)
        .zip(numbers(&current()))
        .is_some_and(|(theirs, ours)| theirs > ours)
}

/// `run` without printing: `say` hears each step, and the result says what
/// was installed and whether the service was restarted on it.
pub fn install(version: Option<&str>, say: &mut dyn FnMut(&str)) -> Result<String> {
    let current = current();
    let agent = agent();
    let version = match version {
        Some(v) if v.starts_with('v') => v.to_string(),
        Some(v) => format!("v{v}"),
        None => latest(&agent)?,
    };
    let target = installed()?;
    let mut done = if version == current {
        format!(
            "treetop {current} is already installed at {}",
            target.display()
        )
    } else {
        let name = format!("treetop-{version}-linux-{}.tar.gz", arch()?);
        let url = format!("https://github.com/{REPO}/releases/download/{version}/{name}");
        say(&format!("Downloading treetop {version}..."));
        let archive = download(&agent, &url)?;
        let sum = String::from_utf8(download(&agent, &format!("{url}.sha256"))?)
            .context("the checksum file is not text")?;
        verify(&archive, &sum).with_context(|| format!("{name}: nothing was installed"))?;
        replace(&target, &unpack(&archive)?)?;
        format!(
            "treetop {current} -> {version} installed at {}",
            target.display()
        )
    };
    // Restarted even when the binary was already current, since a service
    // started before an earlier install still runs the old one.
    if crate::serve::restart_service()? {
        done.push_str("\ntreetop-serve restarted on it");
    }
    Ok(done)
}

/// The latest release's tag, read from the redirect GitHub serves for it
/// rather than the API, which rate-limits anonymous callers.
fn latest(agent: &ureq::Agent) -> Result<String> {
    let url = format!("https://github.com/{REPO}/releases/latest");
    let response = agent.head(&url).call().with_context(|| url.clone())?;
    let landed = response.get_uri().path().to_string();
    tag_of(&landed).with_context(|| format!("{url} led to {landed}, which names no release"))
}

/// The tag a release page's path ends in, `/o/r/releases/tag/v0.1.52`.
fn tag_of(path: &str) -> Option<String> {
    let tag = path.strip_prefix(&format!("/{REPO}/releases/tag/"))?;
    (tag.starts_with('v') && !tag.contains('/')).then(|| tag.to_string())
}

fn arch() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("amd64"),
        "aarch64" => Ok("arm64"),
        other => bail!("treetop ships no build for {other}"),
    }
}

fn download(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>> {
    agent
        .get(url)
        .call()
        .with_context(|| url.to_string())?
        .body_mut()
        .with_config()
        .limit(LARGEST)
        .read_to_vec()
        .with_context(|| url.to_string())
}

/// Checks `bytes` against a `sha256sum` line, `<hex>  <name>`.
fn verify(bytes: &[u8], sum: &str) -> Result<()> {
    let want = sum
        .split_whitespace()
        .next()
        .context("the checksum file is empty")?;
    let got: String = ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !got.eq_ignore_ascii_case(want) {
        bail!("checksum mismatch: expected {want}, got {got}");
    }
    Ok(())
}

/// The `treetop` binary inside a release archive, unpacked with the
/// system's `tar`.
fn unpack(archive: &[u8]) -> Result<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("treetop-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let unpacked = (|| {
        let file = dir.join("treetop.tar.gz");
        fs::write(&file, archive)?;
        let status = Command::new("tar")
            .arg("xzf")
            .arg(&file)
            .arg("-C")
            .arg(&dir)
            .status()
            .context("running tar")?;
        if !status.success() {
            bail!("tar could not unpack the release: {status}");
        }
        fs::read(dir.join("treetop")).context("the release holds no treetop binary")
    })();
    let _ = fs::remove_dir_all(&dir);
    unpacked
}

/// Writes `binary` beside `target` and renames it over, so the swap is
/// atomic and a treetop already running keeps the file it opened.
fn replace(target: &Path, binary: &[u8]) -> Result<()> {
    let partial = target.with_extension("new");
    let write = || -> std::io::Result<()> {
        fs::write(&partial, binary)?;
        fs::set_permissions(&partial, fs::Permissions::from_mode(0o755))?;
        fs::rename(&partial, target)
    };
    write().map_err(|err| {
        let _ = fs::remove_file(&partial);
        anyhow::anyhow!(
            "could not write {}: {err}; rerun as a user who can, or with install.sh",
            target.display()
        )
    })
}

/// The path of the running binary, as installed: what `/proc/self/exe`
/// names, without the ` (deleted)` Linux adds once the file was replaced.
fn installed() -> Result<PathBuf> {
    let exe = fs::read_link("/proc/self/exe")
        .or_else(|_| std::env::current_exe())
        .context("finding the running binary")?;
    Ok(without_deleted(&exe).0)
}

fn without_deleted(exe: &Path) -> (PathBuf, bool) {
    let text = exe.to_string_lossy();
    match text.strip_suffix(" (deleted)") {
        Some(path) => (PathBuf::from(path), true),
        None => (exe.to_path_buf(), false),
    }
}

/// Whether the running binary has been replaced on disk since it started,
/// as an update or a release install does; then the path to start instead.
pub fn is_replaced() -> Option<PathBuf> {
    let exe = fs::read_link("/proc/self/exe").ok()?;
    let (path, is_deleted) = without_deleted(&exe);
    (is_deleted && path.exists()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_release_is_the_tag_its_page_lands_on() {
        assert_eq!(
            tag_of("/anthonyb8/treetop/releases/tag/v0.1.52").as_deref(),
            Some("v0.1.52")
        );
        assert_eq!(
            tag_of("/anthonyb8/treetop/releases"),
            None,
            "no release yet"
        );
    }

    #[test]
    fn only_a_higher_release_reads_as_newer() {
        let current = current();
        let mut parts: Vec<u64> = current[1..]
            .split('.')
            .map(|p| p.parse().unwrap())
            .collect();
        assert!(!is_newer(&current), "the running one");
        parts[2] += 1;
        let next = format!("v{}.{}.{}", parts[0], parts[1], parts[2]);
        assert!(is_newer(&next));
        assert!(is_newer(&format!("v{}.{}.0", parts[0], parts[1] + 1)));
        assert!(!is_newer("v0.0.1"), "an older one");
        assert!(!is_newer("nightly"), "not a version");
    }

    #[test]
    fn an_archive_is_installed_only_when_its_checksum_matches() {
        let sum = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  hello";
        assert!(verify(b"hello", sum).is_ok());
        assert!(verify(b"hellO", sum).is_err());
        assert!(verify(b"hello", "").is_err());
    }

    #[test]
    fn a_replaced_binary_is_told_by_the_suffix_linux_gives_it() {
        let (path, is_deleted) =
            without_deleted(Path::new("/home/me/.local/bin/treetop (deleted)"));
        assert_eq!(path, Path::new("/home/me/.local/bin/treetop"));
        assert!(is_deleted);
        assert!(!without_deleted(Path::new("/usr/bin/treetop")).1);
    }
}
