//! SSH_ASKPASS bridge: password, passphrase, 2FA and host-key prompts from `ssh` are asked in
//! the app window.
//!
//! `ssh` → `SSH_ASKPASS` (the app's own binary, client mode) → Unix socket (0700 dir) →
//! server in the app → user → reply back.
//! The reply never touches disk or an env variable.

// No bridge on Windows (no Unix socket): `serve`/`request` fail, `configure` returns `false` and ssh runs
// with `BatchMode=yes`, so only key / ssh-agent logins work and no password dialog appears.
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

/// Env variable that switches on client mode (set for ssh only).
pub const SOCK_ENV: &str = "CODEYARD_ASKPASS_SOCK";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskKind {
    /// Password, key passphrase, PIN, one-time code: masked input.
    Secret,
    /// Unknown host key: the answer is "yes" or "no".
    HostKey,
    /// Confirmation only (`SSH_ASKPASS_PROMPT=confirm`, e.g. ssh-agent).
    Confirm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRequest {
    pub kind: AskKind,
    pub prompt: String,
}

/// `None`: the user cancelled (ssh treats it as an auth failure).
pub type Handler = Arc<dyn Fn(AskRequest) -> Option<String> + Send + Sync>;

static SOCKET: OnceLock<PathBuf> = OnceLock::new();

pub fn classify(confirm_only: bool, prompt: &str) -> AskKind {
    if prompt.contains("(yes/no") {
        AskKind::HostKey
    } else if confirm_only {
        AskKind::Confirm
    } else {
        AskKind::Secret
    }
}

/// Start the server and register it process-wide (once). Later `ssh` calls use it
/// instead of BatchMode.
pub fn install(handler: Handler) -> std::io::Result<&'static Path> {
    if let Some(p) = SOCKET.get() {
        return Ok(p);
    }
    let path = serve(handler)?;
    Ok(SOCKET.get_or_init(|| path))
}

pub fn installed() -> Option<&'static Path> {
    SOCKET.get().map(PathBuf::as_path)
}

#[cfg(not(unix))]
pub fn serve(_: Handler) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "askpass bridge is not available on this platform"))
}

/// Listen on a private socket directory; one thread per connection.
#[cfg(unix)]
pub fn serve(handler: Handler) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("codeyard-askpass-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    // Owner only, regardless of umask.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join("s");
    let listener = UnixListener::bind(&path)?;
    std::thread::Builder::new().name("askpass".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let handler = handler.clone();
            let _ = std::thread::Builder::new().name("askpass-req".into()).spawn(move || answer(stream, &*handler));
        }
    })?;
    Ok(path)
}

/// Wire format: client sends `ask\n` or `confirm\n` + prompt and closes its write side;
/// server answers `ok\n<reply>` or `cancel\n`.
#[cfg(unix)]
fn answer(mut stream: UnixStream, handler: &(dyn Fn(AskRequest) -> Option<String> + Send + Sync)) {
    let mut buf = Vec::new();
    if (&mut stream).take(64 * 1024).read_to_end(&mut buf).is_err() {
        return;
    }
    let text = String::from_utf8_lossy(&buf);
    let (head, prompt) = text.split_once('\n').unwrap_or(("ask", &text));
    let request = AskRequest { kind: classify(head == "confirm", prompt), prompt: prompt.trim_end().to_string() };
    let reply = match handler(request) {
        Some(answer) => format!("ok\n{answer}"),
        None => "cancel\n".to_string(),
    };
    let _ = stream.write_all(reply.as_bytes());
}

#[cfg(not(unix))]
pub fn request(_: &Path, _: bool, _: &str) -> std::io::Result<Option<String>> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "askpass bridge is not available on this platform"))
}

/// Client side: send the question, wait for the reply.
#[cfg(unix)]
pub fn request(socket: &Path, confirm_only: bool, prompt: &str) -> std::io::Result<Option<String>> {
    let mut stream = UnixStream::connect(socket)?;
    let head = if confirm_only { "confirm" } else { "ask" };
    stream.write_all(format!("{head}\n{prompt}").as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply.strip_prefix("ok\n").map(str::to_string))
}

/// If ssh invoked this binary as askpass, ask the question and return the exit code;
/// otherwise `None` (normal start).
pub fn client_main() -> Option<i32> {
    let socket = std::env::var_os(SOCK_ENV)?;
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let confirm_only = std::env::var("SSH_ASKPASS_PROMPT").is_ok_and(|v| v == "confirm");
    Some(match request(Path::new(&socket), confirm_only, &prompt) {
        Ok(Some(answer)) => {
            println!("{answer}");
            0
        }
        Ok(None) => 1,
        Err(e) => {
            eprintln!("codeyard askpass: {e}");
            1
        }
    })
}

/// Wire the ssh command to the bridge; `false` if none is installed (caller uses BatchMode).
pub fn configure(cmd: &mut Command) -> bool {
    // `CODEYARD_ASKPASS_EXE`: binary to run as the client (tests point it at the app's binary; normally itself).
    let exe = std::env::var_os("CODEYARD_ASKPASS_EXE").map(PathBuf::from).or_else(|| std::env::current_exe().ok());
    let (Some(socket), Some(exe)) = (installed(), exe) else { return false };
    configure_with(cmd, socket, &exe);
    true
}

/// `exe`: binary that runs as the askpass client (normally the app itself).
pub fn configure_with(cmd: &mut Command, socket: &Path, exe: &Path) {
    cmd.env("SSH_ASKPASS", exe).env("SSH_ASKPASS_REQUIRE", "force").env(SOCK_ENV, socket);
    // OpenSSH < 8.4 only uses askpass when DISPLAY is set.
    if std::env::var_os("DISPLAY").is_none() {
        cmd.env("DISPLAY", ":0");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::sync::Mutex;

    #[test]
    fn classifies_prompts() {
        assert_eq!(classify(false, "deploy@10.0.0.5's password: "), AskKind::Secret);
        assert_eq!(classify(false, "Enter passphrase for key '/home/a/.ssh/id_ed25519': "), AskKind::Secret);
        assert_eq!(
            classify(false, "The authenticity of host 'x' can't be established.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? "),
            AskKind::HostKey
        );
        assert_eq!(classify(true, "Allow use of key?"), AskKind::Confirm);
    }

    #[cfg(unix)]
    #[test]
    fn round_trips_over_socket() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let socket = serve(Arc::new(move |req: AskRequest| {
            log.lock().unwrap().push(req.clone());
            match req.kind {
                AskKind::Secret => Some("gizli şifre".into()),
                AskKind::HostKey => Some("yes".into()),
                AskKind::Confirm => None,
            }
        }))
        .unwrap();
        assert_eq!(request(&socket, false, "Password: ").unwrap().as_deref(), Some("gizli şifre"));
        assert_eq!(request(&socket, false, "continue connecting (yes/no)? ").unwrap().as_deref(), Some("yes"));
        assert_eq!(request(&socket, true, "Allow?").unwrap(), None, "cancelled");
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0], AskRequest { kind: AskKind::Secret, prompt: "Password:".into() });
        assert_eq!(seen[2].kind, AskKind::Confirm);

        // Socket dir is owner-only.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(socket.parent().unwrap()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}
