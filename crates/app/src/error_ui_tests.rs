//! Negative cases: what does the app show when something breaks? Each test asserts the outcome a user
//! can see (error text, band, notice, the tab staying dirty), not internal state.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use gpui::TestAppContext;

use crate::qa_workbench_tests::{fresh, open, open_in, project, setup, setup_with_config};

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// If a file can't be opened, no tab opens and the reason is visible; the window stays intact.
#[gpui::test]
fn opening_a_missing_file_says_so_and_opens_no_tab(cx: &mut TestAppContext) {
    let dir = project("err-missing");
    let (wb, cx) = setup(cx);
    open(&wb, &dir, cx);
    open_in(&wb, &dir.join("src/does-not-exist.rs"), cx);
    let err = wb.read_with(cx, |w, _| w.open_error_text()).expect("an error is shown");
    assert!(
        err.to_lowercase().contains("does-not-exist.rs") || err.to_lowercase().contains("no such file") || err.to_lowercase().contains("not found"),
        "{err:?}"
    );
    assert!(wb.read_with(cx, |w, cx| w.open_paths(cx)).is_empty(), "no tab for a file that cannot be read");
}

/// No permission: the cause is reported as "permission" (not a generic "error").
#[gpui::test]
fn opening_an_unreadable_file_names_the_permission(cx: &mut TestAppContext) {
    let dir = project("err-unreadable");
    let secret = dir.join("src/secret.rs");
    std::fs::write(&secret, "fn x() {}\n").unwrap();
    chmod(&secret, 0o000);
    let (wb, cx) = setup(cx);
    open(&wb, &dir, cx);
    open_in(&wb, &secret, cx);
    let err = wb.read_with(cx, |w, _| w.open_error_text());
    chmod(&secret, 0o644);
    let err = err.expect("an error is shown");
    assert!(err.to_lowercase().contains("permission"), "{err:?}");
}

/// A file that can't be saved: the tab stays dirty (no change is lost) and the band says "Could not save".
#[gpui::test]
fn a_failed_save_keeps_the_edit_and_says_why(cx: &mut TestAppContext) {
    let dir = project("err-save");
    let (wb, cx) = setup(cx);
    open(&wb, &dir, cx);
    open_in(&wb, &dir.join("src/main.rs"), cx);
    let ed = wb.read_with(cx, |w, _| w.active_editor_entity()).unwrap();
    ed.update(cx, |e, cx| e.insert_at_for_test(0, "// edit\n", cx));
    // Make the folder unwritable: creating a temp file in it fails, and so does overwriting.
    chmod(&dir.join("src/main.rs"), 0o444);
    chmod(&dir.join("src"), 0o555);
    ed.update(cx, |e, cx| e.save_now(cx));
    cx.run_until_parked();
    let status = ed.read_with(cx, |e, _| e.status_for_test());
    let dirty = ed.read_with(cx, |e, _| e.is_dirty());
    chmod(&dir.join("src"), 0o755);
    chmod(&dir.join("src/main.rs"), 0o644);
    assert!(dirty, "the edit must not be lost: the tab stays dirty");
    assert!(status.as_deref().is_some_and(|s| s.contains("Could not save")), "{status:?}");
    assert!(std::fs::read_to_string(dir.join("src/main.rs")).unwrap().starts_with("fn main"), "the file on disk is untouched");
}

/// A broken `settings.jsonc`: the app starts, runs with defaults and tells the user the file is broken.
#[gpui::test]
fn a_broken_settings_file_is_reported_not_swallowed(cx: &mut TestAppContext) {
    let cfg = fresh("err-settings");
    std::fs::write(cfg.join("settings.jsonc"), "{ \"editor.fontSize\": 14, this is not json ").unwrap();
    let (wb, cx) = setup_with_config(cx, &cfg);
    wb.update_in(cx, |w, window, cx| w.reload_config_for_test(window, cx));
    cx.run_until_parked();
    let note = wb.read_with(cx, |w, _| w.notice_text()).unwrap_or_default();
    assert!(note.to_lowercase().contains("settings"), "the user is told the settings file is broken: {note:?}");
}

/// A broken `tasks.jsonc`: running a task doesn't silently do nothing, the cause is written out.
#[gpui::test]
fn a_broken_tasks_file_explains_itself(cx: &mut TestAppContext) {
    let dir = project("err-tasks");
    std::fs::create_dir_all(dir.join(".codeyard")).unwrap();
    std::fs::write(dir.join(".codeyard/tasks.jsonc"), "[ { \"label\": \"build\", ").unwrap();
    let (wb, cx) = setup(cx);
    open(&wb, &dir, cx);
    wb.update_in(cx, |w, window, cx| w.run_command_for_test("Tasks: Run Build Task", window, cx));
    cx.run_until_parked();
    let note = wb.read_with(cx, |w, _| w.notice_text()).unwrap_or_default();
    assert!(!note.is_empty(), "a broken tasks file must produce a message");
    assert!(note.to_lowercase().contains("task"), "{note:?}");
}
