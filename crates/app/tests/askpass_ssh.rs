//! Real `ssh` + real `sshd` (127.0.0.1 only, with user privileges,
//! in a temp dir) through the askpass bridge: the unknown host key confirmation and the
//! key passphrase are asked via the `codeyard` binary. Skipped if there is no sshd.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use host::askpass::{AskKind, AskRequest};

fn keygen(path: &Path, passphrase: &str) {
    let ok = Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", passphrase, "-f"]).arg(path).status().unwrap().success();
    assert!(ok);
}

#[test]
fn ssh_asks_host_key_and_passphrase_through_app() {
    let sshd = Path::new("/usr/sbin/sshd");
    if !sshd.exists() {
        assert!(std::env::var_os("CODEYARD_REQUIRE_SSHD").is_none(), "no /usr/sbin/sshd and CODEYARD_REQUIRE_SSHD is set");
        eprintln!("SKIPPED: no /usr/sbin/sshd (set CODEYARD_REQUIRE_SSHD=1 to make this an error)");
        return;
    }
    let dir = std::env::temp_dir().join(format!("codeyard-sshd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    keygen(&dir.join("host_key"), "");
    keygen(&dir.join("user_key"), "parola 123");
    std::fs::copy(dir.join("user_key.pub"), dir.join("authorized_keys")).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let d = dir.display();
    std::fs::write(
        dir.join("sshd_config"),
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_key\nAuthorizedKeysFile {d}/authorized_keys\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\nPidFile {d}/sshd.pid\n"
        ),
    )
    .unwrap();
    let mut server = Command::new(sshd).arg("-D").arg("-f").arg(dir.join("sshd_config")).stderr(Stdio::null()).spawn().unwrap();
    // Let it start listening.
    let start = std::time::Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(start.elapsed().as_secs() < 5, "sshd did not start");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let seen = Arc::new(Mutex::new(Vec::<AskRequest>::new()));
    let log = seen.clone();
    let socket = host::askpass::serve(Arc::new(move |req: AskRequest| {
        log.lock().unwrap().push(req.clone());
        Some(match req.kind {
            AskKind::HostKey => "yes".into(),
            _ => "parola 123".into(),
        })
    }))
    .unwrap();

    let mut ssh = Command::new("ssh");
    host::askpass::configure_with(&mut ssh, &socket, Path::new(env!("CARGO_BIN_EXE_codeyard")));
    let out = ssh
        .args(["-F", "/dev/null", "-o", "IdentitiesOnly=yes", "-o", "IdentityAgent=none", "-o", "ConnectTimeout=5"])
        .arg("-o")
        .arg(format!("UserKnownHostsFile={d}/known_hosts"))
        .arg("-i")
        .arg(dir.join("user_key"))
        .args(["-p", &port.to_string(), "127.0.0.1", "echo", "baglandi"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = server.kill();
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "baglandi", "ssh stderr: {}", String::from_utf8_lossy(&out.stderr));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.iter().map(|r| r.kind).collect::<Vec<_>>(), [AskKind::HostKey, AskKind::Secret], "{seen:?}");
    assert!(seen[0].prompt.contains("fingerprint"), "{}", seen[0].prompt);
    assert!(seen[1].prompt.contains("passphrase"), "{}", seen[1].prompt);
}
