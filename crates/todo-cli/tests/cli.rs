//! End-to-end tests of the `todo` binary on temp `TODO_TUI_HOME`s.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use todo_daemon::paths::{Env, Paths};
use todo_daemon::spool::SpoolEntry;
use todo_proto::google::r#type::Date;
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::{CreatedVia, Item, ListItemsRequest, PersonRef, Priority, Status, person_ref};

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

    /// Every Item, oldest first, read from the running daemon over gRPC.
    fn items(&self) -> Vec<Item> {
        let socket = self.paths().socket;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let channel = todo_daemon::client::connect(&socket).await.unwrap();
            let request = ListItemsRequest {
                page_size: 1000,
                ..Default::default()
            };
            ItemServiceClient::new(channel)
                .list_items(request)
                .await
                .unwrap()
                .into_inner()
                .items
        })
    }
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
fn a_rejected_line_fails_the_run_but_not_the_other_lines() {
    let h = Home::new();
    // Longer than the daemon accepts in one message (4 MiB).
    let huge = "x".repeat(5 << 20);
    let input = format!("before\n{huge}\nafter ^2026-10-24\n");
    let out = h.todo_with_stdin(&["add"], input.as_bytes());
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(
        captured(text(&out.stdout)),
        ["before", "after  ^2026-10-24"]
    );
    // The rejected line is named, cut short.
    let stderr = text(&out.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    let named = format!("todo: error: NOT CAPTURED: \"{}…\": ", "x".repeat(60));
    assert!(stderr.starts_with(&named), "{stderr}");
    assert!(stderr.len() < 1000, "{stderr}");
    assert_eq!(titles(&h.items()), ["before", "after"]);
}

#[test]
fn a_capture_whose_reply_exceeds_4_mib_is_reported_as_captured() {
    let h = Home::new();
    // The request fits the daemon's 4 MiB limit; the reply carries the
    // title twice (the Item and the draft).
    let long = "y".repeat(5 << 19);
    let out = h.todo_with_stdin(&["add"], format!("{long}\n").as_bytes());
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(captured(text(&out.stdout)), [long.as_str()]);
    assert_eq!(h.items().len(), 1);
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
