//! Error types for `mlua-eris`.
//!
//! All operations either return mlua errors (propagated from Lua) or
//! variants specific to persistence (perms registration conflicts,
//! invalid blobs, etc.).

use std::fmt;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur when persisting or unpersisting Lua state.
#[derive(Debug)]
pub enum Error {
    /// An error from the underlying mlua call (compile error, type
    /// mismatch, runtime exception, etc.).
    Lua(mlua::Error),
    /// An attempt to register the same perm key twice with different
    /// values. Perms keys are global to the registry and must be
    /// unique to keep save files unambiguous.
    DuplicatePermKey(String),
    /// `eris.persist` raised a Lua error. The string is Eris's error
    /// message, typically of the form
    /// `"bad permanent value (no value)"` or
    /// `"attempt to persist a light C function (0x...)"`.
    PersistFailed(String),
    /// `eris.unpersist` raised a Lua error. Same shape as the above.
    UnpersistFailed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Lua(e) => write!(f, "mlua error: {}", e),
            Error::DuplicatePermKey(k) => {
                write!(f, "perm key already registered: '{}'", k)
            }
            Error::PersistFailed(msg) => write!(f, "eris.persist failed: {}", msg),
            Error::UnpersistFailed(msg) => write!(f, "eris.unpersist failed: {}", msg),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Lua(e) => Some(e),
            _ => None,
        }
    }
}

impl From<mlua::Error> for Error {
    fn from(e: mlua::Error) -> Self {
        Error::Lua(e)
    }
}
