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
    pub message: String,
    pub line: usize,
    pub column: usize,
}

/// A well-formedness constraint violation.
#[derive(Debug, Clone, PartialEq)]
pub struct WellFormednessError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

/// A namespace constraint violation.
#[derive(Debug, Clone, PartialEq)]
pub struct NamespaceError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

/// An XPath evaluation error.
#[derive(Debug, Clone, PartialEq)]
pub struct XPathError {
    pub message: String,
}

/// An XSD validation error.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub message: String,
    pub line: Option<usize>,
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
    pub fn parse(message: impl Into<String>, line: usize, column: usize) -> Self {
        XmlError::Parse(ParseError {
            message: message.into(),
            line,
            column,
        })
    }

    pub fn well_formedness(message: impl Into<String>, line: usize, column: usize) -> Self {
        XmlError::WellFormedness(WellFormednessError {
            message: message.into(),
            line,
            column,
        })
    }

    pub fn namespace(message: impl Into<String>, line: usize, column: usize) -> Self {
        XmlError::Namespace(NamespaceError {
            message: message.into(),
            line,
            column,
        })
    }

    pub fn xpath(message: impl Into<String>) -> Self {
        XmlError::XPath(XPathError {
            message: message.into(),
        })
    }

    pub fn validation(message: impl Into<String>) -> Self {
        XmlError::Validation(ValidationError {
            message: message.into(),
            line: None,
            column: None,
        })
    }
}
