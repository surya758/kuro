//! Provider health tracking and automatic disabling.
//!
//! This is the mechanism that makes "anime sites go down all the time" a non-event:
//! a provider that fails repeatedly removes itself from searches, and quietly puts
//! itself back once the site recovers.

use crate::paths::{write_atomic, Paths, StoreError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderHealth {
    pub consecutive_failures: u32,
    pub auto_disabled: bool,
    pub disabled_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_success: Option<DateTime<Utc>>,
    /// Set when an auto-disabled provider has been re-enabled by a passing probe,
    /// so the CLI can report the recovery once and then clear it.
    pub last_checked: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthStore {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderHealth>,
}

/// What changed as a result of recording an outcome, so the caller can notify the
/// user exactly once instead of on every run.
#[derive(Debug, PartialEq, Eq)]
pub enum HealthTransition {
    Unchanged,
    JustDisabled,
    Recovered,
}

impl HealthStore {
    pub fn load(paths: &Paths) -> Result<Self, StoreError> {
        let path = paths.health_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|source| StoreError::Json { path, source })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }

    pub fn save(&self, paths: &Paths) -> Result<(), StoreError> {
        let text = serde_json::to_string_pretty(self)
            .expect("HealthStore is always serialisable");
        write_atomic(&paths.health_file(), &text)
    }

    pub fn get(&self, id: &str) -> ProviderHealth {
        self.providers.get(id).cloned().unwrap_or_default()
    }

    pub fn record_success(&mut self, id: &str) -> HealthTransition {
        let entry = self.providers.entry(id.to_string()).or_default();
        let was_disabled = entry.auto_disabled;

        entry.consecutive_failures = 0;
        entry.auto_disabled = false;
        entry.disabled_at = None;
        entry.last_error = None;
        entry.last_success = Some(Utc::now());
        entry.last_checked = Some(Utc::now());

        if was_disabled {
            HealthTransition::Recovered
        } else {
            HealthTransition::Unchanged
        }
    }

    pub fn record_failure(
        &mut self,
        id: &str,
        error: &str,
        threshold: u32,
    ) -> HealthTransition {
        let entry = self.providers.entry(id.to_string()).or_default();
        let was_disabled = entry.auto_disabled;

        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.last_error = Some(error.to_string());
        entry.last_checked = Some(Utc::now());

        if !was_disabled && entry.consecutive_failures >= threshold {
            entry.auto_disabled = true;
            entry.disabled_at = Some(Utc::now());
            HealthTransition::JustDisabled
        } else {
            HealthTransition::Unchanged
        }
    }

    /// Whether an auto-disabled provider has waited out `recheck_interval` and
    /// should be probed again. Providers that are not auto-disabled are always due.
    pub fn is_due_for_recheck(&self, id: &str, recheck_interval: Duration) -> bool {
        let health = self.get(id);
        if !health.auto_disabled {
            return true;
        }

        let Some(disabled_at) = health.disabled_at else {
            return true;
        };

        let elapsed = Utc::now().signed_duration_since(disabled_at);
        match elapsed.to_std() {
            Ok(elapsed) => elapsed >= recheck_interval,
            // Negative elapsed means the clock moved backwards; re-probe rather
            // than leaving the provider disabled forever.
            Err(_) => true,
        }
    }

    /// Providers that should participate in this run: enabled in config, and either
    /// healthy or due for a recheck.
    pub fn is_usable(&self, id: &str, recheck_interval: Duration) -> bool {
        let health = self.get(id);
        !health.auto_disabled || self.is_due_for_recheck(id, recheck_interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disables_only_once_threshold_is_reached() {
        let mut store = HealthStore::default();
        assert_eq!(store.record_failure("p", "boom", 3), HealthTransition::Unchanged);
        assert_eq!(store.record_failure("p", "boom", 3), HealthTransition::Unchanged);
        assert_eq!(store.record_failure("p", "boom", 3), HealthTransition::JustDisabled);
        assert!(store.get("p").auto_disabled);
    }

    #[test]
    fn disabling_is_announced_once_not_on_every_subsequent_failure() {
        let mut store = HealthStore::default();
        for _ in 0..3 {
            store.record_failure("p", "boom", 3);
        }
        assert_eq!(store.record_failure("p", "boom", 3), HealthTransition::Unchanged);
    }

    #[test]
    fn success_clears_failures_and_reports_recovery() {
        let mut store = HealthStore::default();
        for _ in 0..3 {
            store.record_failure("p", "boom", 3);
        }
        assert_eq!(store.record_success("p"), HealthTransition::Recovered);
        assert_eq!(store.get("p").consecutive_failures, 0);
        assert!(!store.get("p").auto_disabled);
        // A second success is not another recovery.
        assert_eq!(store.record_success("p"), HealthTransition::Unchanged);
    }

    #[test]
    fn freshly_disabled_provider_is_not_immediately_rechecked() {
        let mut store = HealthStore::default();
        for _ in 0..3 {
            store.record_failure("p", "boom", 3);
        }
        assert!(!store.is_due_for_recheck("p", Duration::from_secs(1800)));
        // ...but a zero interval means always re-probe.
        assert!(store.is_due_for_recheck("p", Duration::ZERO));
    }

    #[test]
    fn unknown_provider_is_usable() {
        let store = HealthStore::default();
        assert!(store.is_usable("never-seen", Duration::from_secs(1800)));
    }
}
