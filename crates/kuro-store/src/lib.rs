//! Local persistence: config, watch history, bookmarks, and provider health.
//!
//! Everything here is plain local files. `kuro` has no server component and no
//! account, so this crate is the entirety of its state.

pub mod config;
pub mod health;
pub mod history;
pub mod paths;

pub use config::{Config, General, HealthConfig, PlayerConfig, ProviderConfig};
pub use health::{HealthStore, HealthTransition, ProviderHealth};
pub use history::{Bookmark, Bookmarks, History, HistoryEntry};
pub use paths::{write_atomic, Paths, StoreError};
