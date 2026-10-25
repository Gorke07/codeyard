//! File-writing helpers. Files the user edits by hand (settings, shortcuts, projects, themes) get truncated first when written with `std::fs::write`:
//! a crash, power loss or full disk leaves a partial or empty file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomic write: a temp file in the same folder (owner-only, `O_EXCL`), `fsync`, then `rename`. If the target is a symlink
/// (dotfile managers link `settings.jsonc`) the link is kept and the file it points to is updated; an existing file's permissions
/// are kept. On error the original file stays as it was.
pub fn write_atomic(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    let target: PathBuf = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target.file_name().map(|n| n.to_string_lossy().chars().take(60).collect::<String>()).unwrap_or_default();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let tmp = dir.join(format!(".{name}.codeyard-{}-{}-{nanos:x}.tmp", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let existing = std::fs::metadata(&target).ok();
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // While writing, an existing file's contents (possibly private) must not be readable by others: owner-only at first, the original permissions only after
            // the data is written. A new file gets 0666 & ~umask like `std::fs::write`.
            opts.mode(if existing.is_some() { 0o600 } else { 0o666 });
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data.as_ref())?;
        if let Some(m) = &existing {
            f.set_permissions(m.permissions())?;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("codeyard-fsutil-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_replace_whole_files_and_leave_no_temp_files() {
        let d = dir("basic");
        let f = d.join("settings.jsonc");
        write_atomic(&f, "one").unwrap();
        write_atomic(&f, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "two");
        let names: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["settings.jsonc"], "no temp file remains");
    }

    #[test]
    fn a_failed_write_keeps_the_old_content() {
        let d = dir("fail");
        let f = d.join("keep.json");
        std::fs::write(&f, "precious").unwrap();
        // The target is a directory: the rename fails and nothing else changes.
        let sub = d.join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert!(write_atomic(&sub, "x").is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "precious");
        assert_eq!(std::fs::read_dir(&d).unwrap().flatten().count(), 2, "temp file cleaned up after the failure");
        // A missing folder is an error, not a panic.
        assert!(write_atomic(&d.join("no/such/dir/file"), "x").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_permissions_are_preserved() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("link");
        let real = d.join("dotfiles-settings.jsonc");
        std::fs::write(&real, "old").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.join("settings.jsonc");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_atomic(&link, "new").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link is still a link");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
        // A new file gets the same permissions `std::fs::write` would give (umask applied), not the temp file's 0600.
        let (fresh, plain) = (d.join("fresh.json"), d.join("plain.json"));
        write_atomic(&fresh, "x").unwrap();
        std::fs::write(&plain, "x").unwrap();
        assert_eq!(std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777, std::fs::metadata(&plain).unwrap().permissions().mode() & 0o777);
    }
}
