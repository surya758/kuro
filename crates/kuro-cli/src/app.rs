//! Shared application context: config, providers, resolver, player.

use anyhow::{Context, Result};
use kuro_core::{FetchConfig, FetchCtx, Provider, ProviderId, QualityPref};
use kuro_player::{IinaPlayer, Player};
use kuro_providers::Registry;
use kuro_resolver::ResolverChain;
use kuro_store::{Config, HealthStore, History, HealthTransition, Paths};
use std::sync::Arc;

pub struct App {
    pub paths: Paths,
    pub config: Config,
    pub health: HealthStore,
    pub registry: Registry,
    pub ctx: FetchCtx,
    pub resolver: ResolverChain,
    pub quality: QualityPref,
    pub provider_filter: Option<String>,
    pub json: bool,
    pub dry_run: bool,
}

impl App {
    pub fn new(
        provider_filter: Option<String>,
        quality_override: Option<QualityPref>,
        json: bool,
        dry_run: bool,
    ) -> Result<Self> {
        let paths = Paths::discover().context("locating config directory")?;
        paths.ensure_dirs().context("creating config directories")?;

        let config = Config::load(&paths).context("loading config")?;
        let health = HealthStore::load(&paths).context("loading provider health")?;
        let registry = Registry::load(Some(&paths.user_providers_dir()));

        let ctx = FetchCtx::new(FetchConfig {
            timeout: config.general.request_timeout,
            ..FetchConfig::default()
        })
        .context("building HTTP client")?;

        let quality = quality_override.unwrap_or(config.general.default_quality);

        Ok(Self {
            paths,
            config,
            health,
            registry,
            ctx,
            resolver: ResolverChain::default(),
            quality,
            provider_filter,
            json,
            dry_run,
        })
    }

    /// Providers that should take part in this run.
    ///
    /// A provider participates when it is enabled in config, not auto-disabled
    /// (or due for a recheck), and matches `--provider` if that was given.
    pub fn active_providers(&self) -> Vec<Arc<dyn Provider>> {
        let recheck = self.config.health.recheck_interval;

        self.registry
            .all()
            .into_iter()
            .filter(|p| {
                let id = p.id();
                let id = id.as_str();

                if let Some(filter) = &self.provider_filter {
                    if id != filter {
                        return false;
                    }
                }

                self.config.is_enabled(id) && self.health.is_usable(id, recheck)
            })
            .collect()
    }

    pub fn provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.registry.get(id)
    }

    pub fn history(&self) -> Result<History> {
        History::load(&self.paths).context("loading watch history")
    }

    /// Record a provider success and report a recovery, if this ends an outage.
    pub fn note_success(&mut self, id: &ProviderId) {
        if self.health.record_success(id.as_str()) == HealthTransition::Recovered {
            eprintln!(
                "✓ {} recovered and was re-enabled automatically.",
                id.as_str()
            );
        }
    }

    /// Record a provider failure, announcing an auto-disable exactly once.
    pub fn note_failure(&mut self, id: &ProviderId, error: &str) {
        let threshold = self.config.health.auto_disable_after_failures;
        if self.health.record_failure(id.as_str(), error, threshold)
            == HealthTransition::JustDisabled
        {
            eprintln!(
                "⚠  {id} auto-disabled after {threshold} consecutive failures (last: {error}).\n\
                    Re-enable with: kuro provider enable {id}",
                id = id.as_str()
            );
        }
    }

    pub fn save_health(&self) -> Result<()> {
        self.health.save(&self.paths).context("saving provider health")
    }

    pub fn save_config(&self) -> Result<()> {
        self.config.save(&self.paths).context("saving config")
    }

    pub fn player(&self) -> Result<IinaPlayer> {
        IinaPlayer::discover(self.config.player.path.as_deref()).map_err(|e| {
            anyhow::anyhow!(
                "{e}\n\nInstall IINA from https://iina.io, or set `player.path` in {}",
                self.paths.config_file().display()
            )
        })
    }
}

/// Ensure a player binary is actually usable before a command relies on it.
pub async fn require_player(app: &App) -> Result<IinaPlayer> {
    let player = app.player()?;
    if !player.is_available().await {
        anyhow::bail!(
            "player `{}` was found at {} but is not runnable",
            player.name(),
            player.binary().display()
        );
    }
    Ok(player)
}
