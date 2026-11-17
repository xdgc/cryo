// vim: ts=4 et

//! A single error type for the whole program. Kept as a plain enum
//! rather than reaching for a crate like `anyhow`, in keeping with the
//! dependency-free starting point -- nothing here needs more than
//! "what happened" and a message.

use std::fmt;

#[derive(Debug)]
pub enum CryoError {
    Io(std::io::Error),

    /// A `zfs`/`zpool` subcommand exited nonzero; carries its stderr.
    Command(String),

    /// A property value or snapshot name didn't parse.
    Parse(String),
}

impl fmt::Display for CryoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CryoError::Io(err) => write!(f, "{err}"),
            CryoError::Command(msg) => write!(f, "{msg}"),
            CryoError::Parse(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CryoError {}

impl From<std::io::Error> for CryoError {
    fn from(err: std::io::Error) -> Self {
        CryoError::Io(err)
    }
}

pub type Result<T> = std::result::Result<T, CryoError>;
