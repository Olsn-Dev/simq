//! Local configuration and identity. Lives in the OS configuration folder,
//! never in the shared folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::github::GithubConfig;
use crate::i18n::{t, Lang};

/// Light or dark mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    Light,
    Dark,
    /// Follow the computer's setting (also for unknown values in the configuration file).
    #[default]
    #[serde(other)]
    System,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark];

    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => t("Som datorn", "Same as the computer"),
            ThemeChoice::Light => t("Ljust", "Light"),
            ThemeChoice::Dark => t("Mörkt", "Dark"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub sync_folder: Option<PathBuf>,
    /// Set when the app syncs with a GitHub repository instead of a shared folder.
    #[serde(default)]
    pub github: Option<GithubConfig>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub language: Lang,
    #[serde(default)]
    pub theme: ThemeChoice,
    pub client_id: Uuid,
    /// The app version that last started with these settings. Missing in
    /// settings from before 0.6.0.
    #[serde(default)]
    pub last_seen_version: Option<String>,
    /// Only in the file: true when the team key is in the system's password
    /// store instead of in the file. Always false in memory, where the key is
    /// in `github.token` as usual.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub token_in_keychain: bool,
}

impl Config {
    fn fresh() -> Self {
        Config {
            sync_folder: None,
            github: None,
            display_name: String::new(),
            // Settings from before this choice existed keep Swedish (the
            // serde default); a new installation follows the computer.
            language: Lang::of_computer(),
            theme: ThemeChoice::default(),
            client_id: Uuid::new_v4(),
            last_seen_version: None,
            token_in_keychain: false,
        }
    }

    /// True once the first-run dialog has been completed.
    pub fn is_complete(&self) -> bool {
        (self.sync_folder.is_some() || self.github.is_some())
            && !self.display_name.trim().is_empty()
    }

    /// The folder the app works against: the shared folder, or the local mirror
    /// of the GitHub repository.
    pub fn effective_folder(&self, paths: &Paths) -> Option<PathBuf> {
        match &self.github {
            Some(github) => Some(paths.mirror_folder(&github.repo)),
            None => self.sync_folder.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub cache_file: PathBuf,
    /// The local mirrors of GitHub repositories live here.
    pub mirror_root: PathBuf,
    /// Settings from when the app was called SimKö. Read once if no new ones exist.
    pub legacy_config_file: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "{}",
        t(
            "Hittar ingen konfigurationsmapp för den här användaren",
            "Cannot find a configuration folder for this user"
        )
    )]
    NoHome,
    #[error(
        "{}: {0}",
        t("Kan inte spara inställningarna", "Cannot save the settings")
    )]
    Io(#[from] std::io::Error),
    #[error(
        "{}: {0}",
        t("Kan inte spara inställningarna", "Cannot save the settings")
    )]
    Json(#[from] serde_json::Error),
}

impl Paths {
    /// One mirror folder per repository, so events are never mixed between repositories.
    pub fn mirror_folder(&self, repo: &str) -> PathBuf {
        let name: String = repo
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.mirror_root.join(name)
    }
}

/// With `--config-dir` both settings and cache end up in that folder, so that
/// several instances can run on the same computer.
pub fn resolve_paths(override_dir: Option<&Path>) -> Result<Paths, ConfigError> {
    if let Some(dir) = override_dir {
        return Ok(Paths {
            config_file: dir.join("config.json"),
            cache_file: dir.join("cache.sqlite"),
            mirror_root: dir.join("mirror"),
            legacy_config_file: None,
        });
    }
    let dirs = directories::ProjectDirs::from("se", "SimQ", "SimQ").ok_or(ConfigError::NoHome)?;
    Ok(Paths {
        config_file: dirs.config_dir().join("config.json"),
        cache_file: dirs.cache_dir().join("cache.sqlite"),
        mirror_root: dirs.data_local_dir().join("mirror"),
        legacy_config_file: directories::ProjectDirs::from("se", "SimKo", "SimKo")
            .map(|old| old.config_dir().join("config.json")),
    })
}

/// The version whose news the user should see now, and notes that this
/// version has started. Returns the version last seen, or `None` on a first
/// run (nothing to tell then). Settings from before 0.6.0 lack the field and
/// count as 0.5.2.
pub fn take_last_seen_version(config: &mut Config, current: &str) -> Option<String> {
    let last = match config.last_seen_version.take() {
        Some(last) => Some(last),
        None if config.is_complete() => Some("0.5.2".to_string()),
        None => None,
    };
    config.last_seen_version = Some(current.to_string());
    last
}

// ---------------------------------------------------------------------------
// The team key in the system's password store
// ---------------------------------------------------------------------------

/// A place to keep the team key outside the settings file.
pub trait Secrets {
    /// True if the secret was stored.
    fn set(&self, account: &str, secret: &str) -> bool;
    fn get(&self, account: &str) -> Option<String>;
    fn delete(&self, account: &str);
}

/// The Keychain on macOS and the Credential Manager on Windows. Elsewhere
/// nothing is stored, and the key stays in the settings file.
pub struct SystemSecrets;

const KEYCHAIN_SERVICE: &str = "se.simq.SimQ";

#[cfg(any(target_os = "macos", windows))]
impl Secrets for SystemSecrets {
    fn set(&self, account: &str, secret: &str) -> bool {
        keyring::Entry::new(KEYCHAIN_SERVICE, account)
            .and_then(|entry| entry.set_password(secret))
            .is_ok()
    }
    fn get(&self, account: &str) -> Option<String> {
        keyring::Entry::new(KEYCHAIN_SERVICE, account)
            .and_then(|entry| entry.get_password())
            .ok()
    }
    fn delete(&self, account: &str) {
        if let Ok(entry) = keyring::Entry::new(KEYCHAIN_SERVICE, account) {
            let _ = entry.delete_credential();
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
impl Secrets for SystemSecrets {
    fn set(&self, _account: &str, _secret: &str) -> bool {
        false
    }
    fn get(&self, _account: &str) -> Option<String> {
        None
    }
    fn delete(&self, _account: &str) {}
}

/// Never touches the real password store from the tests.
#[cfg(test)]
fn system_secrets() -> &'static dyn Secrets {
    &tests::NoSecrets
}

#[cfg(not(test))]
fn system_secrets() -> &'static dyn Secrets {
    &SystemSecrets
}

/// One entry per installation, so that several instances (`--config-dir`)
/// can have different keys.
fn keychain_account(config: &Config) -> String {
    format!("team-key-{}", config.client_id)
}

/// The key last written to or read from the password store, so that saving
/// the settings again (e.g. a new theme) does not touch the store, which can
/// make macOS ask for permission.
static STORED_KEY: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

fn remember_stored(account: &str, key: Option<&str>) {
    *STORED_KEY.lock().unwrap_or_else(|e| e.into_inner()) =
        key.map(|key| (account.to_string(), key.to_string()));
}

fn is_stored(account: &str, key: &str) -> bool {
    STORED_KEY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|(a, k)| a == account && k == key)
}

/// Reads the configuration. If it is missing (first run) or cannot be
/// parsed, a new one is created with a new client_id.
pub fn load_or_init(paths: &Paths) -> Result<Config, ConfigError> {
    load_with(paths, system_secrets())
}

pub fn load_with(paths: &Paths, secrets: &dyn Secrets) -> Result<Config, ConfigError> {
    if let Ok(text) = std::fs::read_to_string(&paths.config_file) {
        if let Ok(mut config) = serde_json::from_str::<Config>(&text) {
            if std::mem::take(&mut config.token_in_keychain) {
                let account = keychain_account(&config);
                if let Some(github) = &mut config.github {
                    // If the store does not answer, the key is empty and
                    // GitHub denies access: the user pastes it again.
                    github.token = secrets.get(&account).unwrap_or_default();
                    if !github.token.is_empty() {
                        remember_stored(&account, Some(&github.token));
                    }
                }
            } else if config.github.is_some() {
                // A key in plain text from an older version moves to the store.
                let _ = save_with(paths, &config, secrets);
            }
            return Ok(config);
        }
    }
    let legacy = paths
        .legacy_config_file
        .as_ref()
        .and_then(|file| std::fs::read_to_string(file).ok())
        .and_then(|text| serde_json::from_str::<Config>(&text).ok());
    let config = legacy.unwrap_or_else(Config::fresh);
    save(paths, &config)?;
    Ok(config)
}

/// Saves the configuration. The team key goes to the system's password store
/// when there is one, and is only written to the file if that fails.
pub fn save(paths: &Paths, config: &Config) -> Result<(), ConfigError> {
    save_with(paths, config, system_secrets())
}

pub fn save_with(paths: &Paths, config: &Config, secrets: &dyn Secrets) -> Result<(), ConfigError> {
    if let Some(parent) = paths.config_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let account = keychain_account(config);
    let mut on_disk = config.clone();
    on_disk.token_in_keychain = false;
    match &mut on_disk.github {
        Some(github) if !github.token.is_empty() => {
            let stored = is_stored(&account, &github.token)
                || (secrets.set(&account, &github.token) && {
                    remember_stored(&account, Some(&github.token));
                    true
                });
            if stored {
                github.token.clear();
                on_disk.token_in_keychain = true;
            }
        }
        Some(_) => {}
        None => {
            // No GitHub any more: the old key is removed from the store.
            if STORED_KEY
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|(a, _)| *a == account)
            {
                secrets.delete(&account);
                remember_stored(&account, None);
            }
        }
    }
    let text = serde_json::to_string_pretty(&on_disk)?;
    let tmp = paths.config_file.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &paths.config_file)?;
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CliArgs {
    pub config_dir: Option<PathBuf>,
    pub help: bool,
    pub version: bool,
}

/// Parses the command line. Unknown arguments are ignored (the operating system
/// may add its own when the app is started from the desktop).
pub fn parse_args<I: Iterator<Item = String>>(mut args: I) -> Result<CliArgs, String> {
    let mut out = CliArgs::default();
    while let Some(arg) = args.next() {
        if arg == "--config-dir" {
            let value = args.next().ok_or_else(|| {
                t(
                    "--config-dir kräver en sökväg",
                    "--config-dir requires a path",
                )
                .to_string()
            })?;
            out.config_dir = Some(PathBuf::from(value));
        } else if let Some(value) = arg.strip_prefix("--config-dir=") {
            if value.is_empty() {
                return Err(t(
                    "--config-dir kräver en sökväg",
                    "--config-dir requires a path",
                )
                .to_string());
            }
            out.config_dir = Some(PathBuf::from(value));
        } else if arg == "--help" || arg == "-h" {
            out.help = true;
        } else if arg == "--version" || arg == "-V" {
            out.version = true;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A password store that stores nothing, like the one on Linux.
    pub(super) struct NoSecrets;

    impl Secrets for NoSecrets {
        fn set(&self, _account: &str, _secret: &str) -> bool {
            false
        }
        fn get(&self, _account: &str) -> Option<String> {
            None
        }
        fn delete(&self, _account: &str) {}
    }

    /// A password store in memory.
    #[derive(Default)]
    struct FakeSecrets(RefCell<HashMap<String, String>>);

    impl Secrets for FakeSecrets {
        fn set(&self, account: &str, secret: &str) -> bool {
            self.0
                .borrow_mut()
                .insert(account.to_string(), secret.to_string());
            true
        }
        fn get(&self, account: &str) -> Option<String> {
            self.0.borrow().get(account).cloned()
        }
        fn delete(&self, account: &str) {
            self.0.borrow_mut().remove(account);
        }
    }

    #[test]
    fn the_team_key_is_kept_out_of_the_settings_file() {
        let tmp = TempDir::new();
        let paths = resolve_paths(Some(tmp.path())).unwrap();
        let secrets = FakeSecrets::default();
        let mut config = load_with(&paths, &secrets).unwrap();
        config.display_name = "Åsa".to_string();
        config.github = Some(GithubConfig {
            repo: "lag/simq".to_string(),
            token: "github_pat_hemlig".to_string(),
            branch: "main".to_string(),
        });
        save_with(&paths, &config, &secrets).unwrap();
        let text = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(!text.contains("github_pat_hemlig"));
        assert!(text.contains("token_in_keychain"));
        let account = keychain_account(&config);
        assert_eq!(secrets.get(&account).as_deref(), Some("github_pat_hemlig"));
        // Read back, the key is where it always was in memory.
        assert_eq!(load_with(&paths, &secrets).unwrap(), config);

        // Without a password store it stays in the file, as before.
        save_with(&paths, &config, &NoSecrets).unwrap();
        // (The key was stored a moment ago, so nothing new is written.)
        let other = Config {
            client_id: Uuid::new_v4(),
            ..config.clone()
        };
        save_with(&paths, &other, &NoSecrets).unwrap();
        let text = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(text.contains("github_pat_hemlig"));
        assert!(!text.contains("token_in_keychain"));

        // A key in plain text from an older version moves to the store when read.
        let moved = load_with(&paths, &secrets).unwrap();
        assert_eq!(moved, other);
        let text = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(!text.contains("github_pat_hemlig"));
        assert_eq!(
            secrets.get(&keychain_account(&other)).as_deref(),
            Some("github_pat_hemlig")
        );

        // Switching to a shared folder removes the key from the store.
        let folder = Config {
            github: None,
            sync_folder: Some(tmp.path().to_path_buf()),
            ..other.clone()
        };
        save_with(&paths, &folder, &secrets).unwrap();
        assert_eq!(secrets.get(&keychain_account(&other)), None);

        // A store that has lost the key gives an empty key, not an error.
        save_with(&paths, &other, &secrets).unwrap();
        secrets.delete(&keychain_account(&other));
        let lost = load_with(&paths, &secrets).unwrap();
        assert_eq!(lost.github.unwrap().token, "");
    }

    fn args(list: &[&str]) -> Result<CliArgs, String> {
        parse_args(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn cli_arguments() {
        assert_eq!(args(&[]).unwrap(), CliArgs::default());
        assert_eq!(
            args(&["--config-dir", "/tmp/a"]).unwrap().config_dir,
            Some(PathBuf::from("/tmp/a"))
        );
        assert_eq!(
            args(&["--config-dir=/tmp/b"]).unwrap().config_dir,
            Some(PathBuf::from("/tmp/b"))
        );
        assert!(args(&["--config-dir"]).is_err());
        assert!(args(&["--okänd", "-psn_0_1"]).is_ok());
    }

    #[test]
    fn config_round_trip_keeps_client_id() {
        let tmp = TempDir::new();
        let paths = resolve_paths(Some(tmp.path())).unwrap();
        let mut config = load_or_init(&paths).unwrap();
        assert!(!config.is_complete());
        config.display_name = "Åsa".to_string();
        config.sync_folder = Some(tmp.path().join("SimQ åäö"));
        save(&paths, &config).unwrap();
        let again = load_or_init(&paths).unwrap();
        assert_eq!(again, config);
        assert!(again.is_complete());
        assert_eq!(again.language, Lang::of_computer());
    }

    #[test]
    fn language_is_saved_and_old_configs_still_load() {
        let tmp = TempDir::new();
        let paths = resolve_paths(Some(tmp.path())).unwrap();
        let mut config = load_or_init(&paths).unwrap();
        config.language = Lang::En;
        save(&paths, &config).unwrap();
        assert_eq!(load_or_init(&paths).unwrap().language, Lang::En);

        // A configuration from before the language choice lacks the field: Swedish applies.
        let old = format!(
            "{{\"display_name\": \"Bo\", \"client_id\": \"{}\"}}",
            config.client_id
        );
        std::fs::write(&paths.config_file, old).unwrap();
        let loaded = load_or_init(&paths).unwrap();
        assert_eq!(loaded.language, Lang::Sv);
        // The same goes for light or dark mode: it follows the computer.
        assert_eq!(loaded.theme, ThemeChoice::System);
        let mut config = loaded.clone();
        config.theme = ThemeChoice::Dark;
        save(&paths, &config).unwrap();
        assert_eq!(load_or_init(&paths).unwrap().theme, ThemeChoice::Dark);
        assert_eq!(
            serde_json::from_str::<ThemeChoice>("\"sepia\"").unwrap(),
            ThemeChoice::System
        );
        assert_eq!(loaded.client_id, config.client_id);
    }

    #[test]
    fn the_last_seen_version_is_noted() {
        let tmp = TempDir::new();
        let paths = resolve_paths(Some(tmp.path())).unwrap();
        // First run: nothing to tell, but the version is noted.
        let mut config = load_or_init(&paths).unwrap();
        assert_eq!(take_last_seen_version(&mut config, "0.6.0"), None);
        assert_eq!(config.last_seen_version.as_deref(), Some("0.6.0"));
        // Settings from before the field existed count as 0.5.2.
        config.last_seen_version = None;
        config.display_name = "Åsa".to_string();
        config.sync_folder = Some(tmp.path().to_path_buf());
        assert_eq!(
            take_last_seen_version(&mut config, "0.6.0").as_deref(),
            Some("0.5.2")
        );
        save(&paths, &config).unwrap();
        let mut again = load_or_init(&paths).unwrap();
        assert_eq!(
            take_last_seen_version(&mut again, "0.6.1").as_deref(),
            Some("0.6.0")
        );
    }

    #[test]
    fn settings_from_the_old_app_name_are_taken_over() {
        let old_dir = TempDir::new();
        let new_dir = TempDir::new();
        let old_paths = resolve_paths(Some(old_dir.path())).unwrap();
        let mut old = load_or_init(&old_paths).unwrap();
        old.display_name = "Åsa".to_string();
        save(&old_paths, &old).unwrap();

        let mut paths = resolve_paths(Some(new_dir.path())).unwrap();
        paths.legacy_config_file = Some(old_paths.config_file.clone());
        let taken_over = load_or_init(&paths).unwrap();
        assert_eq!(taken_over, old);
        assert!(paths.config_file.is_file());
    }
}
