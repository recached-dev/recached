//! Errors the embedded cache can return.

use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The key is not covered by any live query, so the local store has no
    /// opinion about it.
    ///
    /// This is the error that keeps an embedded cache honest. A plain `None`
    /// here would be indistinguishable from "the key does not exist", and a
    /// service would serve a confidently wrong answer for every key it forgot
    /// to [`watch`](crate::Cache::watch). Either watch a pattern covering the
    /// key, or use [`get_or_fetch`](crate::Cache::get_or_fetch).
    NotHydrated { key: String },

    /// The key holds a collection but was read as a string (or vice versa).
    WrongType { key: String },

    /// The server replied with an error.
    Server(String),

    /// The socket is down. Writes are queued in the outbox and replayed on
    /// reconnect; reads that need the network cannot proceed.
    Disconnected,

    /// The connection task has shut down — the `Cache` handle is dead.
    Closed,

    /// A network round-trip did not complete in time.
    Timeout,

    /// The initial connection could not be established.
    Connect(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotHydrated { key } => write!(
                f,
                "key `{key}` is not covered by any live query \
                 (call `watch()` with a matching pattern, or use `get_or_fetch()`)"
            ),
            Error::WrongType { key } => write!(f, "key `{key}` holds a different type"),
            Error::Server(msg) => write!(f, "server error: {msg}"),
            Error::Disconnected => write!(f, "not connected to the recached server"),
            Error::Closed => write!(f, "cache connection task has shut down"),
            Error::Timeout => write!(f, "timed out waiting for the server"),
            Error::Connect(msg) => write!(f, "could not connect: {msg}"),
        }
    }
}

impl std::error::Error for Error {}
