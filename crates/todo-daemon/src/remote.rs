//! The remote listener (ADR-0004): its `[remote]` config, the paired-device
//! tokens and the interceptor that checks them.
//!
//! The listener serves gRPC-Web on a loopback TCP address for the phone app;
//! `tailscale serve` adds TLS in front of it. Every call on it carries
//! `authorization: Bearer <token>`, checked against the SHA-256s in
//! [`Paths::remote_tokens`](crate::paths::Paths::remote_tokens). The CLI writes that
//! file; the daemon rereads it whenever it changes, so pairing and revoking
//! need no restart.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tonic::service::Interceptor;
use tonic::{Request, Status};

/// Prefix of every token, so one is recognisable in a paste or a log.
pub const TOKEN_PREFIX: &str = "todo_";

/// The part of `config.toml` the daemon reads before M6 brings the rest.
/// Unknown sections and keys are ignored, so M6's additions do not break it.
#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    remote: Option<RemoteConfig>,
}

/// `[remote]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfig {
    /// Where the listener binds, e.g. `127.0.0.1:7743`.
    pub listen: SocketAddr,
    /// Allows an address other than loopback. Off by default: the intended
    /// setup puts `tailscale serve` in front of a loopback listener.
    #[serde(default)]
    pub allow_non_loopback: bool,
}

/// The `[remote]` section of the config file at `path`: `None` when the
/// file or the section is absent.
pub fn read_config(path: &Path) -> anyhow::Result<Option<RemoteConfig>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let file: ConfigFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let Some(remote) = file.remote else {
        return Ok(None);
    };
    if !remote.listen.ip().is_loopback() && !remote.allow_non_loopback {
        bail!(
            "[remote] listen = \"{}\" is not a loopback address; put `tailscale serve` in \
             front of 127.0.0.1, or set allow_non_loopback = true",
            remote.listen
        );
    }
    Ok(Some(remote))
}

/// One paired device. Only the token's SHA-256 is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    /// Hex SHA-256 of the whole token, prefix included.
    pub sha256: String,
    /// RFC 3339.
    pub created_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TokenFile {
    devices: Vec<PairedDevice>,
}

fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// The paired devices in the tokens file; none when it does not exist.
pub fn list_devices(path: &Path) -> anyhow::Result<Vec<PairedDevice>> {
    Ok(read_tokens(path)?.devices)
}

fn read_tokens(path: &Path) -> anyhow::Result<TokenFile> {
    match fs::read_to_string(path) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(TokenFile::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Replaces the tokens file atomically, mode 0600.
fn write_tokens(path: &Path, file: &TokenFile) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    // A tmp left by an older run may have another mode.
    out.set_permissions(fs::Permissions::from_mode(0o600))?;
    out.write_all(serde_json::to_string_pretty(file)?.as_bytes())?;
    out.write_all(b"\n")?;
    out.sync_all()?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Pairs a device: stores the SHA-256 of a new random token and returns the
/// device and the token, which is never stored and cannot be shown again.
pub fn pair(
    path: &Path,
    name: &str,
    now: jiff::Timestamp,
) -> anyhow::Result<(PairedDevice, String)> {
    let name = name.trim();
    if name.is_empty() {
        bail!("the device name must not be empty");
    }
    let mut file = read_tokens(path)?;
    if file
        .devices
        .iter()
        .any(|d| d.name.eq_ignore_ascii_case(name))
    {
        bail!("a device named {name:?} is already paired; revoke it first or pick another name");
    }
    let mut secret = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut secret))
        .context("reading /dev/urandom")?;
    let token = format!("{TOKEN_PREFIX}{}", hex::encode(secret));
    let device = PairedDevice {
        id: todo_core::new_ulid().to_string(),
        name: name.to_owned(),
        sha256: hash(&token),
        created_at: now.to_string(),
    };
    file.devices.push(device.clone());
    write_tokens(path, &file)?;
    Ok((device, token))
}

/// Revokes the device whose id (or its last characters, at least 4, as
/// `todo remote list` shows it) or name is `which`. Returns it.
pub fn revoke(path: &Path, which: &str) -> anyhow::Result<PairedDevice> {
    let mut file = read_tokens(path)?;
    let which_up = which.to_ascii_uppercase();
    let matches: Vec<usize> = file
        .devices
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            d.name.eq_ignore_ascii_case(which) || (which.len() >= 4 && d.id.ends_with(&which_up))
        })
        .map(|(i, _)| i)
        .collect();
    let i = match matches.as_slice() {
        [i] => *i,
        [] => bail!("no paired device matches {which:?}"),
        _ => bail!("{which:?} matches more than one device; use more of its id"),
    };
    let device = file.devices.remove(i);
    write_tokens(path, &file)?;
    Ok(device)
}

/// Request extension on every call that arrived on the remote listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Remote;

/// The tokens file as last read, reread when its mtime or size changes.
#[derive(Debug)]
struct Cache {
    stamp: Option<(SystemTime, u64)>,
    hashes: Vec<[u8; 32]>,
}

/// The remote listener's interceptor: checks the bearer token and marks the
/// request [`Remote`].
#[derive(Debug, Clone)]
pub(crate) struct Auth {
    path: PathBuf,
    cache: Arc<Mutex<Cache>>,
}

impl Auth {
    pub(crate) fn new(path: PathBuf) -> Self {
        Auth {
            path,
            cache: Arc::new(Mutex::new(Cache {
                stamp: None,
                hashes: Vec::new(),
            })),
        }
    }

    /// Whether `token` belongs to a paired device. A file that cannot be
    /// read or parsed pairs nothing.
    fn accepts(&self, token: &str) -> bool {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = fs::metadata(&self.path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        if stamp != cache.stamp {
            cache.hashes = match read_tokens(&self.path) {
                Ok(file) => file
                    .devices
                    .iter()
                    .filter_map(|d| hex::decode(&d.sha256).ok()?.try_into().ok())
                    .collect(),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "remote tokens unreadable; refusing all");
                    Vec::new()
                }
            };
            cache.stamp = stamp;
        }
        let want: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        // Compare against every hash, without stopping early.
        cache
            .hashes
            .iter()
            .fold(false, |found, h| found | constant_time_eq(h, &want))
    }
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Interceptor for Auth {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        let token = request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim);
        match token {
            Some(t) if self.accepts(t) => {
                request.extensions_mut().insert(Remote);
                Ok(request)
            }
            Some(_) => Err(Status::unauthenticated(
                "unknown or revoked token; pair again with `todo remote pair`",
            )),
            None => Err(Status::unauthenticated(
                "missing authorization: Bearer <token>",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_needs_loopback_unless_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert_eq!(read_config(&path).unwrap(), None);
        fs::write(&path, "[other]\nx = 1\n").unwrap();
        assert_eq!(read_config(&path).unwrap(), None);
        fs::write(&path, "[remote]\nlisten = \"127.0.0.1:7743\"\n").unwrap();
        assert_eq!(
            read_config(&path).unwrap().unwrap().listen,
            "127.0.0.1:7743".parse().unwrap()
        );
        fs::write(&path, "[remote]\nlisten = \"0.0.0.0:7743\"\n").unwrap();
        assert!(read_config(&path).is_err());
        fs::write(
            &path,
            "[remote]\nlisten = \"0.0.0.0:7743\"\nallow_non_loopback = true\n",
        )
        .unwrap();
        assert!(read_config(&path).unwrap().unwrap().allow_non_loopback);
        fs::write(&path, "[remote]\nlisten = \"127.0.0.1:7743\"\ntypo = 1\n").unwrap();
        assert!(read_config(&path).is_err());
    }

    #[test]
    fn pair_accept_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote-tokens.json");
        let auth = Auth::new(path.clone());
        assert!(!auth.accepts("todo_nope"));
        let (phone, token) = pair(&path, "phone", jiff::Timestamp::UNIX_EPOCH).unwrap();
        assert!(token.starts_with(TOKEN_PREFIX));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!fs::read_to_string(&path).unwrap().contains(&token));
        assert!(auth.accepts(&token));
        assert!(!auth.accepts("todo_nope"));
        assert!(pair(&path, "Phone", jiff::Timestamp::UNIX_EPOCH).is_err());
        let (_, other) = pair(&path, "tablet", jiff::Timestamp::UNIX_EPOCH).unwrap();
        assert_eq!(list_devices(&path).unwrap().len(), 2);
        assert!(revoke(&path, "x").is_err());
        let gone = revoke(&path, &phone.id[phone.id.len() - 6..].to_lowercase()).unwrap();
        assert_eq!(gone, phone);
        // A same-second rewrite can keep the mtime; the size changes.
        assert!(!auth.accepts(&token));
        assert!(auth.accepts(&other));
        revoke(&path, "TABLET").unwrap();
        assert!(!auth.accepts(&other));
    }

    #[test]
    fn interceptor_checks_the_bearer_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote-tokens.json");
        let (_, token) = pair(&path, "phone", jiff::Timestamp::UNIX_EPOCH).unwrap();
        let mut auth = Auth::new(path);
        let with = |v: Option<&str>| {
            let mut r = Request::new(());
            if let Some(v) = v {
                r.metadata_mut().insert("authorization", v.parse().unwrap());
            }
            r
        };
        let err = auth.call(with(None)).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
        let err = auth.call(with(Some("Bearer todo_bad"))).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
        let ok = auth.call(with(Some(&format!("Bearer {token}")))).unwrap();
        assert_eq!(ok.extensions().get::<Remote>(), Some(&Remote));
    }
}
