//! User configuration (`~/.config/kuro/config.toml`).

use crate::paths::{write_atomic, Paths, StoreError};
use kuro_core::QualityPref;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub player: PlayerConfig,
    pub health: HealthConfig,
    /// Keyed by provider id. Providers with no entry use [`ProviderConfig::default`],
    /// so a newly shipped provider is enabled without the user editing anything.
    pub providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub default_quality: QualityPref,
    /// Max providers queried simultaneously.
    pub concurrency: usize,
    #[serde(with = "humantime_serde")]
    pub search_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
}

impl Default for General {
    fn default() -> Self {
        Self {
            default_quality: QualityPref::P1080,
            concurrency: 6,
            search_timeout: Duration::from_secs(8),
            request_timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerConfig {
    pub backend: String,
    /// Explicit path to the player binary; searched on `PATH` and in the IINA app
    /// bundle when unset.
    pub path: Option<String>,
    pub fullscreen: bool,
    /// Seek to the last known position when replaying an episode.
    pub resume: bool,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            backend: "iina".to_string(),
            path: None,
            fullscreen: false,
            resume: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HealthConfig {
    /// Consecutive failures before a provider is automatically disabled.
    pub auto_disable_after_failures: u32,
    /// How long an auto-disabled provider stays out before being re-probed.
    #[serde(with = "humantime_serde")]
    pub recheck_interval: Duration,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            auto_disable_after_failures: 3,
            recheck_interval: Duration::from_secs(30 * 60),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderConfig {
    pub enabled: bool,
    /// Higher wins when merging duplicate results across providers.
    pub priority: i32,
    /// Preferred embed hosts, best first. Empty means "try mirrors in page order".
    pub mirrors: Vec<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            priority: 0,
            mirrors: Vec::new(),
        }
    }
}

impl Config {
    /// Load config, returning defaults when the file does not exist yet.
    pub fn load(paths: &Paths) -> Result<Self, StoreError> {
        let path = paths.config_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|source| StoreError::Toml { path, source }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }

    pub fn save(&self, paths: &Paths) -> Result<(), StoreError> {
        let text = toml::to_string_pretty(self)?;
        write_atomic(&paths.config_file(), &text)
    }

    pub fn provider(&self, id: &str) -> ProviderConfig {
        self.providers.get(id).cloned().unwrap_or_default()
    }

    pub fn provider_mut(&mut self, id: &str) -> &mut ProviderConfig {
        self.providers.entry(id.to_string()).or_default()
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.provider(id).enabled
    }

    pub fn priority(&self, id: &str) -> i32 {
        self.provider(id).priority
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_providers_default_to_enabled() {
        let cfg = Config::default();
        assert!(cfg.is_enabled("a-provider-shipped-after-this-config-was-written"));
    }

    #[test]
    fn roundtrips_through_toml() {
        let mut cfg = Config::default();
        cfg.provider_mut("luciferdonghua").priority = 10;
        cfg.provider_mut("luciferdonghua").mirrors = vec!["rumble".into()];

        let text = toml::to_string_pretty(&cfg).expect("serialises");
        let back: Config = toml::from_str(&text).expect("deserialises");

        assert_eq!(back.priority("luciferdonghua"), 10);
        assert_eq!(back.provider("luciferdonghua").mirrors, vec!["rumble"]);
        assert_eq!(back.general.default_quality, QualityPref::P1080);
    }

    #[test]
    fn partial_config_fills_in_defaults() {
        let cfg: Config = toml::from_str("[general]\nconcurrency = 2\n").expect("parses");
        assert_eq!(cfg.general.concurrency, 2);
        // Untouched fields keep their defaults rather than becoming zero values.
        assert_eq!(cfg.general.default_quality, QualityPref::P1080);
        assert!(cfg.player.resume);
    }
}
