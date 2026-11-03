//! `config.toml` (design 8.2): read by the daemon only, never written.
//! Every problem is a warning that names its key, and the value falls back
//! to its default, so a bad file never stops the daemon. Only a file that
//! is not TOML at all fails a reload; at start it is read as empty.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;
use todo_core::Weekday;
use todo_core::report::ReportConfig;
use todo_proto::convert::{EnumField as _, timestamp_to_proto};
use todo_proto::v1;
use tokio::sync::{Mutex, watch};

/// The config file format this build reads.
pub const CONFIG_VERSION: u32 = 1;

/// [`Ui::archive_done_after_days`] by default.
pub const ARCHIVE_DONE_AFTER_DAYS: u32 = 7;

/// [`Backup::retain_days`] by default.
pub const RETAIN_DAYS: u32 = 14;

/// The longest `[l48] window_days`: a month of working days.
pub const MAX_WINDOW_DAYS: u32 = 31;

/// The config the daemon runs with: the file's values, defaults for the
/// rest, and what was wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub path: PathBuf,
    pub exists: bool,
    pub config_version: u32,
    pub ui: Ui,
    /// Key sequence → action id; `""` unbinds.
    pub keymap: BTreeMap<String, String>,
    pub backup: Backup,
    pub l48: ReportConfig,
    /// `[meetings] provider`: false for "none" (design 2.9).
    pub meetings: bool,
    pub warnings: Vec<Warning>,
    pub loaded_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ui {
    pub review_day: Weekday,
    pub archive_done_after_days: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    /// False also when `retain_days` is invalid (design 7.5).
    pub enabled: bool,
    pub interval: Interval,
    pub retain_days: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interval {
    Daily,
}

impl Interval {
    pub fn name(self) -> &'static str {
        match self {
            Interval::Daily => "daily",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// Dotted, e.g. `ui.archive_done_after_days` or `keymap.K`; empty for
    /// the file as a whole.
    pub key: String,
    pub message: String,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.key.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.key, self.message)
        }
    }
}

/// A file that cannot be read as TOML, or at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {message}")]
pub struct ConfigError {
    pub path: String,
    pub message: String,
}

impl Config {
    /// Every value a default, as with no file.
    pub fn defaults(path: PathBuf) -> Config {
        Config {
            path,
            exists: false,
            config_version: CONFIG_VERSION,
            ui: Ui {
                review_day: Weekday::Monday,
                archive_done_after_days: ARCHIVE_DONE_AFTER_DAYS,
            },
            keymap: BTreeMap::new(),
            backup: Backup {
                enabled: true,
                interval: Interval::Daily,
                retain_days: RETAIN_DAYS,
            },
            l48: ReportConfig::default(),
            meetings: true,
            warnings: Vec::new(),
            loaded_at: Timestamp::now(),
        }
    }

    /// Reads `path`. A missing file is the defaults; one that is not TOML
    /// (or cannot be read) is an error.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Config::defaults(path.to_owned()));
            }
            Err(e) => return Err(error(path, e.to_string())),
        };
        Config::parse(path, &text)
    }

    /// [`Config::load`], except that a file that is not TOML is read as
    /// empty with a warning saying why: what the daemon starts with.
    pub fn load_or_defaults(path: &Path) -> Config {
        Config::load(path).unwrap_or_else(|e| {
            let mut config = Config::defaults(path.to_owned());
            config.exists = path.exists();
            config.warnings.push(Warning {
                key: String::new(),
                message: format!("{}; using the defaults", e.message),
            });
            config
        })
    }

    /// Reads `text` as the contents of `path`.
    pub fn parse(path: &Path, text: &str) -> Result<Config, ConfigError> {
        let table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
            let at = e
                .span()
                .map(|span| format!("line {}: ", line_of(text, span.start)))
                .unwrap_or_default();
            error(path, format!("{at}{}", e.message().trim_end()))
        })?;
        let mut config = Config::defaults(path.to_owned());
        config.exists = true;
        Reader::new(&mut config).read(table);
        Ok(config)
    }

    /// How long the scheduler waits before it archives a closed Item.
    pub fn archive_done_after(&self) -> SignedDuration {
        SignedDuration::from_hours(i64::from(self.ui.archive_done_after_days) * 24)
    }

    /// Whether `other` differs from this in anything but `loaded_at`.
    pub fn same_as(&self, other: &Config) -> bool {
        Config {
            loaded_at: other.loaded_at,
            ..self.clone()
        } == *other
    }
}

impl From<&Config> for v1::ConfigSnapshot {
    fn from(c: &Config) -> Self {
        v1::ConfigSnapshot {
            path: c.path.display().to_string(),
            exists: c.exists,
            config_version: c.config_version,
            ui: Some(v1::UiConfig {
                review_day: c.ui.review_day.to_i32(),
                archive_done_after_days: c.ui.archive_done_after_days,
            }),
            keymap: c.keymap.clone().into_iter().collect(),
            backup: Some(v1::BackupConfig {
                enabled: c.backup.enabled,
                interval: c.backup.interval.name().to_owned(),
                retain_days: c.backup.retain_days,
            }),
            meetings: Some(v1::MeetingsConfig {
                provider: if c.meetings { "teams" } else { "none" }.to_owned(),
            }),
            l48: Some(v1::L48Config {
                window_days: c.l48.window_days,
                working_days: c.l48.working_days.iter().map(|d| d.to_i32()).collect(),
            }),
            warnings: c
                .warnings
                .iter()
                .map(|w| v1::ConfigWarning {
                    key: w.key.clone(),
                    message: w.message.clone(),
                })
                .collect(),
            loaded_at: Some(timestamp_to_proto(c.loaded_at)),
        }
    }
}

fn error(path: &Path, message: String) -> ConfigError {
    ConfigError {
        path: path.display().to_string(),
        message,
    }
}

/// The 1-based line of byte `offset` in `text`.
fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

/// Extracts each key by hand, so each problem gets its own warning.
struct Reader<'a> {
    config: &'a mut Config,
}

impl<'a> Reader<'a> {
    fn new(config: &'a mut Config) -> Self {
        Reader { config }
    }

    fn warn(&mut self, key: impl Into<String>, message: impl Into<String>) {
        self.config.warnings.push(Warning {
            key: key.into(),
            message: message.into(),
        });
    }

    fn read(mut self, table: toml::Table) {
        for (key, value) in table {
            match key.as_str() {
                "config_version" => self.version(value),
                "ui" => self.section("ui", value, Reader::ui),
                "keymap" => self.section("keymap", value, Reader::keymap),
                "backup" => self.section("backup", value, Reader::backup),
                "l48" => self.section("l48", value, Reader::l48),
                "meetings" => self.section("meetings", value, Reader::meetings),
                "instances" => self.warn(key, "not used until Jira sync (P2)"),
                "assistant" => self.warn(key, "not used until the Claude assistant (P3)"),
                "stale" => self.warn(key, "not used until stale reviews (P4)"),
                _ => self.warn(quote_key(&key), "unknown key; ignored"),
            }
        }
    }

    fn version(&mut self, value: toml::Value) {
        match value.as_integer().and_then(|v| u32::try_from(v).ok()) {
            Some(v @ 1..) => {
                self.config.config_version = v;
                if v > CONFIG_VERSION {
                    self.warn(
                        "config_version",
                        format!(
                            "{v} is newer than this todo understands ({CONFIG_VERSION}); \
                             keys it does not know are ignored"
                        ),
                    );
                }
            }
            _ => self.warn("config_version", "must be a whole number ≥ 1; using 1"),
        }
    }

    fn section(&mut self, name: &str, value: toml::Value, read: fn(&mut Self, toml::Table)) {
        match value {
            toml::Value::Table(table) => read(self, table),
            other => self.warn(
                name,
                format!("must be a table ([{name}]), not {}", kind(&other)),
            ),
        }
    }

    fn ui(&mut self, table: toml::Table) {
        for (key, value) in table {
            let path = format!("ui.{}", quote_key(&key));
            match key.as_str() {
                "archive_done_after_days" => match days(&value) {
                    Some(days) => self.config.ui.archive_done_after_days = days,
                    None => self.warn(
                        path,
                        format!("must be a whole number ≥ 1; using {ARCHIVE_DONE_AFTER_DAYS}"),
                    ),
                },
                "review_day" => match value.as_str().and_then(Weekday::from_name) {
                    Some(day) => self.config.ui.review_day = day,
                    None => self.warn(path, "must be a weekday, e.g. \"monday\"; using monday"),
                },
                _ => self.warn(path, "unknown key; ignored"),
            }
        }
    }

    fn backup(&mut self, table: toml::Table) {
        let mut retain_invalid = false;
        for (key, value) in table {
            let path = format!("backup.{}", quote_key(&key));
            match key.as_str() {
                "enabled" => match value.as_bool() {
                    Some(enabled) => self.config.backup.enabled = enabled,
                    None => self.warn(path, "must be true or false; using true"),
                },
                "interval" => {
                    if value.as_str() != Some("daily") {
                        self.warn(path, "must be \"daily\"; using daily");
                    }
                }
                "retain_days" => match days(&value) {
                    Some(days) => self.config.backup.retain_days = days,
                    None => {
                        retain_invalid = true;
                        self.warn(
                            path,
                            "must be a whole number ≥ 1; automatic backups are off until it is",
                        );
                    }
                },
                _ => self.warn(path, "unknown key; ignored"),
            }
        }
        // After the loop, so an `enabled = true` written later cannot undo it.
        if retain_invalid {
            self.config.backup.enabled = false;
        }
    }

    fn meetings(&mut self, table: toml::Table) {
        for (key, value) in table {
            let path = format!("meetings.{}", quote_key(&key));
            match (key.as_str(), value.as_str()) {
                ("provider", Some("teams")) => self.config.meetings = true,
                ("provider", Some("none")) => self.config.meetings = false,
                ("provider", _) => self.warn(path, "must be \"teams\" or \"none\"; using teams"),
                _ => self.warn(path, "unknown key; ignored"),
            }
        }
    }

    fn l48(&mut self, table: toml::Table) {
        for (key, value) in table {
            let path = format!("l48.{}", quote_key(&key));
            match key.as_str() {
                "window_days" => match days(&value).filter(|&d| d <= MAX_WINDOW_DAYS) {
                    Some(days) => self.config.l48.window_days = days,
                    None => self.warn(
                        path,
                        format!("must be a whole number from 1 to {MAX_WINDOW_DAYS}; using 2"),
                    ),
                },
                "working_days" => {
                    let days: Option<Vec<Weekday>> = value.as_array().and_then(|days| {
                        days.iter()
                            .map(|d| d.as_str().and_then(Weekday::from_name))
                            .collect()
                    });
                    match days {
                        Some(mut days) if !days.is_empty() => {
                            days.sort();
                            days.dedup();
                            self.config.l48.working_days = days;
                        }
                        _ => self.warn(
                            path,
                            "must be a list of weekdays, e.g. [\"mon\", \"tue\"]; using monday to friday",
                        ),
                    }
                }
                _ => self.warn(path, "unknown key; ignored"),
            }
        }
    }

    fn keymap(&mut self, table: toml::Table) {
        for (keys, value) in table {
            let path = format!("keymap.{}", quote_key(&keys));
            let Some(action) = value.as_str() else {
                self.warn(
                    path,
                    format!("must be an action id in quotes, not {}", kind(&value)),
                );
                continue;
            };
            if let Err(why) = check_sequence(&keys) {
                self.warn(path, why);
                continue;
            }
            if !action.is_empty() && !VOCABULARY.is_action(action) {
                self.warn(
                    path,
                    format!("{action:?} is not an action id (design 2.5 lists them)"),
                );
                continue;
            }
            self.config.keymap.insert(keys, action.to_owned());
        }
    }
}

/// A key as TOML would need it written in a dotted path: bare when it can
/// be, else a basic string.
fn quote_key(key: &str) -> String {
    if !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return key.to_owned();
    }
    let mut out = String::with_capacity(key.len() + 2);
    out.push('"');
    for c in key.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A whole number of days, at least 1.
fn days(value: &toml::Value) -> Option<u32> {
    value
        .as_integer()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v >= 1)
}

fn kind(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "a string",
        toml::Value::Integer(_) => "a number",
        toml::Value::Float(_) => "a number",
        toml::Value::Boolean(_) => "true or false",
        toml::Value::Datetime(_) => "a date",
        toml::Value::Array(_) => "a list",
        toml::Value::Table(_) => "a table",
    }
}

/// The TUI's keymap vocabulary (`keymap.json`, kept equal to the TUI's
/// table by its tests).
#[derive(Deserialize)]
struct Vocabulary {
    actions: BTreeMap<String, Vec<String>>,
    named_tokens: Vec<String>,
    /// ctrl+<letter> keys the terminal sends as another key, and that key.
    ctrl_aliases: BTreeMap<String, String>,
}

impl Vocabulary {
    fn is_action(&self, id: &str) -> bool {
        self.actions.values().flatten().any(|a| a == id)
    }

    fn is_token(&self, token: &str) -> bool {
        token.chars().count() == 1 && !token.chars().any(char::is_control)
            || self.named_tokens.iter().any(|t| t == token)
            || token
                .strip_prefix("ctrl+")
                .is_some_and(|k| k.len() == 1 && k.as_bytes()[0].is_ascii_lowercase())
    }
}

static VOCABULARY: LazyLock<Vocabulary> = LazyLock::new(|| {
    serde_json::from_str(include_str!("keymap.json")).expect("keymap.json is valid")
});

/// Whether `keys` is a key sequence: tokens separated by single spaces.
fn check_sequence(keys: &str) -> Result<(), String> {
    if keys.is_empty() {
        return Err("an empty key sequence".to_owned());
    }
    for token in keys.split(' ') {
        if token.is_empty() {
            return Err(
                "separate the keys of a sequence with single spaces, e.g. \"g g\"".to_owned(),
            );
        }
        if let Some(key) = VOCABULARY.ctrl_aliases.get(token) {
            return Err(format!(
                "{token} reaches todo as {key}, so bind {key:?} instead"
            ));
        }
        if !VOCABULARY.is_token(token) {
            return Err(format!(
                "{token:?} is not a key: use one character, ctrl+<letter>, or a name such as \
                 enter, esc, tab, space, up, pagedown"
            ));
        }
    }
    Ok(())
}

/// The running config, shared by the RPCs, SIGHUP and the scheduler.
#[derive(Clone)]
pub(crate) struct ConfigHandle {
    tx: Arc<watch::Sender<Arc<Config>>>,
    path: PathBuf,
    /// One reload at a time, so two never race to publish or record.
    reloading: Arc<Mutex<()>>,
    /// A published config whose change is not recorded yet.
    unrecorded: Arc<AtomicBool>,
}

impl ConfigHandle {
    pub(crate) fn new(config: Config) -> Self {
        let path = config.path.clone();
        ConfigHandle {
            tx: Arc::new(watch::channel(Arc::new(config)).0),
            path,
            reloading: Arc::new(Mutex::new(())),
            unrecorded: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn current(&self) -> Arc<Config> {
        self.tx.borrow().clone()
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Arc<Config>> {
        self.tx.subscribe()
    }

    /// Reads the file again. On success returns the new config and whether
    /// it differs from the old one; the receivers see it only if it does.
    /// A file that is not TOML keeps the running config.
    #[cfg(test)]
    pub(crate) async fn reload(&self) -> Result<(Arc<Config>, bool), ConfigError> {
        let _one = self.reloading.lock().await;
        self.reload_locked().await
    }

    async fn reload_locked(&self) -> Result<(Arc<Config>, bool), ConfigError> {
        let path = self.path.clone();
        let config = tokio::task::spawn_blocking(move || Config::load(&path))
            .await
            .map_err(|e| error(&self.path, e.to_string()))??;
        let changed = !self.current().same_as(&config);
        let config = Arc::new(config);
        if changed {
            self.tx.send_replace(Arc::clone(&config));
        }
        Ok((config, changed))
    }
}

/// Why a reload failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReloadError {
    /// The file is not TOML, or cannot be read: the running config stays.
    #[error(transparent)]
    File(#[from] ConfigError),
    /// The config changed but its change could not be recorded, so clients
    /// have not seen it; the next reload records it.
    #[error("recording the config change failed: {0}")]
    Store(#[from] todo_store::StoreError),
}

/// Reloads the config and, when it changed, records a config change, so
/// every subscriber gets the new one. Shared by `ReloadConfig` and SIGHUP.
/// A change whose record failed is recorded by the next reload, changed or
/// not.
pub(crate) async fn reload(
    handle: &ConfigHandle,
    store: &todo_store::Store,
) -> Result<(Arc<Config>, bool), ReloadError> {
    let _one = handle.reloading.lock().await;
    let (config, changed) = handle.reload_locked().await?;
    if changed || handle.unrecorded.load(Ordering::Acquire) {
        handle.unrecorded.store(true, Ordering::Release);
        store.record_config_change().await?;
        handle.unrecorded.store(false, Ordering::Release);
        tracing::info!(warnings = config.warnings.len(), "config reloaded");
    }
    Ok((config, changed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Config {
        Config::parse(Path::new("/c/config.toml"), text).expect("valid TOML")
    }

    fn warnings(text: &str) -> Vec<String> {
        parse(text)
            .warnings
            .iter()
            .map(Warning::to_string)
            .collect()
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        let config = parse("");
        let defaults = Config::defaults(PathBuf::from("/c/config.toml"));
        assert!(config.exists);
        assert!(config.same_as(&Config {
            exists: true,
            ..defaults
        }));
        assert_eq!(config.ui.archive_done_after_days, 7);
        assert_eq!(config.ui.review_day, Weekday::Monday);
        assert!(config.backup.enabled);
        assert_eq!(config.backup.retain_days, 14);
        assert_eq!(
            config.archive_done_after(),
            SignedDuration::from_hours(7 * 24)
        );
    }

    #[test]
    fn a_missing_file_is_the_defaults_without_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("config.toml")).unwrap();
        assert!(!config.exists);
        assert!(config.warnings.is_empty());
    }

    #[test]
    fn reads_every_key() {
        let config = parse(
            r#"
            config_version = 1
            [ui]
            review_day = "Fri"
            archive_done_after_days = 3
            [keymap]
            "K" = "move_up"
            "g d" = "delete"
            "x" = ""
            [backup]
            enabled = false
            interval = "daily"
            retain_days = 30
            [l48]
            window_days = 3
            working_days = ["sun", "Monday", "tue", "mon"]
            [meetings]
            provider = "none"
            "#,
        );
        assert_eq!(config.warnings, vec![]);
        assert!(!config.meetings);
        assert_eq!(
            warnings("[meetings]\nprovider = \"zoom\""),
            ["meetings.provider: must be \"teams\" or \"none\"; using teams"]
        );
        assert_eq!(config.l48.window_days, 3);
        assert_eq!(
            config.l48.working_days,
            [Weekday::Monday, Weekday::Tuesday, Weekday::Sunday]
        );
        assert_eq!(config.ui.review_day, Weekday::Friday);
        assert_eq!(config.ui.archive_done_after_days, 3);
        assert!(!config.backup.enabled);
        assert_eq!(config.backup.retain_days, 30);
        let keymap: Vec<_> = config
            .keymap
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(keymap, [("K", "move_up"), ("g d", "delete"), ("x", "")]);
    }

    #[test]
    fn a_bad_l48_value_warns_and_falls_back() {
        assert_eq!(
            warnings("[l48]\nwindow_days = 0\nworking_days = []\nlater = 1"),
            [
                "l48.window_days: must be a whole number from 1 to 31; using 2",
                "l48.working_days: must be a list of weekdays, e.g. [\"mon\", \"tue\"]; using monday to friday",
                "l48.later: unknown key; ignored",
            ]
        );
        assert_eq!(
            warnings("[l48]\nworking_days = [\"mon\", \"funday\"]").len(),
            1
        );
        let config = parse("[l48]\nwindow_days = 99\nworking_days = \"mon\"");
        assert_eq!(config.l48, todo_core::report::ReportConfig::default());
    }

    #[test]
    fn a_bad_value_warns_and_falls_back() {
        assert_eq!(
            warnings("[ui]\narchive_done_after_days = 0\nreview_day = 3"),
            [
                "ui.archive_done_after_days: must be a whole number ≥ 1; using 7",
                "ui.review_day: must be a weekday, e.g. \"monday\"; using monday",
            ]
        );
        assert_eq!(
            parse("[ui]\narchive_done_after_days = 0")
                .ui
                .archive_done_after_days,
            7
        );
        assert_eq!(
            warnings("[backup]\nenabled = \"yes\"\ninterval = \"hourly\""),
            [
                "backup.enabled: must be true or false; using true",
                "backup.interval: must be \"daily\"; using daily",
            ]
        );
        assert_eq!(
            warnings("config_version = \"one\""),
            ["config_version: must be a whole number ≥ 1; using 1"]
        );
        assert_eq!(
            warnings("ui = 3"),
            ["ui: must be a table ([ui]), not a number"]
        );
    }

    #[test]
    fn invalid_retain_days_turns_automatic_backups_off() {
        for text in [
            "[backup]\nretain_days = 0",
            "[backup]\nretain_days = \"two weeks\"",
            "[backup]\nretain_days = 1.5",
        ] {
            let config = parse(text);
            assert!(!config.backup.enabled, "{text}");
            assert_eq!(
                config.warnings[0].to_string(),
                "backup.retain_days: must be a whole number ≥ 1; automatic backups are off until it is"
            );
        }
    }

    #[test]
    fn invalid_retain_days_wins_over_a_later_enabled() {
        let config = parse("[backup]\nretain_days = 0\nenabled = true\n");
        assert!(!config.backup.enabled);
        assert_eq!(config.warnings[0].key, "backup.retain_days");
    }

    #[test]
    fn warning_keys_are_written_as_toml_would() {
        assert_eq!(quote_key("K"), "K");
        assert_eq!(quote_key("g g"), "\"g g\"");
        assert_eq!(quote_key("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(quote_key("\t\u{0}\u{7f}é"), "\"\\t\\u0000\\u007Fé\"");
        assert_eq!(
            warnings("\"a.b\" = 1\n[ui]\n\"a b\" = 1\n[backup]\n\"x y\" = 1"),
            [
                "\"a.b\": unknown key; ignored",
                "ui.\"a b\": unknown key; ignored",
                "backup.\"x y\": unknown key; ignored",
            ]
        );
    }

    #[test]
    fn unknown_and_future_keys_warn() {
        assert_eq!(
            warnings(
                "colour = 1\n[ui]\ntheme = \"dark\"\n[[instances]]\nid = \"w\"\n[assistant]\n[stale]\n"
            ),
            [
                "colour: unknown key; ignored",
                "ui.theme: unknown key; ignored",
                "instances: not used until Jira sync (P2)",
                "assistant: not used until the Claude assistant (P3)",
                "stale: not used until stale reviews (P4)",
            ]
        );
    }

    #[test]
    fn a_newer_config_version_warns_but_is_read() {
        let config = parse("config_version = 2\n[ui]\narchive_done_after_days = 2");
        assert_eq!(config.config_version, 2);
        assert_eq!(config.ui.archive_done_after_days, 2);
        assert_eq!(
            config.warnings[0].to_string(),
            "config_version: 2 is newer than this todo understands (1); keys it does not know are ignored"
        );
    }

    #[test]
    fn keymap_checks_actions_and_keys() {
        let config = parse(
            r#"
            [keymap]
            "K" = "set_priority"
            "ctrl+x" = "quit"
            "ctrl+X" = "quit"
            "g  g" = "top"
            "<f2>" = "help"
            "alt+j" = "move_down"
            "fkey" = "help_close"
            "space" = 3
            "" = "quit"
            "#,
        );
        let got: Vec<String> = config.warnings.iter().map(Warning::to_string).collect();
        let not_a_key = "is not a key: use one character, ctrl+<letter>, or a name such as \
                         enter, esc, tab, space, up, pagedown";
        assert_eq!(
            got,
            [
                "keymap.K: \"set_priority\" is not an action id (design 2.5 lists them)".to_owned(),
                format!("keymap.\"ctrl+X\": \"ctrl+X\" {not_a_key}"),
                "keymap.\"g  g\": separate the keys of a sequence with single spaces, e.g. \"g g\""
                    .to_owned(),
                format!("keymap.\"<f2>\": \"<f2>\" {not_a_key}"),
                format!("keymap.\"alt+j\": \"alt+j\" {not_a_key}"),
                "keymap.space: must be an action id in quotes, not a number".to_owned(),
                "keymap.\"\": an empty key sequence".to_owned(),
            ]
        );
        let kept: Vec<_> = config.keymap.keys().map(String::as_str).collect();
        assert_eq!(kept, ["ctrl+x", "fkey"]);
    }

    #[test]
    fn ctrl_keys_the_terminal_sends_as_another_key_are_refused() {
        let config = parse(
            "[keymap]\n\"ctrl+i\" = \"quit\"\n\"ctrl+m\" = \"quit\"\n\"ctrl+space\" = \"mark\"\n",
        );
        let got: Vec<String> = config.warnings.iter().map(Warning::to_string).collect();
        assert_eq!(
            got,
            [
                "keymap.\"ctrl+i\": ctrl+i reaches todo as tab, so bind \"tab\" instead",
                "keymap.\"ctrl+m\": ctrl+m reaches todo as enter, so bind \"enter\" instead",
            ]
        );
        assert_eq!(config.keymap.keys().collect::<Vec<_>>(), ["ctrl+space"]);
    }

    #[test]
    fn not_toml_is_an_error_naming_the_line() {
        let err = Config::parse(
            Path::new("/c/config.toml"),
            "[ui]\narchive_done_after_days = \n",
        )
        .unwrap_err();
        assert!(err.message.starts_with("line 2: "), "{err}");
        assert_eq!(err.path, "/c/config.toml");
    }

    #[test]
    fn at_start_a_file_that_is_not_toml_is_read_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[ui\n").unwrap();
        let config = Config::load_or_defaults(&path);
        assert!(config.exists);
        assert_eq!(config.ui.archive_done_after_days, 7);
        assert_eq!(config.warnings.len(), 1);
        assert!(
            config.warnings[0].message.starts_with("line 1: "),
            "{:?}",
            config.warnings
        );
        assert!(config.warnings[0].message.ends_with("; using the defaults"));
    }

    #[tokio::test]
    async fn reload_publishes_only_a_change_and_keeps_the_config_on_bad_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let handle = ConfigHandle::new(Config::load_or_defaults(&path));
        let mut rx = handle.subscribe();
        rx.mark_unchanged();

        let (_, changed) = handle.reload().await.unwrap();
        assert!(!changed);
        assert!(!rx.has_changed().unwrap());

        std::fs::write(&path, "[ui]\narchive_done_after_days = 2\n").unwrap();
        let (config, changed) = handle.reload().await.unwrap();
        assert!(changed);
        assert_eq!(config.ui.archive_done_after_days, 2);
        assert!(rx.has_changed().unwrap());
        assert_eq!(rx.borrow_and_update().ui.archive_done_after_days, 2);

        std::fs::write(&path, "[ui\n").unwrap();
        assert!(handle.reload().await.is_err());
        assert_eq!(handle.current().ui.archive_done_after_days, 2);
        assert!(!rx.has_changed().unwrap());
    }

    #[test]
    fn the_vocabulary_parses() {
        assert!(VOCABULARY.is_action("move_up"));
        assert!(VOCABULARY.is_action("help_close"));
        assert!(!VOCABULARY.is_action("set_priority"));
        for token in ["j", "G", "?", "^", "enter", "shift+tab", "ctrl+d", "fkey"] {
            assert!(VOCABULARY.is_token(token), "{token}");
        }
        for token in ["", "jj", "ctrl+", "ctrl+D", "ctrl+dd", "alt+j", "\u{7}"] {
            assert!(!VOCABULARY.is_token(token), "{token}");
        }
    }
}
