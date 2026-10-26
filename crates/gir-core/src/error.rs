use std::path::PathBuf;
use thiserror::Error;

/// Core error type for all `gir` repository, storage, object, and index operations.
#[derive(Debug, Error)]
pub enum GirError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("not a git repository (or any of the parent directories): .git")]
    NotARepository,

    #[error("invalid object ID hex string '{0}': expected 40 hexadecimal characters")]
    InvalidObjectId(String),

    #[error("invalid object ID prefix '{0}': must be between 4 and 40 hexadecimal characters")]
    InvalidObjectPrefix(String),

    #[error("object not found: {0}")]
    ObjectNotFound(String),

    #[error("ambiguous object prefix '{prefix}': matches {count} objects")]
    AmbiguousObjectPrefix { prefix: String, count: usize },

    #[error("corrupt git object {id}: {reason}")]
    CorruptObject { id: String, reason: String },

    #[error("corrupt packfile or index '{path}': {reason}")]
    CorruptPack { path: PathBuf, reason: String },

    #[error("object hash mismatch: expected {expected}, computed {actual}")]
    HashMismatch { expected: String, actual: String },

    #[error("unexpected object type: expected {expected}, found {found}")]
    UnexpectedObjectType { expected: String, found: String },

    #[error("unable to create '{path}.lock': file exists (another git/gir process may be active)")]
    LockContention { path: PathBuf },

    #[error("configuration error: {0}")]
    Config(String),

    #[error("missing user identity: please set user.name and user.email via `gir config`")]
    MissingIdentity,

    #[error("{0}")]
    InvalidArgument(String),
}
