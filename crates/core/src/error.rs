use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid game definition '{id}': {message}")]
    InvalidGame { id: String, message: String },

    #[error("failed to parse game definition: {0}")]
    GameToml(#[from] toml::de::Error),

    #[error("unknown game '{0}'")]
    UnknownGame(String),

    #[error("{0}")]
    Archive(String),

    #[error("{0}")]
    Install(String),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("{0}")]
    Nexus(String),

    /// A network problem that may go away: worth trying again later.
    #[error("{0}")]
    Network(String),

    #[error("{0}")]
    Wine(String),

    #[error("{0} not found")]
    NotFound(String),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
