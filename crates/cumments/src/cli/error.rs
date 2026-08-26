//! Stable CLI failures and process exit-code classification.

use std::fmt;

/// Stable CLI failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliErrorKind {
    Runtime,
    Validation,
    NotFound,
    Conflict,
    Unauthorized,
    Dependency,
    Confirmation,
}

impl CliErrorKind {
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Runtime => 1,
            Self::Validation => 10,
            Self::NotFound => 11,
            Self::Conflict => 12,
            Self::Unauthorized => 13,
            Self::Dependency => 14,
            Self::Confirmation => 15,
        }
    }
}

/// A classified terminal failure from the CLI adapter boundary.
#[derive(Debug)]
pub struct CliError {
    kind: CliErrorKind,
    message: String,
    source: Option<anyhow::Error>,
}

impl CliError {
    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::Runtime, message)
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::Validation, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::Conflict, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::Unauthorized, message)
    }

    pub fn dependency(message: impl Into<String>, source: anyhow::Error) -> Self {
        let mut error = Self::new(CliErrorKind::Dependency, message);
        error.source = Some(source);
        error
    }

    pub fn confirmation(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::Confirmation, message)
    }

    pub fn kind(&self) -> CliErrorKind {
        self.kind
    }

    fn new(kind: CliErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|source| source.as_ref())
    }
}

impl From<anyhow::Error> for CliError {
    fn from(source: anyhow::Error) -> Self {
        let message = source.root_cause().to_string();
        Self {
            kind: CliErrorKind::Runtime,
            message,
            source: Some(source),
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::runtime(error.to_string())
    }
}

pub type CliResult<T> = Result<T, CliError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_contract_is_stable() {
        assert_eq!(CliErrorKind::Runtime.exit_code(), 1);
        assert_eq!(CliErrorKind::Validation.exit_code(), 10);
        assert_eq!(CliErrorKind::NotFound.exit_code(), 11);
        assert_eq!(CliErrorKind::Conflict.exit_code(), 12);
        assert_eq!(CliErrorKind::Unauthorized.exit_code(), 13);
        assert_eq!(CliErrorKind::Dependency.exit_code(), 14);
        assert_eq!(CliErrorKind::Confirmation.exit_code(), 15);
    }
}
