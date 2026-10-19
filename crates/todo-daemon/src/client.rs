//! Dialing the daemon socket (design 4.2: gRPC over UDS, `:authority` localhost).

use std::io;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Error};
use tower::service_fn;

/// Connects a tonic channel to the daemon socket at `socket`. Every dial,
/// including reconnects, first checks the socket's directory ([`dial`]).
pub async fn connect(socket: &Path) -> Result<Channel, Error> {
    let socket: PathBuf = socket.to_owned();
    Endpoint::from_static("http://localhost")
        .connect_with_connector(service_fn(move |_| {
            let socket = socket.clone();
            async move { dial(&socket).await.map(TokioIo::new) }
        }))
        .await
}

/// Connects to `socket` after [`check_runtime_dir`] on its directory, so we
/// never talk to a socket another user could have planted.
pub async fn dial(socket: &Path) -> io::Result<UnixStream> {
    check_runtime_dir(socket.parent().unwrap_or(Path::new("/")))?;
    UnixStream::connect(socket).await
}

/// True when `err` means no daemon is listening (missing or stale socket).
pub fn is_not_running(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// The directories that keep other users away from the socket: `runtime`
/// itself and, under the D8 fallback `/tmp/todo-tui-$UID/h-<sha8>`, its parent.
pub fn guarded_dirs(runtime: &Path) -> Vec<&Path> {
    let fallback = PathBuf::from(format!("/tmp/todo-tui-{}", uid()));
    let mut dirs = vec![runtime];
    if let Some(parent) = runtime.parent()
        && parent == fallback
    {
        dirs.push(parent);
    }
    dirs
}

/// Checks that each of [`guarded_dirs`] is a real directory (not a symlink)
/// owned by us with no group or other permission bits. A missing directory
/// is `NotFound` (no daemon yet); anything else unsafe is `PermissionDenied`.
pub fn check_runtime_dir(runtime: &Path) -> io::Result<()> {
    for dir in guarded_dirs(runtime) {
        let meta = owned_dir_meta(dir)?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(unsafe_dir(
                dir,
                &format!(
                    "is accessible by other users (mode {:o})",
                    meta.mode() & 0o777
                ),
            ));
        }
    }
    Ok(())
}

/// `lstat`s `dir` and checks it is a directory we own.
pub(crate) fn owned_dir_meta(dir: &Path) -> io::Result<std::fs::Metadata> {
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(unsafe_dir(dir, "is not a directory"));
    }
    if meta.uid() != uid() {
        return Err(unsafe_dir(dir, "is owned by another user"));
    }
    Ok(meta)
}

fn unsafe_dir(dir: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("refusing to use {}: it {why}", dir.display()),
    )
}

fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}
