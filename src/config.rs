//! Local configuration and identity. Lives in the OS configuration folder,
//! never in the shared folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::github::GithubConfig;
use crate::i18n::{t, Lang};

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
    pub client_id: Uuid,
}

impl Config {
    fn fresh() -> Self {
        Config {
            sync_folder: None,
            github: None,
            display_name: String::new(),
            language: Lang::default(),
            client_id: Uuid::new_v4(),
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

/// Reads the configuration. If it is missing (first run) or cannot be
/// parsed, a new one is created with a new client_id.
pub fn load_or_init(paths: &Paths) -> Result<Config, ConfigError> {
    if let Ok(text) = std::fs::read_to_string(&paths.config_file) {
        if let Ok(config) = serde_json::from_str::<Config>(&text) {
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

pub fn save(paths: &Paths, config: &Config) -> Result<(), ConfigError> {
    if let Some(parent) = paths.config_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(config)?;
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
        assert_eq!(again.language, Lang::Sv);
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
        assert_eq!(loaded.client_id, config.client_id);
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
