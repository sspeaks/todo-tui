//! End-to-end tests of the `todo` binary on temp `TODO_TUI_HOME`s.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use todo_core::MAX_CAPTURE_BYTES;
use todo_daemon::paths::{Env, Paths};
use todo_daemon::spool::SpoolEntry;
use todo_proto::google::r#type::Date;
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::{
    CreatedVia, GetItemRequest, Item, ListItemsRequest, PersonRef, Priority, SetStatusRequest,
    Status, person_ref,
};
use tonic::transport::Channel;

/// A temp home whose daemon is stopped on drop, even if the test fails.
struct Home {
    _dir: TempDir,
    home: PathBuf,
}

impl Home {
    fn new() -> Home {
        let dir = TempDir::new().unwrap();
        let home = dir.path().to_owned();
        Home { _dir: dir, home }
    }

    /// A home deep enough that its socket path exceeds 100 bytes (D8).
    fn long() -> Home {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("x".repeat(100));
        Home { _dir: dir, home }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_todo"));
        cmd.args(args).env("TODO_TUI_HOME", &self.home);
        cmd
    }

    fn todo(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// Runs `todo` with `input` on its stdin.
    fn todo_with_stdin(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let input = input.to_owned();
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let out = child.wait_with_output().unwrap();
        writer.join().unwrap().unwrap();
        out
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.todo(args);
        assert!(
            out.status.success(),
            "todo {args:?} failed: {}\nlog:\n{}",
            String::from_utf8_lossy(&out.stderr),
            std::fs::read_to_string(self.home.join("logs/daemon.log")).unwrap_or_default()
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn paths(&self) -> Paths {
        let env = Env {
            todo_tui_home: Some(self.home.clone()),
            ..Env::from_process()
        };
        Paths::resolve(&env).unwrap()
    }

    /// Runs `rpc` with a client of the running daemon.
    fn rpc<T>(&self, rpc: impl AsyncFnOnce(ItemServiceClient<Channel>) -> T) -> T {
        let socket = self.paths().socket;
        block_on(async {
            let channel = todo_daemon::client::connect(&socket).await.unwrap();
            rpc(ItemServiceClient::new(channel)).await
        })
    }

    /// Every Item the default filter shows (no deleted ones), oldest first,
    /// read from the running daemon over gRPC.
    fn items(&self) -> Vec<Item> {
        self.rpc(async |mut items| {
            let request = ListItemsRequest {
                page_size: 1000,
                ..Default::default()
            };
            items.list_items(request).await.unwrap().into_inner().items
        })
    }

    /// The Item `id`, deleted or not, from the running daemon.
    fn item(&self, id: &str) -> Item {
        self.rpc(async |mut items| {
            let request = GetItemRequest { id: id.to_owned() };
            let item = items.get_item(request).await.unwrap().into_inner().item;
            item.unwrap()
        })
    }
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.todo(&["daemon", "stop"]);
    }
}

fn socket_in(dir: &Path) -> bool {
    dir.join("daemon.sock").exists()
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

fn titles(items: &[Item]) -> Vec<&str> {
    items.iter().map(|i| i.title.as_str()).collect()
}

/// Each `<short id>  <rest>` line of `add` output, without its id.
fn captured(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .map(|line| {
            let (id, rest) = line.split_once("  ").unwrap();
            assert_eq!(id.len(), 8, "{line:?}");
            rest
        })
        .collect()
}

fn free_text(name: &str) -> Option<PersonRef> {
    Some(PersonRef {
        r#ref: Some(person_ref::Ref::FreeText(name.to_owned())),
    })
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn no_command_execs_the_tui_entry_with_node() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipped: node is not on PATH");
        return;
    }
    let h = Home::new();
    let entry = h.home.join("tui.js");
    std::fs::create_dir_all(&h.home).unwrap();
    let stub = "console.log(JSON.stringify([process.argv.slice(2), \
                process.env.TODO_BIN, process.env.TODO_TUI_HOME])); process.exit(3);";
    std::fs::write(&entry, stub).unwrap();
    let run = |todo_bin: Option<&str>| {
        let mut cmd = h.command(&[]);
        cmd.env("TODO_TUI_ENTRY", &entry);
        match todo_bin {
            Some(bin) => cmd.env("TODO_BIN", bin),
            None => cmd.env_remove("TODO_BIN"),
        };
        cmd.output().unwrap()
    };
    let out = run(None);
    // The TUI's exit status is todo's.
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let (args, bin, home): (Vec<String>, PathBuf, PathBuf) =
        serde_json::from_slice(&out.stdout).unwrap();
    assert!(args.is_empty(), "{args:?}");
    // TODO_BIN is this binary, so the TUI starts its daemon.
    let exe = Path::new(env!("CARGO_BIN_EXE_todo"));
    assert_eq!(bin.canonicalize().unwrap(), exe.canonicalize().unwrap());
    assert_eq!(home, h.home);
    assert_eq!(text(&out.stderr), "");

    // A TODO_BIN that is set is kept; an empty one is not.
    let out = run(Some("/elsewhere/todo"));
    let (_, bin, _): (Vec<String>, String, PathBuf) = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(bin, "/elsewhere/todo");
    let out = run(Some(""));
    let (_, bin, _): (Vec<String>, PathBuf, PathBuf) = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(bin.canonicalize().unwrap(), exe.canonicalize().unwrap());
}

#[test]
fn a_tui_entry_that_is_missing_is_an_error() {
    let h = Home::new();
    std::fs::create_dir_all(&h.home).unwrap();
    let missing = h.home.join("missing/tui.js");
    let out = h
        .command(&[])
        .env("TODO_TUI_ENTRY", &missing)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(
        text(&out.stderr),
        format!(
            "todo: TODO_TUI_ENTRY is {}, which is not a file\n",
            missing.display()
        )
    );
    // Without node on PATH.
    let entry = h.home.join("tui.js");
    std::fs::write(&entry, "").unwrap();
    let out = h
        .command(&[])
        .env("TODO_TUI_ENTRY", &entry)
        .env("PATH", h.home.join("empty"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "todo: cannot run the TUI: node is not on PATH; install Node 24 or newer\n"
    );
    // --help still prints help.
    let out = h.todo(&["--help"]);
    assert!(out.status.success());
    assert!(text(&out.stdout).starts_with("A keyboard-driven todo manager\n"));
}

#[test]
fn add_auto_spawns_the_daemon_then_stop_stops_it() {
    let h = Home::new();
    let out = h.ok(&["add", "call", "the", "dentist"]);
    let (id, title) = out.trim_end().split_once("  ").unwrap();
    assert_eq!(id.len(), 8, "{out:?}");
    assert_eq!(title, "call the dentist");
    assert!(socket_in(&h.home.join("run")));

    // The alias reuses the running daemon.
    assert!(
        h.ok(&["a", "-v", "second"])
            .trim_end()
            .ends_with("  -v second")
    );

    assert_eq!(h.ok(&["daemon", "stop"]).trim(), "todo daemon: stopped");
    assert!(!socket_in(&h.home.join("run")));
    assert_eq!(h.ok(&["daemon", "stop"]).trim(), "todo daemon: not running");
}

#[test]
fn parallel_detach_leaves_one_daemon() {
    let h = Home::new();
    std::thread::scope(|s| {
        let runs: Vec<_> = (0..5)
            .map(|_| s.spawn(|| h.ok(&["daemon", "--detach"])))
            .collect();
        for r in runs {
            r.join().unwrap();
        }
    });
    let pid = std::fs::read_to_string(h.home.join("data/daemon.lock")).unwrap();
    assert!(pid.trim().parse::<u32>().is_ok(), "{pid:?}");
    assert_eq!(
        h.ok(&["daemon", "--detach"]).trim(),
        "todo daemon: already running"
    );
    h.ok(&["add", "still one daemon"]);
    assert_eq!(h.ok(&["daemon", "stop"]).trim(), "todo daemon: stopped");
}

#[test]
fn long_home_uses_the_hashed_tmp_fallback() {
    let h = Home::long();
    h.ok(&["add", "long path"]);
    assert!(!socket_in(&h.home.join("run")));
    // Look only at this home's directory: other homes may be using the
    // fallback at the same time.
    let runtime = h.paths().runtime;
    let uid = rustix::process::getuid().as_raw();
    let fallback = PathBuf::from(format!("/tmp/todo-tui-{uid}"));
    assert_eq!(runtime.parent(), Some(fallback.as_path()));
    let name = runtime.file_name().unwrap().to_string_lossy();
    assert!(name.starts_with("h-"), "{}", runtime.display());
    assert!(socket_in(&runtime));
    h.ok(&["daemon", "stop"]);
    assert!(!socket_in(&runtime));
    std::fs::remove_dir(runtime).unwrap();
}

#[test]
fn add_prints_the_parsed_fields_and_warnings() {
    let h = Home::new();
    let args = [
        "add",
        "call",
        "dentist",
        "^2026-10-24",
        "+2",
        "#home",
        "~dana",
        ">someday",
        "+9",
    ];
    let out = h.todo(&args);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        captured(text(&out.stdout)),
        ["call dentist +9  +2 ^2026-10-24 #home ~dana >someday"]
    );
    assert_eq!(
        text(&out.stderr),
        "todo: warning: +9: priority must be +1 to +5\n"
    );
    let out = h.ok(&["a", "ask", r#"~"the vendor""#, ">next"]);
    assert_eq!(captured(&out), [r#"ask  ~"the vendor" >next"#]);

    let items = h.items();
    assert_eq!(titles(&items), ["call dentist +9", "ask"]);
    let dentist = &items[0];
    assert_eq!(dentist.priority(), Priority::High);
    let due = Date {
        year: 2026,
        month: 10,
        day: 24,
    };
    assert_eq!(dentist.due_date, Some(due));
    assert_eq!(dentist.labels, ["home"]);
    assert_eq!(dentist.waiting.as_ref().unwrap().on, free_text("dana"));
    assert_eq!(dentist.status(), Status::Someday);
    assert_eq!(items[1].status(), Status::Next);
}

#[test]
fn a_capture_sends_its_id_as_request_and_spool_id() {
    let h = Home::new();
    h.ok(&["add", "once"]);
    h.ok(&["daemon", "stop"]);
    // Recorded as a spool id too, a spooled copy of this capture (if its
    // reply was lost) is skipped even after the 24h request id window.
    let db = rusqlite::Connection::open(h.paths().database()).unwrap();
    let keys: (String, Option<String>) = db
        .query_row(
            "SELECT r.request_id, s.spool_id FROM request_ids r
             LEFT JOIN spool_imports s ON s.op_id = r.op_id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(keys.1.as_ref(), Some(&keys.0));
}

#[test]
fn add_without_text_captures_each_line_of_stdin() {
    let h = Home::new();
    let input = "  first line  \n\n \t \nsecond +1 #x\r\n\nthird";
    let out = h.todo_with_stdin(&["add"], input.as_bytes());
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        captured(text(&out.stdout)),
        ["first line", "second  +1 #x", "third"]
    );
    assert_eq!(text(&out.stderr), "");
    assert_eq!(titles(&h.items()), ["first line", "second", "third"]);

    // Nothing to capture is not an error.
    let out = h.todo_with_stdin(&["add"], b"\n  \n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "");
}

/// A pseudo-terminal: its master, to type into, and the path of its slave.
fn pty() -> (File, PathBuf) {
    use rustix::io::{FdFlags, fcntl_setfd};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    fcntl_setfd(&master, FdFlags::CLOEXEC).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let name = ptsname(&master, Vec::new()).unwrap();
    let slave = PathBuf::from(OsStr::from_bytes(name.as_bytes()));
    (master.into(), slave)
}

/// Waits up to 10s for `child` to exit, then collects its stderr.
fn exit_of(child: &mut Child) -> (ExitStatus, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("todo still running after 10s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (status, stderr)
}

/// What `todo add` prints on stderr when stdin is a terminal.
const TERMINAL_PROMPT: &str =
    "todo add: each line is captured when you press Enter; end with Ctrl-D\n";

/// Starts `todo add` with the pty `slave` as stdin, and each line of its
/// stdout as it comes.
fn add_at(h: &Home, slave: &Path) -> (Child, mpsc::Receiver<String>) {
    let mut child = h
        .command(&["add"])
        .stdin(File::options().read(true).write(true).open(slave).unwrap())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    std::thread::spawn(move || {
        for line in stdout.lines() {
            let _ = tx.send(line.unwrap());
        }
    });
    (child, rx)
}

/// The next capture `add_at`'s `todo add` prints, without its id.
fn next_captured(rx: &mpsc::Receiver<String>) -> String {
    let line = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    captured(&line)[0].to_owned()
}

#[test]
fn add_at_a_terminal_captures_each_line_as_it_is_entered() {
    let h = Home::new();
    let (mut terminal, slave) = pty();

    let (mut child, rx) = add_at(&h, &slave);
    terminal.write_all(b"first +1\n").unwrap();
    assert_eq!(next_captured(&rx), "first  +1");
    assert_eq!(titles(&h.items()), ["first"]);
    terminal.write_all(b"\n  \nsecond #x\n").unwrap();
    assert_eq!(next_captured(&rx), "second  #x");
    // Ctrl-C loses nothing already entered.
    let pid = rustix::process::Pid::from_child(&child);
    rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
    let (status, stderr) = exit_of(&mut child);
    assert_eq!(status.signal(), Some(2), "{status:?}");
    assert_eq!(stderr, TERMINAL_PROMPT);
    assert_eq!(titles(&h.items()), ["first", "second"]);

    // Ctrl-D at the start of a line ends it.
    let (mut child, rx) = add_at(&h, &slave);
    terminal.write_all(b"third\n\x04").unwrap();
    assert_eq!(next_captured(&rx), "third");
    let (status, stderr) = exit_of(&mut child);
    assert!(status.success(), "{status:?}: {stderr}");
    assert_eq!(stderr, TERMINAL_PROMPT);
    assert!(rx.recv_timeout(Duration::from_secs(10)).is_err());
    assert_eq!(titles(&h.items()), ["first", "second", "third"]);

    // After text, Ctrl-D ends the line without a newline, and a second one
    // ends the input, as with `cat`.
    let (mut child, rx) = add_at(&h, &slave);
    terminal.write_all(b"fourth\x04\x04").unwrap();
    assert_eq!(next_captured(&rx), "fourth");
    let (status, stderr) = exit_of(&mut child);
    assert!(status.success(), "{status:?}: {stderr}");
    assert_eq!(stderr, TERMINAL_PROMPT);
    assert_eq!(titles(&h.items()), ["first", "second", "third", "fourth"]);
}

#[test]
fn stop_does_not_wait_for_the_terminal_add_that_spawned_the_daemon() {
    let h = Home::new();
    let (mut terminal, slave) = pty();
    let (mut child, rx) = add_at(&h, &slave);
    terminal.write_all(b"first\n").unwrap();
    assert_eq!(next_captured(&rx), "first");
    // The daemon is this `todo add`'s child, and it is still waiting for
    // lines: the daemon must not linger as a zombie once it exits.
    assert_eq!(h.ok(&["daemon", "stop"]), "todo daemon: stopped\n");
    // Later lines go to the spool.
    terminal.write_all(b"second\n\x04").unwrap();
    let line = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(line, "spooled  second");
    let (status, stderr) = exit_of(&mut child);
    assert!(status.success(), "{status:?}: {stderr}");
    let spool = std::fs::read_to_string(h.paths().spool()).unwrap();
    let entry: SpoolEntry = serde_json::from_str(spool.trim_end()).unwrap();
    assert_eq!(entry.text, "second");
}

#[test]
fn a_closed_stdout_does_not_stop_the_captures() {
    let h = Home::new();
    // Like `todo add < notes | head -1`, with the reader gone at once.
    let mut child = h
        .command(&["add"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"one\ntwo #x\nthree\n").unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(titles(&h.items()), ["one", "two", "three"]);
}

#[test]
fn a_line_over_64_kib_fails_the_run_but_not_the_other_lines() {
    let h = Home::new();
    // 64 KiB of UTF-8 in two-byte characters is accepted; one byte more is not.
    let longest = "é".repeat(MAX_CAPTURE_BYTES / 2);
    let input = format!("before\n{longest}x\n{longest}\nafter ^2026-10-24\n");
    let out = h.todo_with_stdin(&["add"], input.as_bytes());
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(
        captured(text(&out.stdout)),
        ["before", longest.as_str(), "after  ^2026-10-24"]
    );
    // The rejected line is named, cut short.
    assert_eq!(
        text(&out.stderr),
        format!(
            "todo: error: NOT CAPTURED: \"{}…\": longer than 64 KiB\n",
            "é".repeat(60)
        )
    );
    assert_eq!(titles(&h.items()), ["before", longest.as_str(), "after"]);
}

#[test]
fn a_capture_over_64_kib_is_neither_sent_nor_spooled() {
    let h = Home::new();
    let over = "y".repeat(MAX_CAPTURE_BYTES + 1);
    let out = h.todo(&["add", &over]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(
        text(&out.stderr),
        format!(
            "todo: error: NOT CAPTURED: \"{}…\": longer than 64 KiB\n",
            "y".repeat(60)
        )
    );
    // Not sent: no daemon was started for it.
    assert!(!socket_in(&h.home.join("run")));

    // Nor spooled while the daemon cannot start.
    std::fs::create_dir_all(&h.home).unwrap();
    std::fs::write(h.home.join("run"), "").unwrap();
    let input = format!("{over}\nkept\n");
    let out = h.todo_with_stdin(&["add"], input.as_bytes());
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "spooled  kept\n");
    let spool = std::fs::read_to_string(h.paths().spool()).unwrap();
    let entry: SpoolEntry = serde_json::from_str(spool.trim_end()).unwrap();
    assert_eq!(entry.text, "kept");
}

#[test]
fn a_stdin_line_that_is_not_utf8_is_captured_with_a_warning() {
    let h = Home::new();
    let out = h.todo_with_stdin(&["add"], b"ok\n\ncaf\xe9 au lait\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(captured(text(&out.stdout)), ["ok", "caf\u{fffd} au lait"]);
    assert_eq!(
        text(&out.stderr),
        "todo: warning: line 3 is not valid UTF-8; its invalid bytes were replaced with U+FFFD\n"
    );
    assert_eq!(titles(&h.items()), ["ok", "caf\u{fffd} au lait"]);
}

#[test]
fn captures_are_spooled_while_the_daemon_cannot_start_then_imported_once() {
    let h = Home::new();
    // A file where the runtime directory belongs: no daemon can listen.
    let run = h.home.join("run");
    std::fs::write(&run, "").unwrap();
    let input = "call dentist ^2026-10-24 +2 #home\n\nask ~\"the vendor\"\n";
    let out = h.todo_with_stdin(&["add"], input.as_bytes());
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "spooled  call dentist ^2026-10-24 +2 #home\nspooled  ask ~\"the vendor\"\n"
    );
    let stderr = text(&out.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.starts_with("todo: daemon unavailable ("), "{stderr}");
    let spool = h.paths().spool();
    assert!(
        stderr.ends_with(&format!(
            "; spooled to {}; it will be imported once the daemon is running again\n",
            spool.display()
        )),
        "{stderr}"
    );
    assert_eq!(h.ok(&["add", "later", ">next"]), "spooled  later >next\n");
    // Blank text is refused, not spooled for an import that would reject it.
    let out = h.todo(&["add", " "]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(text(&out.stderr), "todo: nothing to capture\n");

    let content = std::fs::read_to_string(&spool).unwrap();
    let entries: Vec<SpoolEntry> = content
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let texts: Vec<_> = entries.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "call dentist ^2026-10-24 +2 #home",
            r#"ask ~"the vendor""#,
            "later >next"
        ]
    );
    for e in &entries {
        assert!(e.spool_id.parse::<ulid::Ulid>().is_ok(), "{e:?}");
        assert!(e.captured_at.parse::<jiff::Timestamp>().is_ok(), "{e:?}");
    }
    assert_ne!(entries[0].spool_id, entries[1].spool_id);

    std::fs::remove_file(&run).unwrap();
    h.ok(&["daemon", "--detach"]);
    let imported = || std::fs::metadata(&spool).unwrap().len() == 0;
    wait_until("the spool is imported", imported);
    let check = |items: &[Item]| {
        assert_eq!(titles(items), ["call dentist", "ask", "later"]);
        let dentist = &items[0];
        assert_eq!(dentist.created_via(), CreatedVia::Cli);
        assert_eq!(dentist.priority(), Priority::High);
        let due = Date {
            year: 2026,
            month: 10,
            day: 24,
        };
        assert_eq!(dentist.due_date, Some(due));
        assert_eq!(dentist.labels, ["home"]);
        assert_eq!(
            items[1].waiting.as_ref().unwrap().on,
            free_text("the vendor")
        );
        assert_eq!(items[2].status(), Status::Next);
    };
    check(&h.items());

    // As if the daemon had stopped before truncating the spool: the same
    // entries are not imported a second time.
    h.ok(&["daemon", "stop"]);
    std::fs::write(&spool, &content).unwrap();
    h.ok(&["daemon", "--detach"]);
    wait_until("the spool is imported again", imported);
    check(&h.items());
}

#[test]
fn stop_gives_up_on_a_daemon_that_does_not_answer() {
    use std::os::unix::fs::PermissionsExt;
    let h = Home::new();
    let paths = h.paths();
    std::fs::create_dir_all(&paths.runtime).unwrap();
    std::fs::set_permissions(&paths.runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(&paths.data).unwrap();
    // A live pid, never waited for: the Shutdown call fails first.
    std::fs::write(&paths.lock, std::process::id().to_string()).unwrap();
    // A wedged daemon: it accepts connections and never answers.
    let listener = std::os::unix::net::UnixListener::bind(&paths.socket).unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            held.push(stream);
        }
    });
    let mut child = h
        .command(&["daemon", "stop"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            std::fs::remove_file(&paths.socket).unwrap();
            panic!("todo daemon stop still waiting after 20s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    // So that dropping the home finds no daemon to stop.
    std::fs::remove_file(&paths.socket).unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "todo: shutdown failed: no answer within 5s\n"
    );
}

#[test]
fn captures_are_spooled_when_the_spawned_daemon_exits() {
    let h = Home::new();
    // A directory where the daemon's lock file belongs: the spawned daemon
    // fails, but the spool beside it can be written.
    let paths = h.paths();
    std::fs::create_dir_all(&paths.lock).unwrap();
    let out = h.todo(&["add", "call", "dentist", "+2"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "spooled  call dentist +2\n");
    let stderr = text(&out.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.starts_with("todo: daemon unavailable (daemon exited with"),
        "{stderr}"
    );
    let content = std::fs::read_to_string(paths.spool()).unwrap();
    let entry: SpoolEntry = serde_json::from_str(content.trim_end()).unwrap();
    assert_eq!(entry.text, "call dentist +2");
}

#[test]
fn a_capture_that_cannot_be_spooled_fails_loudly() {
    let h = Home::new();
    // A file where the data directory belongs: the daemon cannot start and
    // the spool cannot be written.
    std::fs::write(h.home.join("data"), "").unwrap();
    let out = h.todo(&["add", "nowhere to go"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    let stderr = text(&out.stderr);
    assert!(stderr.contains("daemon unavailable ("), "{stderr}");
    assert!(
        stderr.contains(r#"NOT CAPTURED: "nowhere to go""#),
        "{stderr}"
    );
}

#[test]
fn undo_hides_a_capture_and_redo_brings_it_back() {
    let h = Home::new();
    h.ok(&["add", "call", "dentist", "+2"]);
    let id = h.items()[0].id.clone();

    let out = h.todo(&["undo"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "Undid: capture \"call dentist\"\n");
    assert_eq!(text(&out.stderr), "");
    // Soft-deleted: hidden from ListItems, a tombstone to GetItem.
    assert_eq!(h.items(), []);
    assert!(h.item(&id).deleted_at.is_some());

    assert_eq!(h.ok(&["redo"]), "Redid: capture \"call dentist\"\n");
    assert_eq!(titles(&h.items()), ["call dentist"]);
    let item = h.item(&id);
    assert_eq!(item.deleted_at, None);
    assert_eq!(item.priority(), Priority::High);
    assert_eq!(h.ok(&["redo"]), "Nothing to redo\n");

    // One history for every client: a change made over gRPC, as the TUI
    // makes it, is the next one `todo undo` undoes.
    h.rpc(async |mut items| {
        let request = SetStatusRequest {
            request_id: todo_core::new_ulid().to_string(),
            id: id.clone(),
            status: Status::Done.into(),
            expected_revision: None,
        };
        items.set_status(request).await.unwrap();
    });
    assert_eq!(h.ok(&["undo"]), "Undid: mark done \"call dentist\"\n");
    assert_eq!(h.item(&id).status(), Status::Inbox);
    assert_eq!(h.ok(&["undo"]), "Undid: capture \"call dentist\"\n");
    assert_eq!(h.ok(&["undo"]), "Nothing to undo\n");
    assert_eq!(h.ok(&["redo"]), "Redid: capture \"call dentist\"\n");
}

#[test]
fn undo_and_redo_on_a_fresh_home_have_nothing_to_do() {
    let h = Home::new();
    let out = h.todo(&["undo"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "Nothing to undo\n");
    assert_eq!(text(&out.stderr), "");
    // Started the daemon, as `todo add` does.
    assert!(socket_in(&h.home.join("run")));
    assert_eq!(h.ok(&["redo"]), "Nothing to redo\n");
}

#[test]
fn undo_reports_the_changes_it_leaves_alone() {
    use todo_core::{Actor, Mutation, MutationOp};
    use todo_store::{MutateInput, OpKind, Store};

    let h = Home::new();
    h.ok(&["add", "call", "dentist"]);
    let id = h.items()[0].id.clone();
    h.ok(&["daemon", "stop"]);
    // Marked done by the user, then moved on by the system, which no RPC
    // can do: a system op is not an undo stack entry.
    block_on(async {
        let store = Store::open(&h.paths().database()).unwrap();
        let now = jiff::Timestamp::now();
        let today = now.to_zoned(jiff::tz::TimeZone::system()).date();
        let set_status = |actor, status| MutateInput {
            request_id: todo_core::new_ulid().to_string(),
            actor,
            kind: OpKind::SetStatus,
            mutations: vec![Mutation {
                id: id.parse().unwrap(),
                expected_revision: None,
                op: MutationOp::SetStatus(status),
            }],
            now,
            today,
        };
        let done = set_status(Actor::User, todo_core::Status::Done);
        store.mutate(done).await.unwrap();
        let next = set_status(Actor::System, todo_core::Status::Next);
        store.mutate(next).await.unwrap();
    });

    let short = &id[id.len() - 8..];
    let out = h.todo(&["undo"]);
    // Succeeds, though it restored nothing.
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!(
            "Undid: mark done \"call dentist\"\n  \
             not restored: status of {short} (the field has changed since)\n"
        )
    );
    assert_eq!(text(&out.stderr), "");
    assert_eq!(h.item(&id).status(), Status::Next);
    // Restoring nothing dropped the entry from both stacks.
    assert_eq!(h.ok(&["redo"]), "Nothing to redo\n");
    assert_eq!(h.ok(&["undo"]), "Undid: capture \"call dentist\"\n");
}

#[test]
fn undo_without_a_daemon_fails_and_spools_nothing() {
    let h = Home::new();
    // A file where the runtime directory belongs: no daemon can listen.
    std::fs::write(h.home.join("run"), "").unwrap();
    for command in ["undo", "redo"] {
        let out = h.todo(&[command]);
        assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "");
        let stderr = text(&out.stderr);
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
        assert!(stderr.starts_with("todo: daemon unavailable ("), "{stderr}");
    }
    assert!(!h.paths().spool().exists());
}
