use std::fmt;

/// One error type for the whole program. Worker code returns this and never panics
/// (release builds use panic = "abort").
#[derive(Debug, Clone)]
pub enum Error {
    Io(String),
    Json(String),
    Net(String),
    /// Bad or unsupported metadata.
    Metadata(String),
    /// A module, instance, or file that does not exist.
    NotFound(String),
    /// Registry is locked by another process.
    Locked(String),
    /// Relationship problems found by the resolver.
    Resolve(crate::resolve::ResolveError),
    /// A file conflict or registry inconsistency during install or remove.
    Conflict(String),
    /// Download hash or size does not match.
    Hash(String),
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(s) => write!(f, "I/O error: {s}"),
            Error::Json(s) => write!(f, "JSON error: {s}"),
            Error::Net(s) => write!(f, "Network error: {s}"),
            Error::Metadata(s) => write!(f, "Bad metadata: {s}"),
            Error::NotFound(s) => write!(f, "Not found: {s}"),
            Error::Locked(s) => write!(f, "Locked: {s}"),
            Error::Resolve(e) => write!(f, "{e}"),
            Error::Conflict(s) => write!(f, "Conflict: {s}"),
            Error::Hash(s) => write!(f, "Download check failed: {s}"),
            Error::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e.to_string())
    }
}

impl From<crate::resolve::ResolveError> for Error {
    fn from(e: crate::resolve::ResolveError) -> Self {
        Error::Resolve(e)
    }
}

/// Add context to an I/O error: `fs::read(p).ctx(p)?`.
pub trait Ctx<T> {
    fn ctx(self, what: impl fmt::Display) -> Result<T>;
}

impl<T> Ctx<T> for std::result::Result<T, std::io::Error> {
    fn ctx(self, what: impl fmt::Display) -> Result<T> {
        self.map_err(|e| Error::Io(format!("{what}: {e}")))
    }
}
