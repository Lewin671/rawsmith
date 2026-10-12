use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The file is damaged or not what it claims to be.
    Format(String),
    /// The file is valid but uses something this tool does not implement.
    Unsupported(String),
    /// The parameter file or a command-line value is invalid.
    Params(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Stable machine-readable category, reported in JSON errors.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Io(_) => "io",
            Error::Format(_) => "format",
            Error::Unsupported(_) => "unsupported",
            Error::Params(_) => "params",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Format(m) | Error::Unsupported(m) | Error::Params(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
