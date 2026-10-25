//! Throwaway `sshd` for tests: on this machine, as the current user, on 127.0.0.1.
//! The `codeyard-e2e` alias is in a separate ssh config; the agent is uploaded to a temp dir.
//! Drop shuts down the ssh master, the remote daemon and sshd.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub const ALIAS: &str = "codeyard-e2e";

pub struct Sshd {
    pub dir: PathBuf,
    pub port: u16,
    child: Child,
    /// Environment variables are process-wide: SSH tests run sequentially.
    _turn: std::sync::MutexGuard<'static, ()>,
}

static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn keygen(path: &Path) -> bool {
    Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(path).status().is_ok_and(|s| s.success())
}

/// `None`: no sshd or agent binary (the test is skipped).
pub fn start(name: &str) -> Option<Sshd> {
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let sshd = Path::new("/usr/sbin/sshd");
    // target/<profile>/deps/codeyard-… → target/<profile>/codeyard-agent
    let agents = std::env::current_exe().ok()?.parent()?.parent()?.to_path_buf();
    if !sshd.exists() || !agents.join("codeyard-agent").is_file() {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("codeyard-sshd-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    let dir = std::fs::canonicalize(&dir).ok()?;
    if !keygen(&dir.join("host_key")) || !keygen(&dir.join("user_key")) {
        return None;
    }
    std::fs::copy(dir.join("user_key.pub"), dir.join("authorized_keys")).ok()?;
    let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
    let d = dir.display();
    std::fs::write(
        dir.join("sshd_config"),
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_key\nAuthorizedKeysFile {d}/authorized_keys\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\nPidFile {d}/sshd.pid\nAllowTcpForwarding yes\n"
        ),
    )
    .ok()?;
    let user = std::env::var("USER").ok()?;
    std::fs::write(
        dir.join("ssh_config"),
        format!(
            "Host {ALIAS}\n  HostName 127.0.0.1\n  Port {port}\n  User {user}\n  IdentityFile {d}/user_key\n  IdentitiesOnly yes\n\
               IdentityAgent none\n  UserKnownHostsFile {d}/known_hosts\n  StrictHostKeyChecking accept-new\n"
        ),
    )
    .ok()?;
    let child = Command::new(sshd).arg("-D").arg("-f").arg(dir.join("sshd_config")).stderr(Stdio::null()).spawn().ok()?;
    let start = std::time::Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        if start.elapsed().as_secs() > 5 {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // SAFETY: only ssh commands read it; there is only one sshd test at a time.
    unsafe {
        std::env::set_var("CODEYARD_SSH_CONFIG", dir.join("ssh_config"));
        std::env::set_var("CODEYARD_REMOTE_DIR", dir.join("remote"));
        std::env::set_var("CODEYARD_AGENT_DIR", &agents);
    }
    Some(Sshd { dir, port, child, _turn: turn })
}

impl Sshd {
    /// Kill the server (cuts the connection and new ones; also closes the remote agent's bridge).
    pub fn stop_server(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new("pkill").arg("-f").arg(format!("{}/remote/agent.*--attach", self.dir.display())).status();
        host::ssh::close_master(ALIAS);
    }

    /// Second alias to the same server without the key (to provoke an auth failure).
    pub fn add_alias_without_key(&self, alias: &str) {
        use std::io::Write;
        let user = std::env::var("USER").unwrap_or_default();
        let mut f = std::fs::OpenOptions::new().append(true).open(self.dir.join("ssh_config")).expect("ssh_config");
        let _ = write!(
            f,
            "\nHost {alias}\n  HostName 127.0.0.1\n  Port {}\n  User {user}\n  IdentityFile /nonexistent/key\n  IdentitiesOnly yes\n  IdentityAgent none\n  \
             PasswordAuthentication no\n  BatchMode yes\n  UserKnownHostsFile {}/known_hosts\n  StrictHostKeyChecking accept-new\n",
            self.port,
            self.dir.display()
        );
    }
}

impl Drop for Sshd {
    fn drop(&mut self) {
        host::ssh::close_master(ALIAS);
        let _ = Command::new("pkill").arg("-f").arg(format!("{}/remote/agent", self.dir.display())).status();
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Fake `docker` (put on PATH): `docker exec -i NAME sh -c SCRIPT` → runs the script on this machine.
/// Tests the container flow (agent upload, `--attach`, remote files) without real docker.
pub struct FakeDocker {
    dir: PathBuf,
    old_path: Option<std::ffi::OsString>,
    _turn: std::sync::MutexGuard<'static, ()>,
}

pub fn start_fake_docker(name: &str) -> Option<FakeDocker> {
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let agents = std::env::current_exe().ok()?.parent()?.parent()?.to_path_buf();
    if !agents.join("codeyard-agent").is_file() {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("codeyard-fakedocker-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("bin")).ok()?;
    let dir = std::fs::canonicalize(&dir).ok()?;
    let shim = dir.join("bin/docker");
    std::fs::write(&shim, "#!/bin/sh\nif [ \"$1\" = exec ]; then shift; shift; shift; exec \"$@\"; fi\nif [ \"$1\" = ps ]; then printf 'fake-box\\tdebian:12\\n'; exit 0; fi\nexit 1\n").ok()?;
    {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        #[cfg(unix)]
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).ok()?;
    }
    let old_path = std::env::var_os("PATH");
    let new_path = format!("{}:{}", dir.join("bin").display(), old_path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
    // SAFETY: one remote test at a time (TURN).
    unsafe {
        std::env::set_var("PATH", new_path);
        std::env::set_var("CODEYARD_REMOTE_DIR", dir.join("remote"));
        std::env::set_var("CODEYARD_AGENT_DIR", &agents);
    }
    Some(FakeDocker { dir, old_path, _turn: turn })
}

impl Drop for FakeDocker {
    fn drop(&mut self) {
        let _ = Command::new("pkill").arg("-f").arg(format!("{}/remote/agent", self.dir.display())).status();
        // SAFETY: see start_fake_docker.
        unsafe {
            match &self.old_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
