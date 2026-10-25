//! The seam between the update dialog (`workbench_update.rs`) and the outside world: the network, the installer, the clock and
//! quitting. `CoreBackend` forwards to `update.rs` and `update_install.rs`; tests inject a fake with scripted outcomes, and
//! `ScriptedBackend` (developer builds only, `CODEYARD_DEV_UPDATE`) plays a whole update with canned data so the dialog can be
//! looked at without a signed release.

// Windows made by tests get a fake backend, so the real one and the developer scenarios are not built there.
#![cfg_attr(test, allow(dead_code))]

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use gpui::App;

use crate::update::{self, Asset, Downloaded, Progress, Release, RemoveOnDrop, UpdateError};
#[cfg(test)]
use crate::update_install::ManualReason;
use crate::update_install::{self, InstallKind, InstallPlan};

/// Everything the workbench needs from the world. The blocking methods run on a worker thread, never on the UI thread.
pub trait UpdateBackend: Send + Sync {
    /// The running version (`major.minor.patch`).
    fn version(&self) -> &str;
    fn check(&self) -> Result<Option<Release>, UpdateError>;
    /// Blocking; `progress` is called while the file arrives, `cancel` stops the transfer.
    fn download(&self, release: &Release, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<Downloaded, UpdateError>;
    fn stage_and_install(&self, dl: &Downloaded, kind: &InstallKind) -> Result<InstallPlan, UpdateError>;
    /// Deletes a downloaded installer that is not needed any more.
    fn discard(&self, dl: Downloaded);
    /// Starts the helper that opens the new version once this process has exited. Call it BEFORE quitting.
    fn relaunch(&self, plan: &InstallPlan) -> io::Result<()>;
    fn install_kind(&self) -> InstallKind;
    fn now(&self) -> SystemTime;
    /// Ends the application (the normal quit path has run before; this is the last step).
    fn quit(&self, cx: &mut App);
}

pub struct CoreBackend;

impl UpdateBackend for CoreBackend {
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn check(&self) -> Result<Option<Release>, UpdateError> {
        update::check(self.version())
    }

    fn download(&self, release: &Release, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<Downloaded, UpdateError> {
        update::download(release, progress, cancel)
    }

    fn stage_and_install(&self, dl: &Downloaded, kind: &InstallKind) -> Result<InstallPlan, UpdateError> {
        update_install::stage_and_install(dl, kind)
    }

    fn discard(&self, dl: Downloaded) {
        // The installer lives alone in its private folder; the guard logs a failure instead of hiding it.
        if let Some(dir) = dl.path().parent() {
            drop(RemoveOnDrop::new(dir.to_path_buf()));
        }
    }

    fn relaunch(&self, plan: &InstallPlan) -> io::Result<()> {
        match update_install::relaunch_command(plan) {
            Some(mut command) => command.spawn().map(drop),
            None => Ok(()),
        }
    }

    fn install_kind(&self) -> InstallKind {
        update_install::install_kind()
    }

    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn quit(&self, cx: &mut App) {
        cx.quit();
    }
}

/// Which story `ScriptedBackend` tells (`CODEYARD_DEV_UPDATE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    /// A newer release exists and everything works.
    Available,
    UpToDate,
    /// The update server cannot be reached.
    Offline,
    /// The download does not match the signed checksums.
    Untrusted,
    /// Everything verifies, but the app cannot be replaced.
    InstallFails,
}

impl Scenario {
    pub fn from_env(value: &str) -> Option<Scenario> {
        Some(match value {
            "available" => Scenario::Available,
            "uptodate" => Scenario::UpToDate,
            "offline" => Scenario::Offline,
            "untrusted" => Scenario::Untrusted,
            "install-fails" => Scenario::InstallFails,
            _ => return None,
        })
    }
}

const DEV_NOTES: &str = "## What's new\n\n- Faster start when a project has many tabs.\n- **Check for Updates…** in the Help menu.\n- Fixed a crash when a language server exits while a rename is running.\n\n## Fixes\n\n- The status bar no longer flickers while a search is running.\n- Soft-wrapped lines keep their indentation guides.\n\nSee the [release page](https://github.com/doancan/codeyard/releases) for the full list.\n";
const DEV_SIZE: u64 = 20_700_000;

/// Plays `Scenario` with canned data: the download crawls at 1 MB/s so a state can be photographed, nothing touches the disk
/// or the network, and quitting only prints a line. Never used in tests (they inject their own fake).
pub struct ScriptedBackend {
    scenario: Scenario,
}

impl ScriptedBackend {
    pub fn new(scenario: Scenario) -> Self {
        ScriptedBackend { scenario }
    }

    fn newer(&self) -> Release {
        Release {
            version: "0.2.0".into(),
            notes: DEV_NOTES.into(),
            asset: Some(Asset { name: "Codeyard-0.2.0-macos-arm64.dmg".into(), url: "https://example.invalid/Codeyard.dmg".into(), sha256: "0".repeat(64) }),
        }
    }
}

impl UpdateBackend for ScriptedBackend {
    fn version(&self) -> &str {
        "0.1.0"
    }

    fn check(&self) -> Result<Option<Release>, UpdateError> {
        std::thread::sleep(Duration::from_millis(1500));
        match self.scenario {
            Scenario::UpToDate => Ok(None),
            Scenario::Offline => Err(UpdateError::Network("curl: (6) Could not resolve host: github.com".into())),
            _ => Ok(Some(self.newer())),
        }
    }

    fn download(&self, _: &Release, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<Downloaded, UpdateError> {
        let mut done = 0;
        while done < DEV_SIZE {
            if cancel.load(Ordering::Relaxed) {
                return Err(UpdateError::Cancelled);
            }
            // The size is "unknown" for the first second, like a server that answers slowly.
            progress(Progress { done, total: (done > 1_000_000).then_some(DEV_SIZE) });
            std::thread::sleep(Duration::from_millis(100));
            done += 100_000;
        }
        match self.scenario {
            Scenario::Untrusted => Err(UpdateError::HashMismatch("Codeyard-0.2.0-macos-arm64.dmg".into())),
            _ => Ok(Downloaded { release: self.newer(), path: PathBuf::from("/dev/null") }),
        }
    }

    fn stage_and_install(&self, _: &Downloaded, _: &InstallKind) -> Result<InstallPlan, UpdateError> {
        std::thread::sleep(Duration::from_millis(2500));
        match self.scenario {
            Scenario::InstallFails => Err(UpdateError::NoWritePermission(PathBuf::from("/Applications"))),
            _ => Ok(InstallPlan::MacApp { app: PathBuf::from("/Applications/Codeyard.app") }),
        }
    }

    fn discard(&self, _: Downloaded) {}

    fn relaunch(&self, plan: &InstallPlan) -> io::Result<()> {
        eprintln!("dev update: would relaunch {plan:?}");
        Ok(())
    }

    fn install_kind(&self) -> InstallKind {
        InstallKind::MacBundle { app: PathBuf::from("/Applications/Codeyard.app") }
    }

    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn quit(&self, _: &mut App) {
        eprintln!("dev update: would quit now");
    }
}

/// The backend a new window uses: the real one, unless a developer build was asked to play a scenario.
#[cfg(not(test))]
pub fn default_backend(dev: bool) -> std::sync::Arc<dyn UpdateBackend> {
    if dev && let Some(scenario) = std::env::var("CODEYARD_DEV_UPDATE").ok().and_then(|v| Scenario::from_env(&v)) {
        return std::sync::Arc::new(ScriptedBackend::new(scenario));
    }
    std::sync::Arc::new(CoreBackend)
}

/// Windows made by tests start with a backend that fails every request, so nothing can reach the network by accident;
/// the tests that look at updates inject their own.
#[cfg(test)]
pub fn default_backend(_dev: bool) -> std::sync::Arc<dyn UpdateBackend> {
    std::sync::Arc::new(OfflineBackend)
}

#[cfg(test)]
struct OfflineBackend;

#[cfg(test)]
impl UpdateBackend for OfflineBackend {
    fn version(&self) -> &str {
        "0.1.0"
    }

    fn check(&self) -> Result<Option<Release>, UpdateError> {
        Err(UpdateError::Network("no network in tests".into()))
    }

    fn download(&self, _: &Release, _: &mut dyn FnMut(Progress), _: &AtomicBool) -> Result<Downloaded, UpdateError> {
        Err(UpdateError::Network("no network in tests".into()))
    }

    fn stage_and_install(&self, _: &Downloaded, _: &InstallKind) -> Result<InstallPlan, UpdateError> {
        Err(UpdateError::ManualOnly)
    }

    fn discard(&self, _: Downloaded) {}

    fn relaunch(&self, _: &InstallPlan) -> io::Result<()> {
        Ok(())
    }

    fn install_kind(&self) -> InstallKind {
        InstallKind::Manual(ManualReason::NotInstalled)
    }

    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn quit(&self, _: &mut App) {}
}
