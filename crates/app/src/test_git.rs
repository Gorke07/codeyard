//! Hermetic git for tests: a fixed identity and no user or system configuration.
//!
//! The app's own `git` calls run through `Host::exec` and inherit this process's environment, so the settings are
//! process-wide (set once, before the first git command of any test) instead of per command. Without them a runner
//! with no resolvable identity (typical CI container) refuses every commit, and a developer's global config
//! (`commit.gpgsign`, hooks, aliases) leaks into the tests.

use std::process::Command;
use std::sync::Once;

const ENV: &[(&str, &str)] = &[
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_AUTHOR_NAME", "Test"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "Test"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
];

/// Applies the environment once; every test helper that runs git calls this first.
pub(crate) fn hermetic() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for (k, v) in ENV {
            // SAFETY: runs before this process's first git child; later calls only wait on the `Once`.
            unsafe { std::env::set_var(k, v) };
        }
    });
}

/// `git` with the hermetic environment and `main` as the default branch.
pub(crate) fn git() -> Command {
    hermetic();
    let mut c = Command::new("git");
    c.args(["-c", "init.defaultBranch=main"]);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_work_without_any_configured_identity() {
        let dir = std::env::temp_dir().join(format!("codeyard-test-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            // `useConfigOnly` makes git fail like a bare CI runner does, even where it could guess a name.
            let o = git().args(["-c", "user.useConfigOnly=true"]).args(args).current_dir(&dir).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).into_owned()
        };
        run(&["init", "-q"]);
        run(&["commit", "-q", "--allow-empty", "-m", "x"]);
        assert_eq!(run(&["log", "-1", "--format=%an <%ae>|%cn <%ce>"]).trim(), "Test <t@example.com>|Test <t@example.com>");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
