//! Auto-spawn (design 4.2): find a listening daemon or start one.

use std::fs::OpenOptions;
use std::os::fd::{BorrowedFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, bail};
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
use rustix::process::{Pid, test_kill_process};
use todo_daemon::client::{connect, dial, is_not_running};
use todo_daemon::paths::{Env, Paths};
use tokio::time::{Instant, sleep};
use tonic::transport::Channel;

/// Total time to wait for a freshly spawned daemon to listen.
const SPAWN_BUDGET: Duration = Duration::from_secs(2);
/// Time `todo daemon stop` waits for the daemon to release its lock.
const STOP_BUDGET: Duration = Duration::from_secs(15);
const FIRST_DELAY: Duration = Duration::from_millis(20);
const MAX_DELAY: Duration = Duration::from_millis(200);

pub(crate) async fn is_listening(socket: &Path) -> bool {
    dial(socket).await.is_ok()
}

/// Connects to the daemon, spawning one if nothing is listening.
pub(crate) async fn connect_or_spawn(paths: &Paths) -> anyhow::Result<Channel> {
    match dial(&paths.socket).await {
        Ok(_) => {}
        Err(e) if is_not_running(&e) => spawn_and_wait(paths).await?,
        Err(e) => {
            return Err(e).with_context(|| format!("connecting to {}", paths.socket.display()));
        }
    }
    connect(&paths.socket)
        .await
        .with_context(|| format!("connecting to {}", paths.socket.display()))
}

/// Starts `todo daemon` detached and waits (20ms doubling to 200ms, up to
/// ~2s) until the socket accepts connections.
pub(crate) async fn spawn_and_wait(paths: &Paths) -> anyhow::Result<()> {
    let mut child = spawn_detached(paths)?;
    let listening = wait_until_listening(paths, &mut child).await;
    // Reap the daemon whenever it exits. While this process runs on (`todo
    // add` at a terminal), it would otherwise stay a zombie, which
    // `wait_for_exit` cannot tell from a running daemon.
    std::thread::spawn(move || child.wait());
    listening
}

/// Polls the socket until it accepts connections, or `child` fails, or
/// [`SPAWN_BUDGET`] runs out.
async fn wait_until_listening(paths: &Paths, child: &mut Child) -> anyhow::Result<()> {
    let log = paths.daemon_log();
    let deadline = Instant::now() + SPAWN_BUDGET;
    let mut delay = FIRST_DELAY;
    loop {
        if is_listening(&paths.socket).await {
            return Ok(());
        }
        // Exit status 0 means another daemon won the lock; keep waiting for it.
        if let Some(status) = child.try_wait()?
            && !status.success()
        {
            bail!("daemon exited with {status}; see {}", log.display());
        }
        if Instant::now() >= deadline {
            bail!(
                "daemon did not start listening on {} within {SPAWN_BUDGET:?}; see {}",
                paths.socket.display(),
                log.display()
            );
        }
        sleep(delay).await;
        delay = (delay * 2).min(MAX_DELAY);
    }
}

/// Runs `<current exe> daemon` in its own process group with stdio going to
/// the daemon log, so it outlives this process and its terminal.
fn spawn_detached(paths: &Paths) -> anyhow::Result<Child> {
    std::fs::create_dir_all(&paths.logs)
        .with_context(|| format!("creating {}", paths.logs.display()))?;
    let log_path = paths.daemon_log();
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    cloexec_inherited_fds();
    let exe = std::env::current_exe().context("locating the todo binary")?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0);
    // The daemon runs from `/`, so pass a relative TODO_TUI_HOME as absolute.
    if let Some(home) = Env::from_process().todo_tui_home {
        cmd.env("TODO_TUI_HOME", home);
    }
    cmd.spawn().context("spawning the daemon")
}

/// Marks every descriptor above stderr close-on-exec so the daemon never
/// inherits one from our parent. Otherwise a parent's pipe (a caller's
/// `$(...)`, or a sibling's output pipe leaked by a racing fork) stays open
/// for the daemon's lifetime and the reader waits for EOF forever.
#[allow(unsafe_code)]
fn cloexec_inherited_fds() {
    let Ok(dir) = std::fs::read_dir("/dev/fd") else {
        return;
    };
    let fds: Vec<RawFd> = dir
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .filter(|&fd| fd > 2)
        .collect();
    for fd in fds {
        // SAFETY: only descriptor flags change; a number that is no longer
        // open (such as the directory handle above) makes fcntl fail.
        let fd = unsafe { BorrowedFd::borrow_raw(fd) };
        if let Ok(flags) = fcntl_getfd(fd) {
            let _ = fcntl_setfd(fd, flags | FdFlags::CLOEXEC);
        }
    }
}

/// The pid the running daemon recorded in `data/daemon.lock`.
pub(crate) fn daemon_pid(paths: &Paths) -> Option<Pid> {
    let text = std::fs::read_to_string(&paths.lock).ok()?;
    Pid::from_raw(text.trim().parse().ok()?)
}

/// Waits for the daemon process `pid` to exit; it releases the lock as it
/// does. This never takes the lock to probe it: holding it even for a moment
/// would make a daemon starting meanwhile believe another one is running.
pub(crate) async fn wait_for_exit(pid: Pid) -> anyhow::Result<()> {
    let deadline = Instant::now() + STOP_BUDGET;
    let mut delay = FIRST_DELAY;
    // EPERM means the pid now belongs to another user, so ours is gone.
    while test_kill_process(pid).is_ok() {
        if Instant::now() >= deadline {
            bail!("daemon (pid {pid}) still running after {STOP_BUDGET:?}");
        }
        sleep(delay).await;
        delay = (delay * 2).min(MAX_DELAY);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_for_exit_follows_the_process_not_the_lock() {
        let mut child = Command::new("sleep").arg("0.2").spawn().unwrap();
        let pid = Pid::from_child(&child);
        let reaper = std::thread::spawn(move || child.wait().unwrap());
        let start = Instant::now();
        wait_for_exit(pid).await.unwrap();
        assert!(start.elapsed() >= Duration::from_millis(150));
        assert!(reaper.join().unwrap().success());
    }
}
