pub mod api;
pub mod cli;
pub mod daemon;
pub mod metadata;
pub mod module;
pub mod rtsyn_cli;
pub mod workspace;

pub const DEFAULT_API_BASE_URL: &str = "http://127.0.0.1:17190";

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Parse(String),
    Api(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(error) => write!(formatter, "{error}"),
            Error::Parse(message) | Error::Api(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Io(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
