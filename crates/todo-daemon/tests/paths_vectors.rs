//! Table-driven tests over `testdata/paths.json`, shared with the TS client.

use std::path::PathBuf;

use serde::Deserialize;
use todo_daemon::paths::{Env, Os, Paths, PathsError};

#[derive(Deserialize)]
struct File {
    vectors: Vec<Vector>,
}

#[derive(Deserialize)]
struct Vector {
    name: String,
    env: VectorEnv,
    #[serde(default)]
    expect: Option<Expect>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct VectorEnv {
    os: String,
    home: Option<PathBuf>,
    todo_tui_home: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
    xdg_state_home: Option<PathBuf>,
    xdg_runtime_dir: Option<PathBuf>,
    tmpdir: Option<PathBuf>,
    uid: u32,
}

#[derive(Deserialize)]
struct Expect {
    config: PathBuf,
    data: PathBuf,
    logs: PathBuf,
    runtime: PathBuf,
    socket: PathBuf,
    lock: PathBuf,
}

#[test]
fn shared_path_vectors() {
    let raw = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testdata/paths.json"
    ));
    let file: File = serde_json::from_str(raw).expect("valid paths.json");
    assert!(!file.vectors.is_empty());

    for v in file.vectors {
        let e = v.env;
        let env = Env {
            os: match e.os.as_str() {
                "macos" => Os::MacOs,
                "linux" => Os::Linux,
                other => panic!("{}: unknown os {other}", v.name),
            },
            home: e.home,
            todo_tui_home: e.todo_tui_home,
            xdg_config_home: e.xdg_config_home,
            xdg_data_home: e.xdg_data_home,
            xdg_state_home: e.xdg_state_home,
            xdg_runtime_dir: e.xdg_runtime_dir,
            tmpdir: e.tmpdir,
            uid: e.uid,
        };
        let got = Paths::resolve(&env);
        match (v.expect, v.error.as_deref()) {
            (Some(x), None) => {
                let want = Paths {
                    config: x.config,
                    data: x.data,
                    logs: x.logs,
                    runtime: x.runtime,
                    socket: x.socket,
                    lock: x.lock,
                };
                assert_eq!(got.as_ref(), Ok(&want), "{}", v.name);
                assert!(want.socket.as_os_str().len() <= 100, "{}", v.name);
            }
            (None, Some("no_home")) => {
                assert_eq!(got, Err(PathsError::NoHome), "{}", v.name);
            }
            (None, Some("relative_todo_tui_home")) => {
                assert!(
                    matches!(got, Err(PathsError::RelativeTodoTuiHome(_))),
                    "{}: {got:?}",
                    v.name
                );
            }
            _ => panic!("{}: needs exactly one of expect / known error", v.name),
        }
    }
}
