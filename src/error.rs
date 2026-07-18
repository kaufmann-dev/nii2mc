use serde_json::Value;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Usage,
    Incompatible,
    Io,
}

impl ErrorKind {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Usage => 2,
            Self::Incompatible => 3,
            Self::Io => 4,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Usage => "invalid_input",
            Self::Incompatible => "incompatible_data",
            Self::Io => "io_error",
        }
    }
}

#[derive(Debug)]
pub struct AppError {
    pub kind: ErrorKind,
    pub message: String,
    pub details: Option<Value>,
}

impl AppError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Usage, message)
    }

    pub fn incompatible(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Incompatible, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Io, message)
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl Display for AppError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(value: std::io::Error) -> Self {
        Self::io(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
