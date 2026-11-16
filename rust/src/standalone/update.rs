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

// ------------------------------------------------------------------ plugins (VST3 / CLAP)

/// Where Jwala's plugins may be installed, per format: the release's bundle name and the folders
/// hosts scan. Only copies that already exist are updated; nothing is installed fresh.
fn plugin_dirs() -> Vec<(&'static str, PathBuf)> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut v = Vec::new();
    if cfg!(windows) {
        if let Some(c) = env("CommonProgramFiles") {
            v.push(("Jwala.vst3", c.join("VST3")));
            v.push(("Jwala.clap", c.join("CLAP")));
        }
        if let Some(l) = env("LOCALAPPDATA") {
            v.push(("Jwala.vst3", l.join("Programs/Common/VST3")));
            v.push(("Jwala.clap", l.join("Programs/Common/CLAP")));
        }
    } else if cfg!(target_os = "macos") {
        let mut roots = vec![PathBuf::from("/Library/Audio/Plug-Ins")];
        roots.extend(env("HOME").map(|h| h.join("Library/Audio/Plug-Ins")));
        for r in roots {
            v.push(("Jwala.vst3", r.join("VST3")));
            v.push(("Jwala.clap", r.join("CLAP")));
        }
    } else {
        if let Some(h) = env("HOME") {
            v.push(("Jwala.vst3", h.join(".vst3")));
            v.push(("Jwala.clap", h.join(".clap")));
        }
        for r in ["/usr/lib", "/usr/local/lib"] {
            v.push(("Jwala.vst3", Path::new(r).join("vst3")));
            v.push(("Jwala.clap", Path::new(r).join("clap")));
        }
    }
    v
}

/// Installed plugins and the new build of each: `(installed, new)`.
fn plugin_targets(staged: &Path) -> Vec<(PathBuf, PathBuf)> {
    plugin_dirs().into_iter().map(|(name, dir)| (dir.join(name), staged.join(name))).filter(|(t, s)| t.exists() && s.exists()).collect()
}

/// A free name to move `p` aside to. A previous `.old` that is still loaded by a host can't be
/// deleted on Windows, so then the next free `.oldN` is used.
fn free_aside(p: &Path) -> PathBuf {
    let base = aside(p);
    remove_any(&base);
    if !base.exists() {
        return base;
    }
    (1..100).map(|i| PathBuf::from(format!("{}{i}", base.display()))).find(|c| {
        remove_any(c);
        !c.exists()
    }).unwrap_or(base)
}

/// Replaces `dst` with `src` file by file. Each existing file is renamed aside first, which works
/// even while a host has it loaded (it keeps the old one until it restarts).
fn replace_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        if !dst.is_dir() {
            return Err(std::io::Error::other(format!("{} isn't laid out like the new version", dst.display())));
        }
        for e in std::fs::read_dir(src)? {
            let e = e?;
            let to = dst.join(e.file_name());
            if e.path().is_dir() && !to.exists() {
                std::fs::create_dir_all(&to)?;
            }
            replace_tree(&e.path(), &to)?;
        }
        return Ok(());
    }
    if dst.is_dir() {
        return Err(std::io::Error::other(format!("{} isn't laid out like the new version", dst.display())));
    }
    let old = dst.exists().then(|| free_aside(dst));
    if let Some(old) = &old {
        std::fs::rename(dst, old)?;
    }
    if let Err(e) = std::fs::copy(src, dst) {
        if let Some(old) = &old {
            let _ = std::fs::rename(old, dst);
        }
        return Err(e);
    }
    Ok(())
}

/// Updates every installed copy of the plugins from `staged`. Copies in folders that need admin
/// rights are retried once through an elevated `--update-plugins` run (unless this is that run).
/// Returns what was updated and what wasn't, for the note shown after the restart.
pub fn install_plugins(staged: &Path, elevated: bool) -> (Vec<String>, Vec<String>) {
    let (mut done, mut problems, mut denied) = (Vec::new(), Vec::new(), false);
    for (target, src) in plugin_targets(staged) {
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match replace_tree(&src, &target) {
            Ok(()) => done.push(name),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && !elevated => denied = true,
            Err(e) => problems.push(format!("{name}: {e}")),
        }
    }
    if denied {
        match run_elevated(staged) {
            Ok(()) => done.push("the system-wide copies".into()),
            Err(e) => problems.push(e),
        }
    }
    (done, problems)
}

/// Runs `jwala --update-plugins <staged>` with admin rights and waits for it.
fn run_elevated(staged: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        elevate_windows(&exe, &format!("--update-plugins \"{}\"", staged.display()))
    }
    #[cfg(target_os = "macos")]
    {
        let q = |p: &Path| p.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("do shell script \"'{}' --update-plugins '{}'\" with administrator privileges", q(&exe), q(staged));
        let ok = std::process::Command::new("osascript").args(["-e", &script]).status().map_err(|e| e.to_string())?.success();
        ok.then_some(()).ok_or_else(|| "the plugins in /Library weren't updated (admin access declined)".into())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let ok = std::process::Command::new("pkexec").arg(&exe).arg("--update-plugins").arg(staged).status().map_err(|e| format!("pkexec: {e}"))?.success();
        ok.then_some(()).ok_or_else(|| "the plugins in /usr weren't updated (admin access declined)".into())
    }
}

#[cfg(windows)]
fn elevate_windows(exe: &Path, params: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[repr(C)]
    struct ShellExecuteInfoW {
        cb_size: u32,
        f_mask: u32,
        hwnd: isize,
        verb: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
        inst_app: isize,
        id_list: *mut std::ffi::c_void,
        class: *const u16,
        hkey_class: isize,
        hot_key: u32,
        icon_or_monitor: isize,
        process: isize,
    }
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn WaitForSingleObject(h: isize, ms: u32) -> u32;
        fn GetExitCodeProcess(h: isize, code: *mut u32) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (verb, file, params) = (wide("runas".as_ref()), wide(exe.as_os_str()), wide(params.as_ref()));
    let mut info = ShellExecuteInfoW {
        cb_size: std::mem::size_of::<ShellExecuteInfoW>() as u32,
        f_mask: 0x40 | 0x100, // SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC
        hwnd: 0,
        verb: verb.as_ptr(),
        file: file.as_ptr(),
        parameters: params.as_ptr(),
        directory: std::ptr::null(),
        show: 0, // SW_HIDE
        inst_app: 0,
        id_list: std::ptr::null_mut(),
        class: std::ptr::null(),
        hkey_class: 0,
        hot_key: 0,
        icon_or_monitor: 0,
        process: 0,
    };
    // fails when the UAC prompt is declined
    if unsafe { ShellExecuteExW(&mut info) } == 0 || info.process == 0 {
        return Err("the plugins in Program Files weren't updated (admin access declined)".into());
    }
    let mut code = 1u32;
    unsafe {
        WaitForSingleObject(info.process, 120_000);
        GetExitCodeProcess(info.process, &mut code);
        CloseHandle(info.process);
    }
    (code == 0).then_some(()).ok_or_else(|| "updating the plugins in Program Files failed".into())
}

/// Shown once by the new version after an update ("Updated to v… · plugins: …").
fn note_path() -> PathBuf {
    crate::presets::app_dir().join("update-note.txt")
}

/// The note the previous version left before restarting into this one, if any (read once).
pub fn take_note() -> Option<String> {
    let t = std::fs::read_to_string(note_path()).ok()?;
    let _ = std::fs::remove_file(note_path());
    Some(t.trim().to_string()).filter(|t| !t.is_empty())
}

// ------------------------------------------------------------------ the app itself

/// Swaps the unpacked build in for the running one (and updates installed plugins), then starts
/// it. On `Ok` the caller must exit right away; on `Err` the running app is left as it was.
pub fn install(staged: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // plugins first: they are read from `staged`, and the app's swap moves files out of it
    let (done, problems) = install_plugins(staged, false);
    let version = staged.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut note = format!("Updated to v{version}");
    if !done.is_empty() {
        note += &format!(" · plugins updated (restart your DAW): {}", done.join(", "));
    }
    if let Some(p) = problems.first() {
        note += &format!(" · {p}");
    }
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
    let _ = std::fs::write(note_path(), note);
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
    fn plugin_bundle_is_replaced_while_its_binary_is_open() {
        let root = std::env::temp_dir().join(format!("jwala-plugin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (src, dst) = (root.join("new/Jwala.vst3"), root.join("installed/Jwala.vst3"));
        for (d, body) in [(&src, "new"), (&dst, "old")] {
            std::fs::create_dir_all(d.join("Contents/x86_64-win")).unwrap();
            std::fs::write(d.join("Contents/x86_64-win/Jwala.vst3"), body).unwrap();
        }
        std::fs::write(src.join("Contents/moduleinfo.json"), "{}").unwrap();
        let bin = dst.join("Contents/x86_64-win/Jwala.vst3");
        // a host holding the old binary
        let held = std::fs::File::open(&bin).unwrap();
        replace_tree(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(&bin).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(aside(&bin)).unwrap(), "old");
        assert!(dst.join("Contents/moduleinfo.json").exists());
        // a second update while the first old copy is still held uses the next free name
        drop(held);
        let held = std::fs::File::open(aside(&bin)).unwrap();
        replace_tree(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(&bin).unwrap(), "new");
        drop(held);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn old_copy_sits_next_to_the_app() {
        assert_eq!(aside(Path::new("dir/Jwala.exe")), PathBuf::from("dir/Jwala.exe.old"));
    }
}
