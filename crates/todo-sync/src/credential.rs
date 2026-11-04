//! Credentials (design 5.10): config holds only a reference, resolved here.
//! The secret is never logged, and never leaves the daemon or the CLI.

use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where an instance's secret is.
#[derive(Clone, PartialEq, Eq)]
pub enum CredentialRef {
    /// The macOS login keychain: a generic password by service and, if
    /// given, account (`keychain:todo-tui/work-jira`).
    Keychain {
        service: String,
        account: Option<String>,
    },
    /// An environment variable (`env:TODO_JIRA_PAT`).
    Env(String),
    /// A file of mode 0600 or tighter (`file:~/.config/todo-tui/jira.token`).
    File(PathBuf),
}

impl fmt::Debug for CredentialRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CredentialRef::Keychain {
                service,
                account: Some(a),
            } => write!(f, "keychain:{service}/{a}"),
            CredentialRef::Keychain {
                service,
                account: None,
            } => write!(f, "keychain:{service}"),
            CredentialRef::Env(name) => write!(f, "env:{name}"),
            CredentialRef::File(path) => write!(f, "file:{}", path.display()),
        }
    }
}

impl CredentialRef {
    /// Reads `keychain:service[/account]`, `env:NAME` or `file:PATH` (`~/`
    /// for `home`).
    pub fn parse(text: &str, home: Option<&Path>) -> Result<Self, String> {
        let (scheme, rest) = text
            .split_once(':')
            .ok_or_else(|| "must be keychain:…, env:… or file:…".to_owned())?;
        let rest = rest.trim();
        if rest.is_empty() {
            return Err(format!("{scheme}: needs a name after the colon"));
        }
        match scheme {
            "keychain" => {
                let (service, account) = match rest.split_once('/') {
                    Some((s, a)) if !s.is_empty() && !a.is_empty() => (s, Some(a.to_owned())),
                    Some(_) => return Err("keychain: write it as service/account".to_owned()),
                    None => (rest, None),
                };
                Ok(CredentialRef::Keychain {
                    service: service.to_owned(),
                    account,
                })
            }
            "env" => {
                if rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    Ok(CredentialRef::Env(rest.to_owned()))
                } else {
                    Err("env: a variable name is letters, digits and _".to_owned())
                }
            }
            "file" => {
                let path = match (rest.strip_prefix("~/"), home) {
                    (Some(tail), Some(home)) => home.join(tail),
                    (Some(_), None) => return Err("file: ~ needs a home directory".to_owned()),
                    (None, _) => PathBuf::from(rest),
                };
                if path.is_relative() {
                    return Err("file: the path must be absolute (or start with ~/)".to_owned());
                }
                Ok(CredentialRef::File(path))
            }
            other => Err(format!("{other}: must be keychain:…, env:… or file:…")),
        }
    }
}

/// A resolved secret. Its Debug never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential(String);

impl Credential {
    pub fn new(secret: impl Into<String>) -> Self {
        Credential(secret.into())
    }

    /// For the Authorization header only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential(…)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("{0}: not found")]
    NotFound(String),
    #[error("{0}: empty")]
    Empty(String),
    #[error("{0}: readable by others (mode {1:o}); chmod 600 it")]
    TooOpen(String, u32),
    #[error("{0}: {1}")]
    Unreadable(String, String),
}

/// Reads secrets; tests replace the keychain and the environment.
pub trait Secrets {
    /// The keychain's generic password, `None` when there is none.
    fn keychain(&self, service: &str, account: Option<&str>) -> Result<Option<String>, String>;
    fn env(&self, name: &str) -> Option<String>;
}

/// The real ones: `security` on macOS, the process environment.
pub struct SystemSecrets;

impl Secrets for SystemSecrets {
    fn keychain(&self, service: &str, account: Option<&str>) -> Result<Option<String>, String> {
        let mut cmd = Command::new("/usr/bin/security");
        cmd.args(["find-generic-password", "-s", service]);
        if let Some(a) = account {
            cmd.args(["-a", a]);
        }
        cmd.arg("-w");
        let out = cmd
            .output()
            .map_err(|e| format!("security: {e} (the keychain is macOS only)"))?;
        // 44: errSecItemNotFound.
        match out.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned())),
            Some(44) => Ok(None),
            _ => Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
        }
    }

    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// The secret `r` names, trimmed of surrounding whitespace (a token file
/// usually ends with a newline).
pub fn resolve(r: &CredentialRef, secrets: &dyn Secrets) -> Result<Credential, CredentialError> {
    let at = r.to_string();
    let raw = match r {
        CredentialRef::Keychain { service, account } => secrets
            .keychain(service, account.as_deref())
            .map_err(|e| CredentialError::Unreadable(at.clone(), e))?
            .ok_or_else(|| CredentialError::NotFound(at.clone()))?,
        CredentialRef::Env(name) => secrets
            .env(name)
            .ok_or_else(|| CredentialError::NotFound(at.clone()))?,
        CredentialRef::File(path) => {
            let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => CredentialError::NotFound(at.clone()),
                _ => CredentialError::Unreadable(at.clone(), e.to_string()),
            })?;
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                return Err(CredentialError::TooOpen(at, mode));
            }
            std::fs::read_to_string(path)
                .map_err(|e| CredentialError::Unreadable(at.clone(), e.to_string()))?
        }
    };
    let secret = raw.trim();
    if secret.is_empty() {
        return Err(CredentialError::Empty(at));
    }
    Ok(Credential::new(secret))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct Fake {
        keychain: HashMap<(String, Option<String>), String>,
        env: HashMap<String, String>,
    }

    impl Secrets for Fake {
        fn keychain(&self, service: &str, account: Option<&str>) -> Result<Option<String>, String> {
            Ok(self
                .keychain
                .get(&(service.to_owned(), account.map(str::to_owned)))
                .cloned())
        }
        fn env(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }
    }

    fn fake() -> Fake {
        Fake {
            keychain: HashMap::from([(
                ("todo-tui".to_owned(), Some("work-jira".to_owned())),
                "kc-secret\n".to_owned(),
            )]),
            env: HashMap::from([
                ("TODO_JIRA_PAT".to_owned(), " env-secret ".to_owned()),
                ("BLANK".to_owned(), "  ".to_owned()),
            ]),
        }
    }

    #[test]
    fn references_parse() {
        let home = Path::new("/home/me");
        assert_eq!(
            CredentialRef::parse("keychain:todo-tui/work-jira", Some(home)),
            Ok(CredentialRef::Keychain {
                service: "todo-tui".into(),
                account: Some("work-jira".into())
            })
        );
        assert_eq!(
            CredentialRef::parse("keychain:jira", None),
            Ok(CredentialRef::Keychain {
                service: "jira".into(),
                account: None
            })
        );
        assert_eq!(
            CredentialRef::parse("env:TODO_JIRA_PAT", None),
            Ok(CredentialRef::Env("TODO_JIRA_PAT".into()))
        );
        assert_eq!(
            CredentialRef::parse("file:~/.config/t.token", Some(home)),
            Ok(CredentialRef::File("/home/me/.config/t.token".into()))
        );
        for bad in [
            "plain",
            "vault:x",
            "env:",
            "env:A-B",
            "file:rel/path",
            "keychain:a/",
            "keychain:/b",
        ] {
            assert!(CredentialRef::parse(bad, Some(home)).is_err(), "{bad}");
        }
    }

    #[test]
    fn secrets_resolve_trimmed() {
        let f = fake();
        let kc = CredentialRef::parse("keychain:todo-tui/work-jira", None).unwrap();
        assert_eq!(resolve(&kc, &f).unwrap().expose(), "kc-secret");
        let env = CredentialRef::Env("TODO_JIRA_PAT".into());
        assert_eq!(resolve(&env, &f).unwrap().expose(), "env-secret");
        assert_eq!(
            resolve(&CredentialRef::Env("NOPE".into()), &f),
            Err(CredentialError::NotFound("env:NOPE".into()))
        );
        assert_eq!(
            resolve(&CredentialRef::Env("BLANK".into()), &f),
            Err(CredentialError::Empty("env:BLANK".into()))
        );
        let other = CredentialRef::parse("keychain:todo-tui/other", None).unwrap();
        assert!(matches!(
            resolve(&other, &f),
            Err(CredentialError::NotFound(_))
        ));
    }

    #[test]
    fn a_token_file_must_be_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jira.token");
        std::fs::write(&path, "file-secret\n").unwrap();
        let r = CredentialRef::File(path.clone());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            resolve(&r, &fake()),
            Err(CredentialError::TooOpen(_, 0o644))
        ));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(resolve(&r, &fake()).unwrap().expose(), "file-secret");
        let missing = CredentialRef::File(dir.path().join("none"));
        assert!(matches!(
            resolve(&missing, &fake()),
            Err(CredentialError::NotFound(_))
        ));
    }

    #[test]
    fn debug_never_shows_the_secret() {
        assert_eq!(format!("{:?}", Credential::new("hunter2")), "Credential(…)");
    }
}
