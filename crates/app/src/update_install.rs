//! Installing a downloaded, hash-verified update: replace the macOS app bundle in place, or hand the setup program to
//! Windows. Entry point: `stage_and_install`; what the UI does afterwards is `relaunch_command`.
//!
//! macOS in short: mount the disk image read-only -> copy `Codeyard.app` next to the installed app (same volume, so the
//! last step is a rename) -> unmount -> check the copy -> `rename(installed, old)`, `rename(copy, installed)`, rolling
//! back on any failure -> delete the old copy. The installed app is untouched until every check has passed.
#![allow(dead_code)] // Used by the UI next.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::update::{Downloaded, RemoveOnDrop, UpdateError, io_err, private_dir, verify_file};

const APP_NAME: &str = "Codeyard.app";

/// How this copy of Codeyard can be updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// Running from a `.app` bundle whose folder we may write to: replaced in place.
    MacBundle { app: PathBuf },
    /// Installed by the Inno Setup installer: the new setup program upgrades it.
    WindowsInstalled,
    /// Anything else (debug build, bare binary, Linux, read-only `/Applications`, portable zip): the UI opens the release
    /// page and says that this copy cannot update itself.
    Manual,
}

/// What `stage_and_install` did; pass it to `relaunch_command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallPlan {
    /// The bundle at `app` now holds the new version; the running process still has the old code loaded.
    MacApp { app: PathBuf },
    /// The setup program is running (it closes this app and starts the new one itself).
    WindowsInstaller,
}

pub fn install_kind() -> InstallKind {
    // A debug build must never replace itself or be replaced by a release.
    if cfg!(debug_assertions) {
        return InstallKind::Manual;
    }
    let Ok(exe) = std::env::current_exe().and_then(std::fs::canonicalize) else { return InstallKind::Manual };
    if cfg!(target_os = "macos") {
        mac_bundle(&exe).filter(|app| app.parent().is_some_and(dir_writable)).map_or(InstallKind::Manual, |app| InstallKind::MacBundle { app })
    } else if cfg!(windows) && exe.with_file_name("unins000.exe").is_file() {
        InstallKind::WindowsInstalled
    } else {
        InstallKind::Manual
    }
}

/// `<name>.app` for an executable at `<name>.app/Contents/MacOS/<exe>`.
fn mac_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && app.extension()? == "app").then(|| app.to_path_buf())
}

/// Really tries to create a file: read-only volumes and ACLs do not show in the permission bits.
pub(crate) fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".codeyard-write-test-{}", std::process::id()));
    let created = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).is_ok();
    if created && let Err(e) = std::fs::remove_file(&probe) {
        eprintln!("could not remove {}: {e}", probe.display());
    }
    created
}

/// Installs the downloaded update. The hash is checked again here, so a file replaced after `download` is never opened.
/// macOS: replaces the bundle (see the module comment); on error the installed app is exactly as before, except for
/// `RollbackFailed`, whose message says where the previous version is. Windows: starts the setup program detached
/// (`installer_args`) and returns at once; the UI must quit now. Blocking (a few seconds on macOS).
pub fn stage_and_install(dl: &Downloaded, kind: &InstallKind) -> Result<InstallPlan, UpdateError> {
    let asset = dl.release.asset.as_ref().ok_or(UpdateError::NoInstaller)?;
    verify_file(&dl.path, &asset.sha256, &asset.name)?;
    match kind {
        InstallKind::MacBundle { app } => install_mac(&dl.path, &dl.release.version, app),
        InstallKind::WindowsInstalled => {
            installer_command(&dl.path).spawn().map_err(|e| UpdateError::Tool { tool: "installer", detail: e.to_string() })?;
            Ok(InstallPlan::WindowsInstaller)
        }
        InstallKind::Manual => Err(UpdateError::ManualOnly),
    }
}

// --- Windows ---

/// Silent, no dialogs, no reboot; `CloseApplications` lets Setup close this app. Setup's own restart of closed
/// applications is off and `/relaunch=1` makes `installer/windows/codeyard.iss` start the new version exactly once.
/// (Written from the Inno Setup documentation; not run on Windows yet.)
fn installer_args() -> [&'static str; 6] {
    ["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/NORESTARTAPPLICATIONS", "/relaunch=1"]
}

fn installer_command(setup: &Path) -> Command {
    let mut c = Command::new(setup);
    c.args(installer_args()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    c
}

// --- Relaunch ---

/// Waits (at most a minute) for `$1` to exit, then opens the app at `$2` as a new instance with `$3`.
/// Paths and the pid are positional arguments, never part of the script text.
const RELAUNCH_SCRIPT: &str = r#"i=0; while kill -0 "$1" 2>/dev/null; do i=$((i+1)); [ "$i" -le 300 ] || exit 1; sleep 0.2; done; exec "$3" -n "$2""#;

/// The command that starts the new version after this process has exited. Spawn it BEFORE quitting; it is detached
/// (no stdio, own process group) and survives us. `None` on Windows, where Setup starts the app.
pub fn relaunch_command(plan: &InstallPlan) -> Option<Command> {
    match plan {
        InstallPlan::MacApp { app } => Some(relaunch_with(std::process::id(), app, Path::new("/usr/bin/open"))),
        InstallPlan::WindowsInstaller => None,
    }
}

fn relaunch_with(pid: u32, app: &Path, opener: &Path) -> Command {
    let mut c = Command::new("/bin/sh");
    c.args(["-c", RELAUNCH_SCRIPT, "codeyard-relaunch"]).arg(pid.to_string()).arg(app).arg(opener);
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    c
}

// --- macOS ---

/// Runs a helper and returns its stdout; a failure carries its stderr.
fn run(tool: &'static str, cmd: &mut Command) -> Result<String, UpdateError> {
    let out = cmd.stdin(Stdio::null()).output().map_err(|e| UpdateError::Tool { tool, detail: e.to_string() })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(UpdateError::Tool { tool, detail: String::from_utf8_lossy(&out.stderr).trim().to_string() })
    }
}

/// A key of `<app>/Contents/Info.plist`; `None` when the file or the key is missing.
fn plist_string(app: &Path, key: &str) -> Result<Option<String>, UpdateError> {
    let out = Command::new("plutil")
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(app.join("Contents/Info.plist"))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| UpdateError::Tool { tool: "plutil", detail: e.to_string() })?;
    Ok(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

/// A mounted disk image; unmounted when dropped, on every path.
struct Mount {
    point: PathBuf,
}

impl Mount {
    fn attach(dmg: &Path) -> Result<Self, UpdateError> {
        static N: AtomicU32 = AtomicU32::new(0);
        // A private folder (the user's temp folder is owner-only on macOS) instead of /Volumes, where other programs look.
        let point = std::env::temp_dir().join(format!("codeyard-update-mnt-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        private_dir(&point)?;
        let args = ["attach", "-nobrowse", "-readonly", "-noverify", "-noautoopen", "-mountpoint"];
        if let Err(e) = run("hdiutil", Command::new("hdiutil").args(args).arg(&point).arg(dmg)) {
            if let Err(e) = std::fs::remove_dir(&point) {
                eprintln!("could not remove {}: {e}", point.display());
            }
            return Err(match e {
                UpdateError::Tool { detail, .. } => UpdateError::MountFailed(detail),
                other => other,
            });
        }
        Ok(Mount { point })
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let detach = |force: bool| {
            let mut c = Command::new("hdiutil");
            c.arg("detach").arg(&self.point);
            if force {
                c.arg("-force");
            }
            run("hdiutil", &mut c)
        };
        // A process (Spotlight, the copy we just made) can hold the volume for a moment: retry by force.
        if let Err(first) = detach(false)
            && let Err(second) = detach(true)
        {
            eprintln!("could not unmount {}: {first}; {second}", self.point.display());
            return;
        }
        if let Err(e) = std::fs::remove_dir(&self.point) {
            eprintln!("could not remove {}: {e}", self.point.display());
        }
    }
}

fn remove_leftover(path: &Path) -> Result<(), UpdateError> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_err("remove a leftover of an earlier update")(e)),
        _ => Ok(()),
    }
}

/// The staged copy must be our app, at the version the signed list promised, with an intact code signature.
fn verify_staged(staged: &Path, bundle_id: &str, version: &str) -> Result<(), UpdateError> {
    let mismatch = |what: &str, want: &str, got: Option<String>| {
        UpdateError::BundleMismatch(format!("{what} is {}, expected {want}", got.as_deref().unwrap_or("missing")))
    };
    let id = plist_string(staged, "CFBundleIdentifier")?;
    if id.as_deref() != Some(bundle_id) {
        return Err(mismatch("the app identifier", bundle_id, id));
    }
    let v = plist_string(staged, "CFBundleShortVersionString")?;
    if v.as_deref() != Some(version) {
        return Err(mismatch("the version", version, v));
    }
    run("codesign", Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(staged)).map(drop).map_err(|e| match e {
        UpdateError::Tool { detail, .. } => UpdateError::CodesignFailed(detail),
        other => other,
    })
}

/// `installed -> old`, `staged -> installed`; if the second step fails the first is undone.
/// `rename` is a parameter so that tests can fail either step.
fn swap(app: &Path, staged: &Path, old: &Path, rename: &dyn Fn(&Path, &Path) -> io::Result<()>) -> Result<(), UpdateError> {
    rename(app, old).map_err(io_err("move the installed app aside"))?;
    if let Err(e) = rename(staged, app) {
        return Err(match rename(old, app) {
            Ok(()) => UpdateError::Io { what: "put the new app in place", source: e },
            Err(cause) => UpdateError::RollbackFailed { app: app.to_path_buf(), backup: old.to_path_buf(), cause },
        });
    }
    Ok(())
}

pub(crate) fn install_mac(dmg: &Path, version: &str, app: &Path) -> Result<InstallPlan, UpdateError> {
    let parent = app.parent().ok_or_else(|| UpdateError::BundleMismatch("the installed app has no folder".into()))?;
    if !dir_writable(parent) {
        return Err(UpdateError::NoWritePermission(parent.to_path_buf()));
    }
    let bundle_id = plist_string(app, "CFBundleIdentifier")?.ok_or_else(|| UpdateError::BundleMismatch("the installed app has no identifier".into()))?;
    let pid = std::process::id();
    let (staged, old) = (parent.join(format!(".Codeyard.update-{pid}.app")), parent.join(format!(".Codeyard.old-{pid}.app")));
    remove_leftover(&staged)?;
    remove_leftover(&old)?;
    let staged_guard = RemoveOnDrop::new(staged.clone());
    {
        let mount = Mount::attach(dmg)?;
        let source = mount.point.join(APP_NAME);
        // A symlink here could point anywhere on the disk image's host.
        if !std::fs::symlink_metadata(&source).is_ok_and(|m| m.is_dir()) {
            return Err(UpdateError::BundleMismatch(format!("the disk image has no {APP_NAME}")));
        }
        run("ditto", Command::new("ditto").arg(&source).arg(&staged))?;
    } // unmounted here, before the checks
    verify_staged(&staged, &bundle_id, version)?;
    swap(app, &staged, &old, &|a, b| std::fs::rename(a, b))?;
    staged_guard.keep();
    // The new app is in place; a failure from here on must not turn the update into an error.
    if let Err(e) = std::fs::remove_dir_all(&old) {
        eprintln!("could not remove the previous version at {}: {e}", old.display());
    }
    if let Err(e) = clear_quarantine(app) {
        eprintln!("could not clear the quarantine flag of {}: {e}", app.display());
    }
    Ok(InstallPlan::MacApp { app: app.to_path_buf() })
}

/// Gatekeeper would ask again on first launch if the new bundle carried the download flag.
fn clear_quarantine(app: &Path) -> Result<(), UpdateError> {
    const FLAG: &str = "com.apple.quarantine";
    match run("xattr", Command::new("xattr").args(["-p", FLAG]).arg(app)) {
        Err(UpdateError::Tool { detail, .. }) if detail.contains("No such xattr") => return Ok(()),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    run("xattr", Command::new("xattr").args(["-dr", FLAG]).arg(app)).map(drop)
}

#[cfg(test)]
#[path = "update_install_tests.rs"]
mod tests;
