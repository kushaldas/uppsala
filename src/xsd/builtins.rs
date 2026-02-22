//! Built-in type validation helpers for XSD datatypes.
//!
//! Provides validation of values against XSD built-in types (string, boolean,
//! decimal, float, double, integer variants, date/time types, binary types,
//! name types, etc.), whitespace normalization, and facet enforcement.

use std::cmp::Ordering;

use crate::dom::{Document, NodeId};
use crate::error::ValidationError;
use crate::namespace::build_resolver_for_node;
use crate::xsd_regex::XsdRegex;

use super::datetime::{
    is_valid_date, is_valid_datetime, is_valid_duration, is_valid_gday, is_valid_gmonth,
    is_valid_gmonthday, is_valid_gyear, is_valid_gyearmonth, is_valid_time, normalize_datetime_tz,
};
use super::decimal::compare_values;
use super::types::{BuiltInType, Facet, WhiteSpaceHandling};

/// Check if a string is a valid NCName (non-colonized name).
pub(crate) fn is_valid_ncname(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Check if a string is a valid XML Name (allows colons, unlike NCName).
/// NameStartChar = letter | '_' | ':'
/// NameChar = NameStartChar | digit | '.' | '-'
/// Covers MS tests: Name001/004/005/006/014/017/018
fn is_valid_xml_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_' || first == ':') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// Check if a string is a valid QName (prefix:localname or just localname).
/// Both prefix and localname must be valid NCNames.
/// Covers MS tests: QName001/004/005/007/008/010/011
fn is_valid_qname(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if let Some(colon_pos) = s.find(':') {
        // Must have exactly one colon
        if s[colon_pos + 1..].contains(':') {
            return false;
        }
        let prefix = &s[..colon_pos];
        let local = &s[colon_pos + 1..];
        is_valid_ncname(prefix) && is_valid_ncname(local)
    } else {
        is_valid_ncname(s)
    }
}

/// Determine the whiteSpace normalization mode for a built-in type.
/// Per XSD Part 2: string→preserve, normalizedString→replace,
/// token and all types derived from token→collapse.
pub(crate) fn whitespace_for_type(bt: &BuiltInType) -> WhiteSpaceHandling {
    match bt {
        BuiltInType::String | BuiltInType::AnyType | BuiltInType::AnySimpleType => {
            WhiteSpaceHandling::Preserve
        }
        BuiltInType::NormalizedString => WhiteSpaceHandling::Replace,
        // Token and everything derived from it use collapse
        _ => WhiteSpaceHandling::Collapse,
    }
}

/// Apply XSD whiteSpace normalization to a string value.
/// - Preserve: return as-is
/// - Replace: replace CR, LF, TAB with space
/// - Collapse: replace CR/LF/TAB with space, collapse runs of spaces, strip leading/trailing
pub(crate) fn apply_whitespace_normalization(text: &str, mode: &WhiteSpaceHandling) -> String {
    match mode {
        WhiteSpaceHandling::Preserve => text.to_string(),
        WhiteSpaceHandling::Replace => text
            .chars()
            .map(|c| {
                if c == '\r' || c == '\n' || c == '\t' {
                    ' '
                } else {
                    c
                }
            })
            .collect(),
        WhiteSpaceHandling::Collapse => {
            let replaced: String = text
                .chars()
                .map(|c| {
                    if c == '\r' || c == '\n' || c == '\t' {
                        ' '
                    } else {
                        c
                    }
                })
                .collect();
            let mut result = String::with_capacity(replaced.len());
            let mut prev_space = true; // true to strip leading spaces
            for c in replaced.chars() {
                if c == ' ' {
                    if !prev_space {
                        result.push(' ');
                    }
                    prev_space = true;
                } else {
                    result.push(c);
                    prev_space = false;
                }
            }
            // Strip trailing space
            if result.ends_with(' ') {
                result.pop();
            }
            result
        }
    }
}

pub(crate) fn validate_builtin_value(
    text: &str,
    bt: &BuiltInType,
    doc: &Document,
    node: NodeId,
    errors: &mut Vec<ValidationError>,
) {
    // Apply XSD whiteSpace normalization before any validation.
    // Per XSD Part 2, whiteSpace is a pre-processing step applied to the
    // ·lexical representation· before all other facet checks and type validation.
    let ws_mode = whitespace_for_type(bt);
    let normalized = apply_whitespace_normalization(text, &ws_mode);
    let text = &normalized;

    match bt {
        BuiltInType::String | BuiltInType::AnyType | BuiltInType::AnySimpleType => {
            // Any string is valid
        }
        BuiltInType::NormalizedString => {
            // After replace normalization, CR/LF/TAB should already be gone.
            // This check is for safety.
            if text.contains('\r') || text.contains('\n') || text.contains('\t') {
                errors.push(ValidationError {
                    message: "normalizedString must not contain CR, LF, or TAB".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Token => {
            // After collapse normalization, text is already collapsed.
            // Nothing further to check for plain xs:token.
        }
        BuiltInType::Boolean => {
            let v = text.trim();
            if !matches!(v, "true" | "false" | "1" | "0") {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid boolean", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS tests: decimal019-022/025 — reject scientific notation, INF, NaN
        BuiltInType::Decimal => {
            let v = text.trim();
            // XSD decimal lexical space: [+-]?digit+(.digit+)?
            // Must NOT accept scientific notation (E/e), INF, NaN
            let valid = {
                let s = if v.starts_with('+') || v.starts_with('-') {
                    &v[1..]
                } else {
                    v
                };
                if s.is_empty() {
                    false
                } else if let Some(dot_pos) = s.find('.') {
                    let integer_part = &s[..dot_pos];
                    let frac_part = &s[dot_pos + 1..];
                    // Integer part can be empty if there's a fractional part (e.g., ".5")
                    // but at least one of integer or fractional must be non-empty
                    (integer_part.is_empty() || integer_part.chars().all(|c| c.is_ascii_digit()))
                        && !frac_part.is_empty()
                        && frac_part.chars().all(|c| c.is_ascii_digit())
                } else {
                    s.chars().all(|c| c.is_ascii_digit())
                }
            };
            if !valid {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid decimal", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS tests: float018/022-026, double018/022-026 — case-sensitive special values
        BuiltInType::Float | BuiltInType::Double => {
            let v = text.trim();
            let valid = if v == "INF" || v == "-INF" || v == "NaN" {
                true
            } else if v.eq_ignore_ascii_case("inf")
                || v.eq_ignore_ascii_case("nan")
                || v.eq_ignore_ascii_case("-nan")
                || v.eq_ignore_ascii_case("+nan")
                || v == "+INF"
                || v == "+inf"
                || v == "infinity"
                || v == "+infinity"
                || v == "-infinity"
                || v.eq_ignore_ascii_case("infinity")
            {
                false
            } else {
                v.parse::<f64>().is_ok()
            };
            if !valid {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid float/double", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Integer => {
            let v = text.trim();
            if v.parse::<i128>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid integer", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Long => {
            let v = text.trim();
            if v.parse::<i64>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid long", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Int => {
            let v = text.trim();
            if v.parse::<i32>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid int", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Short => {
            let v = text.trim();
            if v.parse::<i16>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid short", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Byte => {
            let v = text.trim();
            if v.parse::<i8>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid byte", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::NonNegativeInteger => {
            let v = text.trim();
            match v.parse::<i128>() {
                Ok(n) if n >= 0 => {}
                _ => {
                    errors.push(ValidationError {
                        message: format!("'{}' is not a valid nonNegativeInteger", text),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        BuiltInType::PositiveInteger => {
            let v = text.trim();
            match v.parse::<i128>() {
                Ok(n) if n > 0 => {}
                _ => {
                    errors.push(ValidationError {
                        message: format!("'{}' is not a valid positiveInteger", text),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        BuiltInType::NonPositiveInteger => {
            let v = text.trim();
            match v.parse::<i128>() {
                Ok(n) if n <= 0 => {}
                _ => {
                    errors.push(ValidationError {
                        message: format!("'{}' is not a valid nonPositiveInteger", text),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        BuiltInType::NegativeInteger => {
            let v = text.trim();
            match v.parse::<i128>() {
                Ok(n) if n < 0 => {}
                _ => {
                    errors.push(ValidationError {
                        message: format!("'{}' is not a valid negativeInteger", text),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        BuiltInType::UnsignedLong => {
            let v = text.trim();
            if v.parse::<u64>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid unsignedLong", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::UnsignedInt => {
            let v = text.trim();
            if v.parse::<u32>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid unsignedInt", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::UnsignedShort => {
            let v = text.trim();
            if v.parse::<u16>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid unsignedShort", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::UnsignedByte => {
            let v = text.trim();
            if v.parse::<u8>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid unsignedByte", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::DateTime => {
            let v = text.trim();
            if !is_valid_datetime(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid dateTime", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Date => {
            let v = text.trim();
            if !is_valid_date(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid date", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Time => {
            let v = text.trim();
            if !is_valid_time(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid time", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS test: hexBinary003 — strip internal whitespace before validation
        BuiltInType::HexBinary => {
            let v: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            if !v.len().is_multiple_of(2) || !v.chars().all(|c| c.is_ascii_hexdigit()) {
                errors.push(ValidationError {
                    message: format!("'{}' is not valid hexBinary", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Base64Binary => {
            let v: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            let is_valid = if v.is_empty() {
                true
            } else if !v.len().is_multiple_of(4) {
                false
            } else {
                let pad_count = v.chars().rev().take_while(|&c| c == '=').count();
                if pad_count > 2 {
                    false
                } else {
                    let data_part = &v[..v.len() - pad_count];
                    let pad_part = &v[v.len() - pad_count..];
                    data_part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
                        && pad_part.chars().all(|c| c == '=')
                }
            };
            if !is_valid {
                errors.push(ValidationError {
                    message: format!("'{}' is not valid base64Binary", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::AnyURI => {
            let v = text.trim();
            if v.contains(' ') {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid anyURI", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::NCName | BuiltInType::ID | BuiltInType::IDREF => {
            let v = text.trim();
            if !is_valid_ncname(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid NCName/ID/IDREF", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS tests: language008/010 — enforce [a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})* pattern
        BuiltInType::Language => {
            let v = text.trim();
            let valid = if v.is_empty() {
                false
            } else {
                let subtags: Vec<&str> = v.split('-').collect();
                if subtags[0].is_empty()
                    || subtags[0].len() > 8
                    || !subtags[0].chars().all(|c| c.is_ascii_alphabetic())
                {
                    false
                } else {
                    subtags[1..].iter().all(|sub| {
                        !sub.is_empty()
                            && sub.len() <= 8
                            && sub.chars().all(|c| c.is_ascii_alphanumeric())
                    })
                }
            };
            if !valid {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid language tag", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::NMTOKEN => {
            let v = text.trim();
            if v.is_empty()
                || !v
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
            {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid NMTOKEN", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::NMTOKENS => {
            let v = text.trim();
            if v.is_empty() {
                errors.push(ValidationError {
                    message: "NMTOKENS must contain at least one token".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else {
                for token in v.split_whitespace() {
                    if token.is_empty()
                        || !token.chars().all(|c| {
                            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
                        })
                    {
                        errors.push(ValidationError {
                            message: format!("'{}' is not a valid NMTOKEN in NMTOKENS", token),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }
        }
        BuiltInType::IDREFS => {
            let v = text.trim();
            if v.is_empty() {
                errors.push(ValidationError {
                    message: "IDREFS must contain at least one IDREF".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else {
                for token in v.split_whitespace() {
                    if !is_valid_ncname(token) {
                        errors.push(ValidationError {
                            message: format!("'{}' is not a valid IDREF in IDREFS", token),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }
        }
        BuiltInType::NOTATION => {
            let v = text.trim();
            if !is_valid_ncname(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid NOTATION value", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::ENTITY => {
            let v = text.trim();
            if !is_valid_ncname(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid ENTITY value", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::ENTITIES => {
            let v = text.trim();
            if v.is_empty() {
                errors.push(ValidationError {
                    message: "ENTITIES must contain at least one ENTITY".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else {
                for token in v.split_whitespace() {
                    if !is_valid_ncname(token) {
                        errors.push(ValidationError {
                            message: format!("'{}' is not a valid ENTITY in ENTITIES", token),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }
        }
        BuiltInType::Duration => {
            let v = text.trim();
            if !is_valid_duration(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid duration", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GYear => {
            let v = text.trim();
            if !is_valid_gyear(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gYear", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GYearMonth => {
            let v = text.trim();
            if !is_valid_gyearmonth(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gYearMonth", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GMonth => {
            let v = text.trim();
            if !is_valid_gmonth(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gMonth", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GMonthDay => {
            let v = text.trim();
            if !is_valid_gmonthday(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gMonthDay", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GDay => {
            let v = text.trim();
            if !is_valid_gday(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gDay", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS tests: Name001/004/005/006/014/017/018
        BuiltInType::Name => {
            let v = text.trim();
            if !is_valid_xml_name(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid Name", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS tests: QName001/004/005/007/008/010/011
        // Note: NOTATION is handled above (validates as NCName, not full QName).
        BuiltInType::QName => {
            let v = text.trim();
            if !is_valid_qname(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid QName", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
    }
}

/// Validate a facet for a list type. Length facets count items, not characters.
pub(crate) fn validate_list_facet(
    items: &[&str],
    facet: &Facet,
    text: &str,
    doc: &Document,
    node: NodeId,
    errors: &mut Vec<ValidationError>,
) {
    let item_count = items.len();
    match facet {
        Facet::MinLength(min) => {
            if item_count < *min {
                errors.push(ValidationError {
                    message: format!("List has {} items, less than minLength {}", item_count, min),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MaxLength(max) => {
            if item_count > *max {
                errors.push(ValidationError {
                    message: format!("List has {} items, exceeds maxLength {}", item_count, max),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::Length(len) => {
            if item_count != *len {
                errors.push(ValidationError {
                    message: format!("List has {} items, expected length {}", item_count, len),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::Enumeration(values) => {
            // For list enumerations, the entire space-collapsed value must match
            let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !values.contains(&collapsed) {
                errors.push(ValidationError {
                    message: format!(
                        "'{}' is not one of the allowed values: {:?}",
                        collapsed, values
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::Pattern(pattern) => {
            // Pattern facets on lists apply to the whole collapsed space-separated value
            if let Ok(re) = XsdRegex::compile(pattern) {
                if !re.is_match(text) {
                    errors.push(ValidationError {
                        message: format!("Value '{}' does not match pattern '{}'", text, pattern),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        Facet::WhiteSpace(_) => {}
        _ => {
            // Other facets (min/max inclusive/exclusive, digits) don't apply to lists
        }
    }
}

/// Compute the "length" of a value for Length/MinLength/MaxLength facets,
/// taking into account type-specific semantics per XSD 1.1 spec:
/// - hexBinary: number of octets (string length / 2)
/// - base64Binary: number of decoded octets
/// - QName/NOTATION: number of URI-qualified characters (URI + local-name length)
/// - All others: number of characters
pub(crate) fn type_aware_length(
    text: &str,
    base_type: &BuiltInType,
    doc: &Document,
    node: NodeId,
) -> usize {
    match base_type {
        BuiltInType::HexBinary => {
            // Each pair of hex characters = 1 octet
            let trimmed = text.trim();
            trimmed.len() / 2
        }
        BuiltInType::Base64Binary => {
            // Count decoded octets from base64
            let stripped: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            if stripped.is_empty() {
                return 0;
            }
            let padding = stripped.chars().rev().take_while(|&c| c == '=').count();
            let non_padding = stripped.len() - padding;
            // Each 4 base64 chars = 3 bytes, minus padding bytes
            (non_padding * 3) / 4
        }
        BuiltInType::QName => {
            // XSD spec: QName length = len(namespace URI) + len(local name).
            // We resolve the QName prefix against the instance document's namespace context.
            let trimmed = text.trim();
            let (prefix, local_name) = if let Some(colon_pos) = trimmed.find(':') {
                (&trimmed[..colon_pos], &trimmed[colon_pos + 1..])
            } else {
                ("", trimmed)
            };

            if prefix.is_empty() {
                // Unprefixed QName: in no namespace, length = local name length.
                local_name.len()
            } else {
                // Prefixed QName: resolve the prefix to a namespace URI
                let resolver = build_resolver_for_node(doc, node);
                if let Some(ns_uri) = resolver.resolve(prefix) {
                    ns_uri.len() + local_name.len()
                } else {
                    // Prefix not bound — fall back to local name length
                    local_name.len()
                }
            }
        }
        _ => text.len(),
    }
}

pub(crate) fn validate_facet(
    text: &str,
    facet: &Facet,
    base_type: &BuiltInType,
    doc: &Document,
    node: NodeId,
    errors: &mut Vec<ValidationError>,
    enforce_qname_length_facets: bool,
) {
    // When enforce_qname_length_facets is false, skip length/minLength/maxLength
    // for QName and NOTATION types (NIST test suite interpretation of W3C Bug #4009).
    let skip_length = !enforce_qname_length_facets
        && matches!(base_type, BuiltInType::QName | BuiltInType::NOTATION);

    match facet {
        Facet::MinLength(min) => {
            if !skip_length {
                let len = type_aware_length(text, base_type, doc, node);
                if len < *min {
                    errors.push(ValidationError {
                        message: format!("Value length {} is less than minLength {}", len, min),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        Facet::MaxLength(max) => {
            if !skip_length {
                let len = type_aware_length(text, base_type, doc, node);
                if len > *max {
                    errors.push(ValidationError {
                        message: format!("Value length {} exceeds maxLength {}", len, max),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        Facet::Length(expected) => {
            if !skip_length {
                let len = type_aware_length(text, base_type, doc, node);
                if len != *expected {
                    errors.push(ValidationError {
                        message: format!("Value length {} does not match length {}", len, expected),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        Facet::Enumeration(values) => {
            let text_normalized = normalize_datetime_tz(text.trim());
            let match_found = values.iter().any(|v| {
                let v_normalized = normalize_datetime_tz(v.trim());
                v_normalized == text_normalized
            });
            if !match_found {
                errors.push(ValidationError {
                    message: format!("'{}' is not one of the allowed values: {:?}", text, values),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MinInclusive(min) => {
            if compare_values(text.trim(), min) == Ordering::Less {
                errors.push(ValidationError {
                    message: format!("Value '{}' is less than minInclusive {}", text.trim(), min),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MaxInclusive(max) => {
            if compare_values(text.trim(), max) == Ordering::Greater {
                errors.push(ValidationError {
                    message: format!("Value '{}' exceeds maxInclusive {}", text.trim(), max),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MinExclusive(min) => {
            let cmp = compare_values(text.trim(), min);
            if cmp == Ordering::Less || cmp == Ordering::Equal {
                errors.push(ValidationError {
                    message: format!(
                        "Value '{}' is not greater than minExclusive {}",
                        text.trim(),
                        min
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MaxExclusive(max) => {
            let cmp = compare_values(text.trim(), max);
            if cmp == Ordering::Greater || cmp == Ordering::Equal {
                errors.push(ValidationError {
                    message: format!(
                        "Value '{}' is not less than maxExclusive {}",
                        text.trim(),
                        max
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::TotalDigits(max_digits) => {
            let digits: String = text.trim().chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() > *max_digits {
                errors.push(ValidationError {
                    message: format!(
                        "Total digits {} exceeds totalDigits {}",
                        digits.len(),
                        max_digits
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::FractionDigits(max_frac) => {
            if let Some(dot_pos) = text.find('.') {
                let frac = &text[dot_pos + 1..];
                let frac_len = frac.trim_end_matches('0').len();
                if frac_len > *max_frac {
                    errors.push(ValidationError {
                        message: format!(
                            "Fraction digits {} exceeds fractionDigits {}",
                            frac_len, max_frac
                        ),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        Facet::Pattern(pattern) => {
            if let Ok(re) = XsdRegex::compile(pattern) {
                if !re.is_match(text) {
                    errors.push(ValidationError {
                        message: format!("Value '{}' does not match pattern '{}'", text, pattern),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
            // If the pattern fails to compile, we silently accept
            // (graceful degradation for unsupported regex features)
        }
        Facet::WhiteSpace(_) => {
            // White space normalization is applied during parsing
        }
    }
}
