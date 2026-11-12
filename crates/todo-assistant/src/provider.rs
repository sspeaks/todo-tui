//! The providers the assistant runs a task on (design 6.1, ADR-0010). A task
//! is a prompt and a JSON Schema for the answer; a provider returns the
//! answer as JSON, validated by the provider against the schema, and what it
//! cost.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// One request to Claude: the whole context as one prompt, and the shape of
/// the answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub system: String,
    pub prompt: String,
    pub schema: Value,
}

/// What a task cost, as the provider reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Input read from the prompt cache.
    pub cached_tokens: u64,
    pub cost_usd: f64,
    pub duration_ms: u64,
}

/// A task's answer: JSON in the task's schema.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub json: Value,
    pub usage: Usage,
}

/// Why a task did not answer, in words the person can act on.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProviderError {
    #[error("Claude Code is not installed: `{0}` was not found (set [assistant] claude_bin)")]
    NotInstalled(String),
    #[error("Claude did not answer within {0} s")]
    Timeout(u64),
    #[error("Claude Code is not logged in: run `claude` once to log in")]
    NotLoggedIn,
    #[error("Claude refused the request: {0}")]
    Refused(String),
    #[error("Claude's answer could not be read: {0}")]
    BadAnswer(String),
}

/// Something that runs a task (design 6.1): Claude Code's CLI, or a fake.
pub trait Provider: Send + Sync {
    fn run(&self, task: Task) -> impl Future<Output = Result<Answer, ProviderError>> + Send;
}

/// The person's own `claude` CLI, headless (design 6.1 `claude_code`): no
/// tools, no MCP servers, no saved session, and run in an empty directory,
/// so the context the daemon builds is all it sees (no project CLAUDE.md or
/// memory). Its user settings load, as they hold how it reaches Claude (a
/// gateway, a token, model names); the daemon never reads them, nor Claude
/// Code's credentials: the CLI uses whatever it is logged into.
#[derive(Debug, Clone)]
pub struct ClaudeCode {
    pub bin: PathBuf,
    /// Empty: the CLI's own default.
    pub model: String,
    pub timeout: Duration,
}

impl ClaudeCode {
    /// The arguments, the prompt going on stdin.
    pub fn args(&self, task: &Task) -> Vec<String> {
        let mut args = vec![
            "-p".to_owned(),
            "--output-format".to_owned(),
            "json".to_owned(),
            "--json-schema".to_owned(),
            task.schema.to_string(),
            "--system-prompt".to_owned(),
            task.system.clone(),
            "--tools".to_owned(),
            String::new(),
            "--strict-mcp-config".to_owned(),
            // User settings only: how the CLI reaches Claude, not a
            // project's settings or hooks.
            "--setting-sources".to_owned(),
            "user".to_owned(),
            "--no-session-persistence".to_owned(),
        ];
        if !self.model.is_empty() {
            args.push("--model".to_owned());
            args.push(self.model.clone());
        }
        args
    }
}

impl Provider for ClaudeCode {
    async fn run(&self, task: Task) -> Result<Answer, ProviderError> {
        // An empty directory: no project's CLAUDE.md or memory joins the
        // context.
        let dir = std::env::temp_dir().join(format!("todo-claude-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut child = Command::new(&self.bin)
            .args(self.args(&task))
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    ProviderError::NotInstalled(self.bin.display().to_string())
                }
                _ => {
                    ProviderError::BadAnswer(format!("could not start {}: {e}", self.bin.display()))
                }
            })?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let prompt = task.prompt.clone();
        let wrote = tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
        });
        let out = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| ProviderError::Timeout(self.timeout.as_secs()))?
            .map_err(|e| ProviderError::BadAnswer(e.to_string()))?;
        let _ = wrote.await;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.trim().is_empty() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(stderr_error(err.trim()));
        }
        parse_envelope(&stdout)
    }
}

/// An error from a run that printed nothing: what stderr says.
fn stderr_error(err: &str) -> ProviderError {
    if login_error(err) {
        ProviderError::NotLoggedIn
    } else {
        ProviderError::BadAnswer(first_line(err, "no output"))
    }
}

/// Whether the CLI says it is not logged in: its own words, not any message
/// that mentions a login.
fn login_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    ["please run /login", "not logged in", "invalid api key"]
        .iter()
        .any(|w| lower.contains(w))
}

fn first_line(s: &str, empty: &str) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or(empty);
    line.chars().take(300).collect()
}

/// The CLI's `--output-format json` envelope: `structured_output` holds the
/// answer; `is_error` with the message in `result` a failure, or with only
/// an `error_…` subtype (a turn limit, no valid structured answer) one that
/// logging in would not fix.
pub fn parse_envelope(stdout: &str) -> Result<Answer, ProviderError> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| ProviderError::BadAnswer(format!("not JSON ({e})")))?;
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
    if v.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
        let msg = first_line(text("result"), "");
        if v.get("api_error_status").and_then(Value::as_u64) == Some(401) || login_error(&msg) {
            return Err(ProviderError::NotLoggedIn);
        }
        if msg.is_empty() {
            return Err(ProviderError::BadAnswer(match text("subtype") {
                "" | "success" => "an error with no message".to_owned(),
                s => s.replace('_', " "),
            }));
        }
        return Err(ProviderError::Refused(msg));
    }
    let json = match v.get("structured_output") {
        Some(Value::Object(_)) => v["structured_output"].clone(),
        // An older CLI: the answer as text in `result`.
        _ => serde_json::from_str(text("result"))
            .map_err(|_| ProviderError::BadAnswer("no structured answer".to_owned()))?,
    };
    let n = |path: &[&str]| {
        path.iter()
            .try_fold(&v, |at, k| at.get(k))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let usage = Usage {
        input_tokens: n(&["usage", "input_tokens"]) + n(&["usage", "cache_creation_input_tokens"]),
        output_tokens: n(&["usage", "output_tokens"]),
        cached_tokens: n(&["usage", "cache_read_input_tokens"]),
        cost_usd: v
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        duration_ms: n(&["duration_ms"]),
    };
    Ok(Answer { json, usage })
}

/// A provider for tests: answers each task with `answer(task)`.
pub struct Fake<F>(pub F);

impl<F> Provider for Fake<F>
where
    F: Fn(&Task) -> Result<Answer, ProviderError> + Send + Sync,
{
    async fn run(&self, task: Task) -> Result<Answer, ProviderError> {
        (self.0)(&task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped as Claude Code 2.1.308 printed it; content synthetic.
    const SUCCESS: &str = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":5762,"num_turns":2,"result":"{\"title\":\"Buy milk\"}","structured_output":{"title":"Buy milk"},"total_cost_usd":0.000689,"usage":{"input_tokens":315,"cache_creation_input_tokens":3150,"cache_read_input_tokens":12,"output_tokens":55}}"#;
    const FAILED: &str = r#"{"type":"result","subtype":"success","is_error":true,"result":"API Error: 400 unknown model\n\ndetails","api_error_status":400}"#;

    #[test]
    fn an_answer_and_its_usage_are_read_from_the_envelope() {
        let a = parse_envelope(SUCCESS).unwrap();
        assert_eq!(a.json, serde_json::json!({"title": "Buy milk"}));
        assert_eq!(
            (
                a.usage.input_tokens,
                a.usage.output_tokens,
                a.usage.cached_tokens,
                a.usage.duration_ms
            ),
            (3465, 55, 12, 5762)
        );
        // An older CLI's text answer.
        let old = r#"{"is_error":false,"result":"{\"title\":\"x\"}"}"#;
        assert_eq!(
            parse_envelope(old).unwrap().json,
            serde_json::json!({"title": "x"})
        );
    }

    #[test]
    fn failures_say_what_to_do() {
        assert_eq!(
            parse_envelope(FAILED),
            Err(ProviderError::Refused(
                "API Error: 400 unknown model".into()
            ))
        );
        let login = r#"{"is_error":true,"result":"Invalid API key · Please run /login"}"#;
        assert_eq!(parse_envelope(login), Err(ProviderError::NotLoggedIn));
        let status = r#"{"is_error":true,"result":"API Error: denied","api_error_status":401}"#;
        assert_eq!(parse_envelope(status), Err(ProviderError::NotLoggedIn));
        // A message that only mentions a login page is not a login error.
        let page =
            r#"{"is_error":true,"result":"API Error: 403 see https://sso.example.com/login"}"#;
        assert!(matches!(
            parse_envelope(page),
            Err(ProviderError::Refused(_))
        ));
        // No message: the subtype says what went wrong.
        let turns = r#"{"is_error":true,"subtype":"error_max_structured_output_retries"}"#;
        assert_eq!(
            parse_envelope(turns),
            Err(ProviderError::BadAnswer(
                "error max structured output retries".into()
            ))
        );
        assert!(matches!(
            parse_envelope("not json"),
            Err(ProviderError::BadAnswer(_))
        ));
        assert!(matches!(
            parse_envelope(r#"{"is_error":false,"result":"prose"}"#),
            Err(ProviderError::BadAnswer(_))
        ));
    }

    fn script(dir: &std::path::Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("claude");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn task() -> Task {
        Task {
            system: "sys".into(),
            prompt: "the prompt".into(),
            schema: serde_json::json!({"type": "object"}),
        }
    }

    #[tokio::test]
    async fn the_cli_gets_the_prompt_on_stdin_and_no_tools() {
        let dir = tempfile::tempdir().unwrap();
        // Echoes its stdin and arguments back as the answer.
        let bin = script(
            dir.path(),
            r#"exec python3 -I -c 'import json,sys; print(json.dumps({"is_error": False, "structured_output": {"stdin": sys.stdin.read(), "args": " ".join(sys.argv[1:])}}))' "$@""#,
        );
        let cc = ClaudeCode {
            bin,
            model: "claude-haiku-5-5".into(),
            timeout: Duration::from_secs(10),
        };
        let a = cc.run(task()).await.unwrap();
        assert_eq!(a.json["stdin"], "the prompt");
        let args = a.json["args"].as_str().unwrap();
        for want in [
            "-p",
            "--output-format json",
            "--strict-mcp-config",
            "--setting-sources user",
            "--no-session-persistence",
            "--model claude-haiku-5-5",
            "--system-prompt sys",
        ] {
            assert!(args.contains(want), "{want:?} not in {args:?}");
        }
    }

    #[tokio::test]
    async fn a_missing_binary_a_hang_and_a_login_error_are_named() {
        let dir = tempfile::tempdir().unwrap();
        let missing = ClaudeCode {
            bin: dir.path().join("nope"),
            model: String::new(),
            timeout: Duration::from_secs(5),
        };
        assert!(matches!(
            missing.run(task()).await,
            Err(ProviderError::NotInstalled(_))
        ));
        let hang = ClaudeCode {
            bin: script(dir.path(), "sleep 30"),
            model: String::new(),
            timeout: Duration::from_millis(200),
        };
        assert_eq!(hang.run(task()).await, Err(ProviderError::Timeout(0)));
        let login = ClaudeCode {
            bin: script(
                dir.path(),
                "echo 'Not logged in. Please run /login' >&2; exit 1",
            ),
            model: String::new(),
            timeout: Duration::from_secs(5),
        };
        assert_eq!(login.run(task()).await, Err(ProviderError::NotLoggedIn));
    }
}
