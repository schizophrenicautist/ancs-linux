use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub adapter: Option<String>,
    pub name: Option<String>,
    pub timeout_ms: i32,
    pub app_store_icons: bool,
    pub app_store_country: Option<String>,
    pub show_preexisting: bool,
    pub dismiss_sync: bool,
    pub copy_codes: bool,
    pub ignore_apps: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            adapter: None,
            name: None,
            timeout_ms: 5000,
            app_store_icons: true,
            app_store_country: None,
            show_preexisting: false,
            dismiss_sync: true,
            copy_codes: true,
            ignore_apps: Vec::new(),
        }
    }
}

pub fn default_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
        .join("ancs-linux")
        .join("config.toml")
}

pub fn load(path: &Path, required: bool) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("invalid config {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && !required => Ok(Config::default()),
        Err(err) => Err(err).with_context(|| format!("cannot read config {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_defaults() {
        assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
    }

    #[test]
    fn parses_shipped_config() {
        let config: Config = toml::from_str(include_str!("../packaging/config.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("timeout = 5").is_err());
    }
}
