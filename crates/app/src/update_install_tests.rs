//! Tests for `update_install.rs`. The bundle swap runs against a fake `.app` in a temp folder and a real disk image
//! made with `hdiutil` (macOS; skipped visibly when the tools are missing, see `CODEYARD_REQUIRE_TOOLS`).

use std::cell::Cell;
use std::time::Duration;

use super::*;
use crate::update::tests::scratch;
use crate::update::{Asset, ErrorKind, Release, sha256_hex};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn update_install_kind_is_manual_in_a_debug_build() {
    assert_eq!(install_kind(), InstallKind::Manual);
}

#[test]
fn update_mac_bundle_is_found_from_the_executable_path() {
    let bundle = |p: &str| mac_bundle(Path::new(p));
    assert_eq!(bundle("/Applications/Codeyard.app/Contents/MacOS/codeyard"), Some("/Applications/Codeyard.app".into()));
    assert_eq!(bundle("/Users/a b/Apps/My Editor.app/Contents/MacOS/x"), Some("/Users/a b/Apps/My Editor.app".into()));
    assert_eq!(bundle("/work/codeyard/target/release/codeyard"), None, "a bare binary");
    assert_eq!(bundle("/Applications/Codeyard.app/Contents/Resources/codeyard"), None);
    assert_eq!(bundle("/Applications/Codeyard/Contents/MacOS/codeyard"), None, "not a .app");
    assert_eq!(bundle("codeyard"), None);
}

#[test]
fn update_dir_writable_really_tries() {
    let (dir, _g) = scratch("writable");
    assert!(dir_writable(&dir));
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "the probe file is removed");
    assert!(!dir_writable(&dir.join("missing")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let denied = !dir_writable(&dir);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(denied, "a read-only folder (this test is meaningless as root)");
    }
}

#[test]
fn update_windows_setup_command_line_is_silent_and_never_reboots() {
    let c = installer_command(Path::new(r"C:\Users\a b\updates\Codeyard-0.2.0-windows-x64-setup.exe"));
    assert_eq!(c.get_program(), r"C:\Users\a b\updates\Codeyard-0.2.0-windows-x64-setup.exe");
    let args: Vec<_> = c.get_args().collect();
    assert_eq!(args, ["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/NORESTARTAPPLICATIONS", "/relaunch=1"]);
}

#[test]
fn update_relaunch_passes_pid_and_path_as_arguments_not_script_text() {
    let app = Path::new("/Applications/My \"Code\"; rm -rf $HOME.app");
    let c = relaunch_with(4242, app, Path::new("/usr/bin/open"));
    assert_eq!(c.get_program(), "/bin/sh");
    let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(args[0], "-c");
    assert!(!args[1].contains("Applications") && !args[1].contains("4242"), "script text is constant");
    assert_eq!(args[3..], ["4242", app.to_str().unwrap(), "/usr/bin/open"]);
    let plan = InstallPlan::MacApp { app: "/Applications/Codeyard.app".into() };
    assert_eq!(relaunch_command(&plan).unwrap().get_args().nth(4).unwrap(), "/Applications/Codeyard.app");
    assert!(relaunch_command(&InstallPlan::WindowsInstaller).is_none());
}

#[cfg(unix)]
#[test]
fn update_relaunch_waits_for_the_process_to_exit_then_opens_the_app() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, _g) = scratch("relaunch");
    let marker = dir.join("opened");
    let opener = dir.join("fake-open");
    // printf, not echo: dash's echo would swallow the leading `-n` as its own option.
    write(&opener, &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\n", marker.display()));
    std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut target = Command::new("sleep").arg("0.6").spawn().unwrap();
    let mut helper = relaunch_with(target.id(), Path::new("/Apps/New Codeyard.app"), &opener).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "still waiting for the old process");
    target.wait().unwrap();
    assert!(helper.wait().unwrap().success());
    assert_eq!(read(&marker).trim(), "-n /Apps/New Codeyard.app");
}

#[test]
fn update_swap_replaces_the_app_and_keeps_the_old_one_aside() {
    let (dir, _g) = scratch("swap-ok");
    let (app, staged, old) = (dir.join("A.app"), dir.join("staged"), dir.join("old"));
    write(&app.join("v"), "1");
    write(&staged.join("v"), "2");
    swap(&app, &staged, &old, &|a, b| std::fs::rename(a, b)).unwrap();
    assert_eq!((read(&app.join("v")), read(&old.join("v"))), ("2".into(), "1".into()));
    assert!(!staged.exists());
}

#[test]
fn update_swap_failure_rolls_back_and_leaves_the_old_app_intact() {
    let (dir, _g) = scratch("swap-rollback");
    let (app, old) = (dir.join("A.app"), dir.join("old"));
    write(&app.join("v"), "1");
    let e = swap(&app, &dir.join("no-such-staged"), &old, &|a, b| std::fs::rename(a, b)).unwrap_err();
    assert!(matches!(e, UpdateError::Io { what: "put the new app in place", .. }), "{e}");
    assert_eq!(read(&app.join("v")), "1");
    assert!(!old.exists(), "moved back");
}

#[test]
fn update_swap_failure_without_rollback_names_where_the_old_app_is() {
    let (dir, _g) = scratch("swap-lost");
    let (app, staged, old) = (dir.join("A.app"), dir.join("staged"), dir.join("old"));
    write(&app.join("v"), "1");
    write(&staged.join("v"), "2");
    let calls = Cell::new(0);
    let flaky = |a: &Path, b: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() == 1 { std::fs::rename(a, b) } else { Err(io::Error::other("injected")) }
    };
    let e = swap(&app, &staged, &old, &flaky).unwrap_err();
    assert!(matches!(&e, UpdateError::RollbackFailed { backup, .. } if *backup == old), "{e}");
    assert_eq!(e.kind(), ErrorKind::Install);
    assert!(e.to_string().contains(old.to_str().unwrap()) && e.to_string().contains(app.to_str().unwrap()));
    assert_eq!(read(&old.join("v")), "1", "the previous version is still on disk");
}

fn downloaded(dir: &Path, bytes: &[u8], claimed: &[u8]) -> Downloaded {
    let path = dir.join("Codeyard-0.2.0-macos-arm64.dmg");
    std::fs::write(&path, bytes).unwrap();
    let asset = Asset { name: "Codeyard-0.2.0-macos-arm64.dmg".into(), url: String::new(), sha256: sha256_hex(claimed) };
    Downloaded { release: Release { version: "0.2.0".into(), notes: String::new(), asset: Some(asset) }, path }
}

#[test]
fn update_install_rechecks_the_hash_and_refuses_manual_installs() {
    let (dir, _g) = scratch("recheck");
    let swapped = downloaded(&dir, b"evil", b"good");
    let e = stage_and_install(&swapped, &InstallKind::MacBundle { app: dir.join("Codeyard.app") }).unwrap_err();
    assert!(matches!(e, UpdateError::HashMismatch(_)), "{e}");
    let fine = downloaded(&dir, b"good", b"good");
    let e = stage_and_install(&fine, &InstallKind::Manual).unwrap_err();
    assert!(matches!(e, UpdateError::ManualOnly), "{e}");
    let mut none = downloaded(&dir, b"good", b"good");
    none.release.asset = None;
    assert!(matches!(stage_and_install(&none, &InstallKind::Manual).unwrap_err(), UpdateError::NoInstaller));
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    const ID: &str = "com.example.fake";

    /// `None` (with a visible SKIPPED line) when the tools are missing. The guard serialises the tests: concurrent
    /// `hdiutil attach` calls fail with EAGAIN.
    fn tools() -> Option<std::sync::MutexGuard<'static, ()>> {
        static HDIUTIL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let ok = ["hdiutil", "codesign", "ditto", "plutil"].iter().all(|t| Command::new("/usr/bin/which").arg(t).output().is_ok_and(|o| o.status.success()));
        if !ok {
            crate::test_sshd::skip_missing("hdiutil,codesign");
        }
        ok.then(|| HDIUTIL.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn sh(tool: &str, args: &[&dyn AsRef<std::ffi::OsStr>]) {
        let out = Command::new(tool).args(args.iter().map(|a| a.as_ref())).output().unwrap();
        assert!(out.status.success(), "{tool}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// `<dir>/<name>` as a minimal bundle; ad-hoc signed when `sign`.
    fn fake_app(dir: &Path, name: &str, id: &str, version: &str, sign: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let app = dir.join(name);
        write(
            &app.join("Contents/Info.plist"),
            &format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict>\
                 <key>CFBundleIdentifier</key><string>{id}</string><key>CFBundleExecutable</key><string>fake</string>\
                 <key>CFBundleShortVersionString</key><string>{version}</string><key>CFBundlePackageType</key><string>APPL</string>\
                 </dict></plist>"
            ),
        );
        let exe = app.join("Contents/MacOS/fake");
        write(&exe, "#!/bin/sh\nexit 0\n");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        if sign {
            sh("codesign", &[&"--force", &"-s", &"-", &app]);
        }
        app
    }

    /// A disk image whose root holds the contents of `src`: a blank HFS+ image filled with `ditto`. (`create -srcfolder`
    /// waits a fixed 4 s on current macOS versions, and `makehybrid` leaves Finder info that `codesign --strict` rejects.)
    fn dmg_of(src: &Path, out: &Path) {
        sh("hdiutil", &[&"create", &"-quiet", &"-size", &"2m", &"-fs", &"HFS+", &"-volname", &"Codeyard", &out]);
        let point = out.with_extension("mnt");
        std::fs::create_dir(&point).unwrap();
        sh("hdiutil", &[&"attach", &"-quiet", &"-nobrowse", &"-mountpoint", &point, &out]);
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            sh("ditto", &[&entry.path(), &point.join(entry.file_name())]);
        }
        sh("hdiutil", &[&"detach", &"-quiet", &point]);
    }

    struct Setup {
        app: PathBuf,
        dmg: PathBuf,
        _g: crate::update::RemoveOnDrop,
        dir: PathBuf,
    }

    /// An installed v0.1.0 in `<dir>/apps` and a disk image holding `Codeyard.app` with the given id/version/signing.
    fn setup(name: &str, id: &str, version: &str, sign: bool) -> Setup {
        let (dir, _g) = scratch(name);
        let app = fake_app(&dir.join("apps"), "Codeyard.app", ID, "0.1.0", true);
        fake_app(&dir.join("image"), "Codeyard.app", id, version, sign);
        let dmg = dir.join("Codeyard-0.2.0-macos-arm64.dmg");
        dmg_of(&dir.join("image"), &dmg);
        Setup { app, dmg, _g, dir }
    }

    fn version_of(app: &Path) -> String {
        plist_string(app, "CFBundleShortVersionString").unwrap().unwrap()
    }

    fn assert_untouched_and_unmounted(s: &Setup) {
        assert_eq!(version_of(&s.app), "0.1.0", "the installed app is untouched");
        let leftovers: Vec<_> = std::fs::read_dir(s.app.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers, ["Codeyard.app"], "no staged or backup copies left");
        let info = Command::new("hdiutil").arg("info").output().unwrap();
        assert!(!String::from_utf8_lossy(&info.stdout).contains(s.dir.to_str().unwrap()), "the image is still mounted");
    }

    #[test]
    fn update_mac_install_replaces_the_bundle_and_unmounts() {
        let Some(_hdiutil) = tools() else { return };
        let s = setup("mac-ok", ID, "0.2.0", true);
        let plan = install_mac(&s.dmg, "0.2.0", &s.app).unwrap();
        assert_eq!(plan, InstallPlan::MacApp { app: s.app.clone() });
        assert_eq!(version_of(&s.app), "0.2.0");
        let names: Vec<_> = std::fs::read_dir(s.app.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["Codeyard.app"], "the old copy and the staging copy are gone");
        sh("codesign", &[&"--verify", &"--deep", &"--strict", &s.app]);
        let info = Command::new("hdiutil").arg("info").output().unwrap();
        assert!(!String::from_utf8_lossy(&info.stdout).contains(s.dir.to_str().unwrap()), "still mounted");
    }

    #[test]
    fn update_mac_install_rejects_a_foreign_bundle_identifier() {
        let Some(_hdiutil) = tools() else { return };
        let s = setup("mac-id", "com.evil.app", "0.2.0", true);
        let e = install_mac(&s.dmg, "0.2.0", &s.app).unwrap_err();
        assert!(matches!(&e, UpdateError::BundleMismatch(m) if m.contains("com.evil.app")), "{e}");
        assert_untouched_and_unmounted(&s);
    }

    #[test]
    fn update_mac_install_rejects_a_version_the_signed_list_did_not_promise() {
        let Some(_hdiutil) = tools() else { return };
        let s = setup("mac-version", ID, "0.2.1", true);
        let e = install_mac(&s.dmg, "0.2.0", &s.app).unwrap_err();
        assert!(matches!(&e, UpdateError::BundleMismatch(m) if m.contains("0.2.1")), "{e}");
        assert_untouched_and_unmounted(&s);
    }

    #[test]
    fn update_mac_install_rejects_a_bundle_that_fails_codesign() {
        let Some(_hdiutil) = tools() else { return };
        let s = setup("mac-codesign", ID, "0.2.0", false);
        let e = install_mac(&s.dmg, "0.2.0", &s.app).unwrap_err();
        assert!(matches!(&e, UpdateError::CodesignFailed(m) if m.contains("not signed")), "{e}");
        assert_untouched_and_unmounted(&s);
    }

    #[test]
    fn update_mac_install_rejects_an_image_without_the_app_and_a_file_that_is_no_image() {
        let Some(_hdiutil) = tools() else { return };
        let (dir, _g) = scratch("mac-noapp");
        let app = fake_app(&dir.join("apps"), "Codeyard.app", ID, "0.1.0", true);
        fake_app(&dir.join("image"), "Other.app", ID, "0.2.0", true);
        let dmg = dir.join("x.dmg");
        dmg_of(&dir.join("image"), &dmg);
        let e = install_mac(&dmg, "0.2.0", &app).unwrap_err();
        assert!(matches!(e, UpdateError::BundleMismatch(_)), "{e}");

        write(&dir.join("junk.dmg"), "not a disk image");
        let e = install_mac(&dir.join("junk.dmg"), "0.2.0", &app).unwrap_err();
        assert!(matches!(e, UpdateError::MountFailed(_)), "{e}");
        assert_eq!(version_of(&app), "0.1.0");
    }

    #[test]
    fn update_mac_install_needs_a_writable_folder_and_does_not_mount_without_one() {
        let Some(_hdiutil) = tools() else { return };
        use std::os::unix::fs::PermissionsExt;
        let s = setup("mac-ro", ID, "0.2.0", true);
        let apps = s.app.parent().unwrap();
        std::fs::set_permissions(apps, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = install_mac(&s.dmg, "0.2.0", &s.app);
        std::fs::set_permissions(apps, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = r.unwrap_err();
        assert!(matches!(&e, UpdateError::NoWritePermission(d) if d == apps), "{e}");
        assert_untouched_and_unmounted(&s);
    }
}
