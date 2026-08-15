//! Per-site scrapers.
//!
//! Most sites are handled by [`declarative::DeclarativeProvider`] plus a selector
//! TOML. Sites needing genuinely novel logic — a new obfuscation scheme, an auth
//! handshake — get their own module implementing `kuro_core::Provider` directly.

pub mod declarative;
pub mod hosts;
pub mod json;
pub mod parse;
pub mod registry;
pub mod spec;

pub use declarative::DeclarativeProvider;
pub use registry::{validate_builtins, LoadedProvider, Registry};
pub use spec::ProviderSpec;
