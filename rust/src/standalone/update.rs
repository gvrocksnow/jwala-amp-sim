//! Standalone self-update from the GitHub releases page.
//!
//! `check` asks GitHub for the latest release and compares its tag with this build's version.
//! `download` fetches this platform's zip, verifies it against the release's `.sha256` file and
//! unpacks it under `%AppData%\Jwala\update`. `install` (on the UI thread, after the state is saved
//! and audio stopped) renames the running app aside, moves the new one into its place and starts
//! it; the caller then exits. The renamed copy is deleted on the next launch (`cleanup`).
//!
//! The plugin never calls any of this: only the standalone checks for updates.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const REPO: &str = "gvrocksnow/jwala-amp-sim";
pub const RELEASES_PAGE: &str = "https://github.com/gvrocksnow/jwala-amp-sim/releases/latest";
/// This build's version; CI stamps the release tag into Cargo.toml.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(windows)]
const PLATFORM: &str = "windows-x64";
#[cfg(target_os = "macos")]
const PLATFORM: &str = "macos-universal";
#[cfg(all(unix, not(target_os = "macos")))]
const PLATFORM: &str = "linux-x64";

/// Biggest download accepted (the release zips are ~30 MB).
const MAX_DOWNLOAD: u64 = 300 << 20;

#[derive(Clone, Debug)]
pub struct Release {
    /// Without the leading `v`.
    pub version: String,
    zip_url: String,
    sha_url: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    /// Downloaded, verified and unpacked into this folder; waiting for `install`.
    Ready(Release, PathBuf),
    Failed(String),
}

/// Shared between the UI and the check/download threads.
#[derive(Clone, Default)]
pub struct Updater {
    status: Arc<Mutex<Status>>,
    /// Bumped on every status change, so the UI knows to redraw.
    gen: Arc<AtomicU32>,
}

impl Updater {
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    pub fn generation(&self) -> u32 {
        self.gen.load(Ordering::Relaxed)
    }

    fn set(&self, s: Status) {
        *self.status.lock().unwrap() = s;
        self.gen.fetch_add(1, Ordering::Relaxed);
    }

    pub fn busy(&self) -> bool {
        matches!(self.status(), Status::Checking | Status::Downloading(_))
    }

    /// Asks GitHub for the latest release, on a background thread.
    pub fn check(&self) {
        if self.busy() {
            return;
        }
        self.set(Status::Checking);
        let me = self.clone();
        std::thread::spawn(move || me.set(latest().unwrap_or_else(Status::Failed)));
    }

    /// Downloads and unpacks the release found by `check`, on a background thread.
    pub fn download(&self) {
        let Status::Available(rel) = self.status() else { return };
        self.set(Status::Downloading(rel.clone()));
        let me = self.clone();
        std::thread::spawn(move || {
            me.set(match stage(&rel) {
                Ok(dir) => Status::Ready(rel, dir),
                Err(e) => Status::Failed(e),
            })
        });
    }

    /// Puts a failed or finished state back to idle (e.g. after its message was shown).
    pub fn reset(&self) {
        self.set(Status::Idle);
    }
}

fn agent() -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| format!("HTTPS unavailable: {e}"))?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(Arc::new(tls))
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        // GitHub's API rejects requests without a User-Agent
        .user_agent(&format!("Jwala/{VERSION}"))
        .build())
}

fn net_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(404, _) => "no release has been published yet".into(),
        ureq::Error::Status(403 | 429, _) => "GitHub is rate-limiting update checks — try again in an hour".into(),
        ureq::Error::Status(code, _) => format!("GitHub answered {code}"),
        ureq::Error::Transport(_) => "couldn't reach GitHub — check your internet connection".into(),
    }
}

/// `"1.2.3"` → `(1, 2, 3)`; missing or non-numeric parts count as 0.
fn parse_version(v: &str) -> (u64, u64, u64) {
    let mut it = v.trim().trim_start_matches('v').split(['.', '-', '+']).map(|p| p.parse::<u64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

fn latest() -> Result<Status, String> {
    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }
    #[derive(serde::Deserialize)]
    struct Rel {
        tag_name: String,
        assets: Vec<Asset>,
    }
    let body = agent()?
        .get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(net_err)?
        .into_string()
        .map_err(|e| e.to_string())?;
    let rel: Rel = serde_json::from_str(&body).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    let version = rel.tag_name.trim_start_matches('v').to_string();
    if parse_version(&version) <= parse_version(VERSION) {
        return Ok(Status::UpToDate);
    }
    let suffix = format!("-{PLATFORM}.zip");
    let zip = rel.assets.iter().find(|a| a.name.ends_with(&suffix)).ok_or_else(|| format!("v{version} has no {PLATFORM} download"))?;
    let sha_name = format!("{}.sha256", zip.name);
    let sha_url = rel.assets.iter().find(|a| a.name == sha_name).map(|a| a.browser_download_url.clone());
    Ok(Status::Available(Release { version, zip_url: zip.browser_download_url.clone(), sha_url }))
}

pub fn update_dir() -> PathBuf {
    crate::presets::app_dir().join("update")
}

/// Downloads, verifies and unpacks `rel`; returns the folder holding the new build.
fn stage(rel: &Release) -> Result<PathBuf, String> {
    use sha2::Digest;
    let sha_url = rel.sha_url.as_ref().ok_or("this release has no checksum file, so it can't be installed automatically — download it from the release page")?;
    let want = agent()?.get(sha_url).call().map_err(net_err)?.into_string().map_err(|e| e.to_string())?;
    let want = want.split_whitespace().next().unwrap_or("").to_ascii_lowercase();

    let mut bytes = Vec::new();
    agent()?
        .get(&rel.zip_url)
        .call()
        .map_err(net_err)?
        .into_reader()
        .take(MAX_DOWNLOAD)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("download interrupted: {e}"))?;
    let got = format!("{:x}", sha2::Sha256::digest(&bytes));
    if want.len() != 64 || got != want {
        return Err("the download didn't match its checksum — not installing it".into());
    }

    let dir = update_dir().join(&rel.version);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    unzip(&bytes, &dir)?;
    // the zip holds one folder, `Jwala-<platform>`
    let root = dir.join(format!("Jwala-{PLATFORM}"));
    if !new_app(&root).exists() {
        return Err("the download doesn't contain the app".into());
    }
    Ok(root)
}

fn unzip(bytes: &[u8], dir: &Path) -> Result<(), String> {
    let mut ar = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("broken download: {e}"))?;
    for i in 0..ar.len() {
        let mut f = ar.by_index(i).map_err(|e| format!("broken download: {e}"))?;
        // `enclosed_name` rejects absolute paths and `..`, so nothing lands outside `dir`
        let Some(name) = f.enclosed_name() else { continue };
        let out = dir.join(name);
        if f.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut w = std::fs::File::create(&out).map_err(|e| format!("can't write {}: {e}", out.display()))?;
        std::io::copy(&mut f, &mut w).map_err(|e| format!("can't unpack {}: {e}", out.display()))?;
        #[cfg(unix)]
        if let Some(mode) = f.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(())
}

/// The new standalone inside an unpacked release.
fn new_app(root: &Path) -> PathBuf {
    if cfg!(windows) {
        root.join("Jwala.exe")
    } else if cfg!(target_os = "macos") {
        root.join("Jwala.app")
    } else {
        root.join("Jwala")
    }
}

/// What gets replaced: the running executable, or on macOS the `.app` bundle around it.
fn install_target(exe: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        // …/Jwala.app/Contents/MacOS/jwala → …/Jwala.app
        if let Some(bundle) = exe.ancestors().nth(3) {
            if bundle.extension().is_some_and(|e| e == "app") {
                return bundle.to_path_buf();
            }
        }
    }
    exe.to_path_buf()
}

fn aside(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

fn remove_any(p: &Path) {
    if p.is_dir() {
        let _ = std::fs::remove_dir_all(p);
    } else {
        let _ = std::fs::remove_file(p);
    }
}

/// Moves `from` to `to`, copying when they are on different drives.
fn move_path(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_path(from, to)
}

fn copy_path(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_path(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// Swaps the unpacked build in for the running one and starts it. On `Ok` the caller must exit
/// right away; on `Err` the running app is left as it was.
pub fn install(staged: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let target = install_target(&exe);
    let new = if target == exe && cfg!(target_os = "macos") {
        // a bare binary outside a bundle (a dev build): replace just the binary
        new_app(staged).join("Contents/MacOS/jwala")
    } else {
        new_app(staged)
    };
    let old = aside(&target);
    remove_any(&old);
    // a running .exe can't be overwritten on Windows, but it can be renamed
    std::fs::rename(&target, &old).map_err(|e| format!("can't replace {} ({e}) — download the update from the release page instead", target.display()))?;
    if let Err(e) = move_path(&new, &target) {
        remove_any(&target);
        let _ = std::fs::rename(&old, &target);
        return Err(format!("couldn't install the update: {e}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755));
    }
    // same path as before; on macOS that is the binary inside the new bundle
    std::process::Command::new(&exe).args(std::env::args_os().skip(1)).spawn().map_err(|e| format!("installed, but couldn't restart: {e} — start Jwala again"))?;
    Ok(())
}

/// Deletes what the last update left behind: the renamed old app and the unpacked download.
/// Runs a moment after launch, once the previous process has surely exited.
pub fn cleanup() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(5));
        if let Ok(exe) = std::env::current_exe() {
            remove_any(&aside(&install_target(&exe)));
        }
        let _ = std::fs::remove_dir_all(update_dir());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(parse_version("v0.10.0") > parse_version("0.9.9"));
        assert!(parse_version("1.0") > parse_version("0.99.99"));
        assert_eq!(parse_version("v0.3.0"), parse_version("0.3.0"));
        assert_eq!(parse_version("0.3.0-beta"), (0, 3, 0));
    }

    #[test]
    fn unzip_keeps_files_inside_the_folder() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            w.start_file(format!("Jwala-{PLATFORM}/README.md"), o).unwrap();
            w.write_all(b"hi").unwrap();
            w.start_file("../escaped.txt", o).unwrap();
            w.write_all(b"bad").unwrap();
            w.finish().unwrap();
        }
        let dir = std::env::temp_dir().join(format!("jwala-unzip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unzip(buf.get_ref(), &dir).unwrap();
        assert_eq!(std::fs::read(dir.join(format!("Jwala-{PLATFORM}/README.md"))).unwrap(), b"hi");
        assert!(!dir.parent().unwrap().join("escaped.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Talks to GitHub: `cargo test --lib live_check -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_check() {
        let s = latest();
        println!("running v{VERSION}: {s:?}");
        assert!(s.is_ok(), "{s:?}");
    }

    #[test]
    fn old_copy_sits_next_to_the_app() {
        assert_eq!(aside(Path::new("dir/Jwala.exe")), PathBuf::from("dir/Jwala.exe.old"));
    }
}
