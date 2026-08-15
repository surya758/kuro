//! Shared application context: config, providers, resolver, player.

use anyhow::{Context, Result};
use kuro_core::{FetchConfig, FetchCtx, Provider, ProviderId, QualityPref};
use kuro_player::{IinaPlayer, MpvPlayer, Player};
use kuro_providers::Registry;
use kuro_resolver::ResolverChain;
use kuro_store::{Config, HealthStore, HealthTransition, History, Paths};
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
    /// `--select-nth`: pick the Nth search result rather than the best match.
    pub select_nth: Option<usize>,
    /// `--skip`: jump over openings and endings when AniSkip has data.
    pub skip: bool,
    pub json: bool,
    pub dry_run: bool,
}

impl App {
    pub fn new(
        provider_filter: Option<String>,
        quality_override: Option<QualityPref>,
        select_nth: Option<usize>,
        skip: bool,
        json: bool,
        dry_run: bool,
        no_cache: bool,
    ) -> Result<Self> {
        let paths = Paths::discover().context("locating config directory")?;
        paths.ensure_dirs().context("creating config directories")?;

        let config = Config::load(&paths).context("loading config")?;
        let health = HealthStore::load(&paths).context("loading provider health")?;
        let registry = Registry::load(Some(&paths.user_providers_dir()));

        let caching_on = config.general.cache && !no_cache;
        let ctx = FetchCtx::new(FetchConfig {
            timeout: config.general.request_timeout,
            cache_dir: caching_on.then(|| paths.cache_dir()),
            impersonate_command: Some(config.general.impersonate_command.clone()),
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
            select_nth,
            skip,
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
        self.health
            .save(&self.paths)
            .context("saving provider health")
    }

    pub fn save_config(&self) -> Result<()> {
        self.config.save(&self.paths).context("saving config")
    }

    /// The configured playback backend.
    ///
    /// Boxed because the choice is only known at runtime: IINA suits everyday
    /// watching, while some sources play only in mpv.
    pub fn player(&self) -> Result<Box<dyn Player>> {
        let path = self.config.player.path.as_deref();
        let backend = self.config.player.backend.to_ascii_lowercase();

        let found: Result<Box<dyn Player>, kuro_core::PlayerError> = match backend.as_str() {
            "mpv" => MpvPlayer::discover(path).map(|p| Box::new(p) as Box<dyn Player>),
            _ => IinaPlayer::discover(path).map(|p| Box::new(p) as Box<dyn Player>),
        };

        found.map_err(|e| {
            let hint = if backend == "mpv" {
                "Install it with `brew install mpv`"
            } else {
                "Install IINA from https://iina.io"
            };
            anyhow::anyhow!(
                "{e}\n\n{hint}, or set `player.path` in {}",
                self.paths.config_file().display()
            )
        })
    }
}

/// The player to use for a particular stream.
///
/// Sources with no muxed rendition are resolved by the player itself, and IINA
/// cannot do it — its bundled extractor is too old for these hosts and it buffers
/// them badly. mpv handles them, so it is used for those alone; everything else
/// stays on the configured backend, which is the nicer thing to watch in.
pub async fn require_player_for(app: &App, stream: &kuro_core::Stream) -> Result<Box<dyn Player>> {
    if stream.ytdl_format.is_some() && !app.config.player.backend.eq_ignore_ascii_case("mpv") {
        match MpvPlayer::discover(None) {
            Ok(mpv) if mpv.is_available().await => {
                tracing::debug!("source needs player-side resolution; using mpv");
                return Ok(Box::new(mpv));
            }
            // Falling through is honest: the configured player at least tries,
            // and its failure is more informative than one about a missing mpv.
            _ => tracing::warn!(
                "this source plays best in mpv, which was not found — install it with `brew install mpv`"
            ),
        }
    }
    require_player(app).await
}

/// Ensure a player binary is actually usable before a command relies on it.
pub async fn require_player(app: &App) -> Result<Box<dyn Player>> {
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
