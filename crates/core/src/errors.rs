//! Shepherd error type shared across crates.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// A provider was asked for an operation it does not implement (e.g. suspend).
    #[error("provider \"{provider}\" does not support operation \"{op}\"")]
    NotSupported { provider: String, op: String },

    /// A referenced sandbox no longer exists.
    #[error("sandbox not found: {0}")]
    SandboxNotFound(String),

    /// A command run inside a sandbox exited non zero.
    #[error("command failed (exit {exit_code}): {cmd}\n{stderr}")]
    ExecFailed {
        cmd: String,
        exit_code: i64,
        stderr: String,
    },

    /// Wraps any lower level error (provider SDKs, io, etc).
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
