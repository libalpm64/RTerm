//! Local Unix IPC. The socket directory is private to the effective UID.
//! An exclusive file lock prevents a second RTerm from replacing the listener.
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::{UnixListener, UnixStream};

pub use crate::agent_protocol::{MAX_REQUEST, MAX_RESPONSE, read_frame, write_frame};

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

// A short path avoids sockaddr_un length limits, including on macOS.
// /tmp is a system-managed sticky directory; never use a caller-supplied TMPDIR.
pub fn pipe_name() -> io::Result<String> {
    Ok(format!("/tmp/rterm-agent-{}/agent.sock", uid()))
}

fn private_parent(path: &Path, create: bool) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| denied("No IPC directory"))?;
    if create {
        match fs::DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    let m = fs::symlink_metadata(parent)?;
    if !m.is_dir() || m.uid() != uid() || m.mode() & 0o077 != 0 {
        return Err(denied("Unsafe RTerm IPC directory"));
    }
    Ok(())
}

fn check_socket(path: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(path)?;
    if !m.file_type().is_socket() || m.uid() != uid() {
        return Err(denied("Unexpected RTerm IPC socket"));
    }
    Ok(())
}

pub struct Listener {
    socket: UnixListener,
    path: PathBuf,
    _lock: File,
}

impl Listener {
    pub async fn accept(&self) -> io::Result<UnixStream> {
        loop {
            let (stream, _) = self.socket.accept().await?;
            if stream.peer_cred().is_ok_and(|cred| cred.uid() == uid()) {
                return Ok(stream);
            }
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // Still holding the lock; a replacement listener cannot exist yet.
        let _ = fs::remove_file(&self.path);
    }
}

pub fn listen(name: &str) -> io::Result<Listener> {
    let path = PathBuf::from(name);
    private_parent(&path, true)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(format!("{name}.lock"))?;
    let meta = lock.metadata()?;
    if !meta.is_file() || meta.uid() != uid() || meta.nlink() != 1 || meta.mode() & 0o077 != 0 {
        return Err(denied("Unsafe RTerm IPC lock"));
    }
    lock.try_lock().map_err(|e| {
        io::Error::other(format!("RTerm IPC already active or lock unavailable: {e}"))
    })?;
    match check_socket(&path) {
        Ok(()) => fs::remove_file(&path)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let socket = UnixListener::bind(&path)?;
    Ok(Listener {
        socket,
        path,
        _lock: lock,
    })
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn peer_executable(pid: i32) -> io::Result<PathBuf> {
    fs::read_link(format!("/proc/{pid}/exe"))
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
fn peer_executable(pid: i32) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = [0u8; 4096];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return Err(io::Error::last_os_error());
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..end])))
}

// The client half is used by the companion binary and IPC tests, not the GUI.
#[allow(dead_code)]
pub async fn request(
    name: &str,
    expected: &Path,
    body: &[u8],
    wait: Duration,
) -> io::Result<Vec<u8>> {
    let path = Path::new(name);
    private_parent(path, false)?;
    check_socket(path)?;
    let mut stream =
        tokio::time::timeout(Duration::from_secs(3), UnixStream::connect(path)).await??;
    let cred = stream.peer_cred()?;
    if cred.uid() != uid() {
        return Err(denied("Unexpected RTerm IPC peer user"));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let pid = cred
            .pid()
            .ok_or_else(|| denied("No RTerm peer process ID"))?;
        if fs::canonicalize(peer_executable(pid)?)? != fs::canonicalize(expected)? {
            return Err(denied("Unexpected RTerm IPC server executable"));
        }
    }
    tokio::time::timeout(
        Duration::from_secs(3),
        write_frame(&mut stream, body, MAX_REQUEST),
    )
    .await??;
    tokio::time::timeout(wait, read_frame(&mut stream, MAX_RESPONSE)).await?
}

#[cfg(test)]
#[path = "../tests/unit/agent_ipc_unix.rs"]
mod tests;
