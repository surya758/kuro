//! XDG-correct on-disk locations, plus atomic write support.

use directories::ProjectDirs;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not determine a home directory for the current user")]
    NoHome,

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to parse {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to serialise config: {0}")]
    TomlSer(#[from] toml::ser::Error),
}

pub struct Paths {
    dirs: ProjectDirs,
}

impl Paths {
    pub fn discover() -> Result<Self, StoreError> {
        let dirs = ProjectDirs::from("", "", "kuro").ok_or(StoreError::NoHome)?;
        Ok(Self { dirs })
    }

    pub fn config_file(&self) -> PathBuf {
        self.dirs.config_dir().join("config.toml")
    }

    /// User selector overrides. Files here shadow the providers shipped with the
    /// binary, so a broken selector can be fixed without touching the install.
    pub fn user_providers_dir(&self) -> PathBuf {
        self.dirs.config_dir().join("providers.d")
    }

    pub fn history_file(&self) -> PathBuf {
        self.dirs.data_dir().join("history.json")
    }

    pub fn bookmarks_file(&self) -> PathBuf {
        self.dirs.data_dir().join("bookmarks.json")
    }

    pub fn health_file(&self) -> PathBuf {
        self.dirs.data_dir().join("health.json")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.dirs.cache_dir().to_path_buf()
    }

    pub fn ensure_dirs(&self) -> Result<(), StoreError> {
        for dir in [
            self.dirs.config_dir(),
            self.dirs.data_dir(),
            self.dirs.cache_dir(),
        ] {
            std::fs::create_dir_all(dir).map_err(|source| StoreError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        Ok(())
    }
}

/// Write via a temporary file plus rename, so a crash mid-write cannot leave a
/// truncated history or health file behind.
pub fn write_atomic(path: &std::path::Path, contents: &str) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let tmp = path.with_extension(format!("tmp{}", std::process::id()));

    std::fs::write(&tmp, contents).map_err(|source| StoreError::Io {
        path: tmp.clone(),
        source,
    })?;

    std::fs::rename(&tmp, path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}
