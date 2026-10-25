//! Pseudo-terminal: `openpty` + shell. No dependencies (libc), to keep the agent small.

#[cfg(unix)]
use std::fs::File;
use std::io::{self, Write};
#[cfg(unix)]
use std::process::Child;

#[cfg(windows)]
use crate::Locked;

#[cfg(unix)]
pub struct Pty {
    master: File,
    child: Child,
    /// Keyboard input is written on a separate thread: a full terminal (e.g. a big paste into a stopped shell) must not block the dispatch
    /// thread and the `ptys` lock.
    input: std::sync::mpsc::SyncSender<Vec<u8>>,
}

#[cfg(windows)]
pub struct Pty {
    master: std::sync::Arc<std::sync::Mutex<Option<Box<dyn portable_pty::MasterPty + Send>>>>,
    /// Keyboard input is written on a separate thread (see unix `Pty::input`).
    input: std::sync::mpsc::SyncSender<Vec<u8>>,
    child: std::sync::Arc<std::sync::Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
}

#[cfg(windows)]
mod windows {
    use super::*;
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    fn size(cols: u16, rows: u16) -> PtySize {
        PtySize { cols: cols.max(1), rows: rows.max(1), pixel_width: 0, pixel_height: 0 }
    }

    pub fn spawn(cols: u16, rows: u16, cwd: Option<&str>, shell: Option<&str>) -> io::Result<(Pty, Box<dyn io::Read + Send>)> {
        let pair = native_pty_system().openpty(size(cols, rows)).map_err(io::Error::other)?;
        let shell = shell.filter(|s| !s.is_empty()).map(std::ffi::OsString::from).or_else(|| std::env::var_os("COMSPEC")).unwrap_or_else(|| "cmd.exe".into());
        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "codeyard");
        if let Some(dir) = cwd.filter(|d| std::path::Path::new(d).is_dir()) {
            cmd.cwd(dir);
        }
        // Open the streams before spawning, so a setup failure cannot orphan a shell.
        let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let mut writer = pair.master.take_writer().map_err(io::Error::other)?;
        let child = std::sync::Arc::new(std::sync::Mutex::new(pair.slave.spawn_command(cmd).map_err(io::Error::other)?));
        drop(pair.slave);
        let master = std::sync::Arc::new(std::sync::Mutex::new(Some(pair.master)));
        let (monitor_child, monitor_master) = (child.clone(), master.clone());
        std::thread::spawn(move || {
            loop {
                if !matches!(monitor_child.locked().try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            // ConPTY keeps its output pipe open until the pseudo-console closes,
            // even after the shell exits. Close it while the output thread drains.
            let master = monitor_master.locked().take();
            drop(master);
        });
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(512);
        std::thread::spawn(move || {
            while let Ok(chunk) = rx.recv() {
                if writer.write_all(&chunk).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        });
        Ok((Pty { master, input: tx, child }, reader))
    }

    pub fn resize(pty: &Pty, cols: u16, rows: u16) -> io::Result<()> {
        let master = pty.master.locked();
        master.as_ref().ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?.resize(size(cols, rows)).map_err(io::Error::other)
    }
}

#[cfg(windows)]
pub use windows::{resize, spawn};

#[cfg(unix)]
mod imp {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    use super::*;

    fn winsize(cols: u16, rows: u16) -> libc::winsize {
        libc::winsize { ws_row: rows.max(1), ws_col: cols.max(1), ws_xpixel: 0, ws_ypixel: 0 }
    }

    /// Start the shell; the returned `File` is for reading output (a copy of the master).
    pub fn spawn(cols: u16, rows: u16, cwd: Option<&str>, shell: Option<&str>) -> io::Result<(Pty, File)> {
        let (mut master, mut slave) = (0, 0);
        let mut ws = winsize(cols, rows);
        // SAFETY: `master`, `slave` and `ws` are valid for the call. Null name and termios mean "don't return a name" and "default
        // termios". `&mut ws` coerces to whichever pointer mutability this platform's libc declares, so no cast is needed.
        let rc = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openpty succeeded, so both are open fds that nothing else owns; ownership moves to the OwnedFds.
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // The shell must not inherit the master (other children spawned meanwhile would keep the pty open). The slave ends are
        // dup2'd onto 0-2 in the child, which clears the flag on the copies.
        for fd in [&master, &slave] {
            // SAFETY: the fd is open (owned by `fd`); F_SETFD only changes the descriptor flags.
            unsafe { libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(fd), libc::F_SETFD, libc::FD_CLOEXEC) };
        }

        let shell = shell.map(str::to_string).or_else(|| std::env::var("SHELL").ok()).unwrap_or_else(|| "/bin/sh".into());
        let mut cmd = Command::new(&shell);
        cmd.arg("-l")
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "codeyard")
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        if let Some(dir) = cwd.filter(|d| std::path::Path::new(d).is_dir()) {
            cmd.current_dir(dir);
        }
        // SAFETY: the closure runs between fork and exec, so it may only make async-signal-safe calls: setsid and ioctl are, and
        // `last_os_error` reads errno without allocating. It captures nothing.
        unsafe {
            cmd.pre_exec(|| {
                // New session + this PTY as controlling terminal: so Ctrl+C and job control work.
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        let master = File::from(master);
        let reader = master.try_clone()?;
        let mut writer = master.try_clone()?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(512);
        std::thread::spawn(move || {
            while let Ok(chunk) = rx.recv() {
                if writer.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
        Ok((Pty { master, child, input: tx }, reader))
    }

    pub fn resize(pty: &Pty, cols: u16, rows: u16) -> io::Result<()> {
        let ws = winsize(cols, rows);
        let fd = std::os::fd::AsRawFd::as_raw_fd(&pty.master);
        // SAFETY: `fd` is the open master, and `&ws` points to a live winsize, which is what TIOCSWINSZ reads.
        if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &ws) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(unix)]
pub use imp::{resize, spawn};

impl Pty {
    /// Enqueue (doesn't block); `WouldBlock` if the queue (512 chunks) is full.
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        use std::sync::mpsc::TrySendError;
        self.input.try_send(data.to_vec()).map_err(|e| match e {
            TrySendError::Full(_) => io::Error::new(io::ErrorKind::WouldBlock, "the terminal is not reading its input"),
            TrySendError::Disconnected(_) => io::ErrorKind::BrokenPipe.into(),
        })
    }

    pub fn kill(&mut self) {
        #[cfg(unix)]
        let _ = self.child.kill();
        #[cfg(windows)]
        let _ = self.child.locked().kill();
    }

    /// Exit code (called after the read end closes).
    pub fn wait(&mut self) -> Option<i32> {
        #[cfg(unix)]
        {
            self.child.wait().ok().and_then(|s| s.code())
        }
        #[cfg(windows)]
        {
            self.child.locked().wait().ok().map(|s| s.exit_code() as i32)
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Without waiting, every closed session would leave a zombie shell.
        self.kill();
        #[cfg(unix)]
        let _ = self.child.wait();
        #[cfg(windows)]
        let _ = self.child.locked().wait();
    }
}
