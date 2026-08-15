//! Provider discovery and construction.
//!
//! Specs come from two places: those compiled into the binary, and those in the
//! user's `providers.d`. A user file with the same `id` **shadows** the built-in,
//! so a broken selector can be fixed locally without waiting for a release.

use crate::declarative::DeclarativeProvider;
use crate::spec::ProviderSpec;
use kuro_core::{Provider, ProviderError};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, warn};

/// Specs shipped with the binary.
const BUILTIN_SPECS: &[(&str, &str)] = &[
    (
        "luciferdonghua",
        include_str!("../../../providers.d/luciferdonghua.toml"),
    ),
    (
        "donghuastream",
        include_str!("../../../providers.d/donghuastream.toml"),
    ),
    ("anidb", include_str!("../../../providers.d/anidb.toml")),
];

pub struct LoadedProvider {
    pub provider: Arc<dyn Provider>,
    /// True when this spec came from the user's `providers.d` rather than the binary.
    pub from_user_dir: bool,
    /// True when the spec asks to be fetched through the external client.
    ///
    /// Kept here because the spec itself is consumed at load time, and `doctor`
    /// needs to know whether the external binary is worth mentioning at all.
    pub needs_impersonation: bool,
}

#[derive(Default)]
pub struct Registry {
    providers: BTreeMap<String, LoadedProvider>,
    /// Specs that failed to load, kept so `kuro provider list` can show *why* a
    /// provider is missing instead of silently omitting it.
    pub errors: Vec<(String, ProviderError)>,
}

impl Registry {
    /// Load built-in specs, then overlay any user specs found in `user_dir`.
    pub fn load(user_dir: Option<&Path>) -> Self {
        let mut registry = Self::default();

        for (id, text) in BUILTIN_SPECS {
            match DeclarativeProvider::from_toml(text) {
                Ok(p) => {
                    registry.providers.insert(
                        p.spec().id.clone(),
                        LoadedProvider {
                            needs_impersonation: p.spec().request.impersonate,
                            provider: Arc::new(p),
                            from_user_dir: false,
                        },
                    );
                }
                Err(e) => {
                    // A built-in that fails to parse is a bug in this crate, not
                    // user error, but it must not prevent the app from starting.
                    warn!(provider = id, error = %e, "built-in provider spec is invalid");
                    registry.errors.push(((*id).to_string(), e));
                }
            }
        }

        if let Some(dir) = user_dir {
            registry.overlay_user_dir(dir);
        }

        registry
    }

    fn overlay_user_dir(&mut self, dir: &Path) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            // No user directory is the normal case, not a problem.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "cannot read user providers directory");
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }

            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "cannot read provider spec");
                    continue;
                }
            };

            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("<unknown>")
                .to_string();

            match DeclarativeProvider::from_toml(&text) {
                Ok(p) => {
                    debug!(provider = %p.spec().id, path = %path.display(), "loaded user provider");
                    self.providers.insert(
                        p.spec().id.clone(),
                        LoadedProvider {
                            needs_impersonation: p.spec().request.impersonate,
                            provider: Arc::new(p),
                            from_user_dir: true,
                        },
                    );
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "invalid user provider spec");
                    self.errors.push((name, e));
                }
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(id).map(|l| Arc::clone(&l.provider))
    }

    pub fn is_user_override(&self, id: &str) -> bool {
        self.providers
            .get(id)
            .map(|l| l.from_user_dir)
            .unwrap_or(false)
    }

    /// Ids of providers that can only be reached through the external client.
    pub fn impersonating_ids(&self) -> impl Iterator<Item = &str> {
        self.providers
            .iter()
            .filter(|(_, loaded)| loaded.needs_impersonation)
            .map(|(id, _)| id.as_str())
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(|s| s.as_str())
    }

    pub fn all(&self) -> Vec<Arc<dyn Provider>> {
        self.providers
            .values()
            .map(|l| Arc::clone(&l.provider))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

/// Parse every built-in spec, returning the first failure.
///
/// Used by the test suite to guarantee a malformed shipped spec cannot be released.
pub fn validate_builtins() -> Result<usize, (String, ProviderError)> {
    for (id, text) in BUILTIN_SPECS {
        ProviderSpec::from_toml(text)
            .map_err(|e| {
                (
                    (*id).to_string(),
                    ProviderError::Config(format!("invalid spec: {e}")),
                )
            })
            .and_then(|spec| DeclarativeProvider::new(spec).map_err(|e| ((*id).to_string(), e)))?;
    }
    Ok(BUILTIN_SPECS.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_spec_parses() {
        let count = validate_builtins().expect("all built-in specs must be valid");
        assert!(
            count > 0,
            "at least one provider should ship with the binary"
        );
    }

    #[test]
    fn registry_loads_builtins_without_a_user_dir() {
        let registry = Registry::load(None);
        assert!(registry.errors.is_empty(), "errors: {:?}", registry.errors);
        assert!(registry.get("luciferdonghua").is_some());
        assert!(registry.get("donghuastream").is_some());
        assert!(!registry.is_user_override("luciferdonghua"));
    }
}
