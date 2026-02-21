//! Error types for the Uppsala XML library.

use std::fmt;

/// The result type used throughout the library.
pub type XmlResult<T> = Result<T, XmlError>;

/// Represents all possible errors that can occur during XML processing.
#[derive(Debug, Clone, PartialEq)]
pub enum XmlError {
    /// The document is not well-formed XML.
    WellFormedness(WellFormednessError),
    /// A namespace-related error.
    Namespace(NamespaceError),
    /// An XPath evaluation error.
    XPath(XPathError),
    /// An XSD validation error.
    Validation(ValidationError),
    /// An unexpected end of input.
    UnexpectedEof,
    /// A generic parse error with a message.
    Parse(ParseError),
}

/// A parse error with location information.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    /// Human-readable description of the error.
    pub message: String,
    /// Line number where the error was detected (1-based).
    pub line: usize,
    /// Column number where the error was detected (1-based).
    pub column: usize,
}

/// A well-formedness constraint violation.
#[derive(Debug, Clone, PartialEq)]
pub struct WellFormednessError {
    /// Human-readable description of the violation.
    pub message: String,
    /// Line number where the violation was detected (1-based).
    pub line: usize,
    /// Column number where the violation was detected (1-based).
    pub column: usize,
}

/// A namespace constraint violation.
#[derive(Debug, Clone, PartialEq)]
pub struct NamespaceError {
    /// Human-readable description of the namespace error.
    pub message: String,
    /// Line number where the error was detected (1-based).
    pub line: usize,
    /// Column number where the error was detected (1-based).
    pub column: usize,
}

/// An XPath evaluation error.
#[derive(Debug, Clone, PartialEq)]
pub struct XPathError {
    /// Human-readable description of the XPath error.
    pub message: String,
}

/// An XSD validation error.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    /// Human-readable description of the validation failure.
    pub message: String,
    /// Line number in the instance document, if available.
    pub line: Option<usize>,
    /// Column number in the instance document, if available.
    pub column: Option<usize>,
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XmlError::WellFormedness(e) => {
                write!(
                    f,
                    "Well-formedness error at {}:{}: {}",
                    e.line, e.column, e.message
                )
            }
            XmlError::Namespace(e) => {
                write!(
                    f,
                    "Namespace error at {}:{}: {}",
                    e.line, e.column, e.message
                )
            }
            XmlError::XPath(e) => write!(f, "XPath error: {}", e.message),
            XmlError::Validation(e) => match (e.line, e.column) {
                (Some(l), Some(c)) => {
                    write!(f, "Validation error at {}:{}: {}", l, c, e.message)
                }
                _ => write!(f, "Validation error: {}", e.message),
            },
            XmlError::UnexpectedEof => write!(f, "Unexpected end of input"),
            XmlError::Parse(e) => {
                write!(f, "Parse error at {}:{}: {}", e.line, e.column, e.message)
            }
        }
    }
}

impl std::error::Error for XmlError {}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(l), Some(c)) => write!(f, "{}:{}: {}", l, c, self.message),
            _ => write!(f, "{}", self.message),
        }
    }
}

impl XmlError {
    /// Create a [`ParseError`] with the given message and source location.
    pub fn parse(message: impl Into<String>, line: usize, column: usize) -> Self {
        XmlError::Parse(ParseError {
            message: message.into(),
            line,
            column,
        })
    }

    /// Create a [`WellFormednessError`] with the given message and source location.
    pub fn well_formedness(message: impl Into<String>, line: usize, column: usize) -> Self {
        XmlError::WellFormedness(WellFormednessError {
            message: message.into(),
            line,
            column,
        })
    }

    /// Create a [`NamespaceError`] with the given message and source location.
    pub fn namespace(message: impl Into<String>, line: usize, column: usize) -> Self {
        XmlError::Namespace(NamespaceError {
            message: message.into(),
            line,
            column,
        })
    }

    /// Create an [`XPathError`] with the given message.
    pub fn xpath(message: impl Into<String>) -> Self {
        XmlError::XPath(XPathError {
            message: message.into(),
        })
    }

    /// Create a [`ValidationError`] with the given message (no source location).
    pub fn validation(message: impl Into<String>) -> Self {
        XmlError::Validation(ValidationError {
            message: message.into(),
            line: None,
            column: None,
        })
    }
}
