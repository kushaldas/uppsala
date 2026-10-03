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

/// Whether a type carries an XSD timezone/fractional-second lexical form whose
/// enumeration values must be normalized before comparison (e.g. `...+00:00`
/// vs `...Z`). All XSD date/time types are timezone-bearing; non-temporal types
/// (string, etc.) must keep their lexical value so a string that merely looks
/// like a timestamp is not silently normalized.
fn is_temporal_type(base_type: &BuiltInType) -> bool {
    matches!(
        base_type,
        BuiltInType::DateTime
            | BuiltInType::Time
            | BuiltInType::Date
            | BuiltInType::GYear
            | BuiltInType::GYearMonth
            | BuiltInType::GMonth
            | BuiltInType::GMonthDay
            | BuiltInType::GDay
    )
}

/// Range-validate a value against a date-like temporal base type, so an invalid
/// facet bound (e.g. `--99` gMonth) is rejected instead of compared lexically.
fn is_valid_date_like(s: &str, base_type: &BuiltInType) -> bool {
    match base_type {
        BuiltInType::Date => is_valid_date(s),
        BuiltInType::GYear => is_valid_gyear(s),
        BuiltInType::GYearMonth => is_valid_gyearmonth(s),
        BuiltInType::GMonth => is_valid_gmonth(s),
        BuiltInType::GMonthDay => is_valid_gmonthday(s),
        BuiltInType::GDay => is_valid_gday(s),
        _ => true,
    }
}

/// A temporal value placed on the timeline: `seconds` is UTC-normalized when
/// the lexical form carries a timezone, otherwise local. Local vs UTC-normalized
/// instants are only partially ordered (see `compare_temporal_instants`).
struct TemporalInstant {
    seconds: i128,
    fraction: String,
    has_tz: bool,
}

/// Maximum timezone offset (14:00) in seconds. Per the XSD order relation on
/// dateTime (Part 2 section 3.2.7.4), a timezone-less value compared against a
/// timezoned one is determinate only when they are more than this far apart.
const MAX_TZ_OFFSET_SECONDS: i128 = 14 * 3_600;

/// Reference year used to place the recurring gMonth/gMonthDay/gDay types on
/// the timeline for comparison (a leap year, so --02-29 is representable),
/// mirroring the XSD 1.1 timeline mapping.
const G_TYPE_REFERENCE_YEAR: i128 = 1972;

fn compare_facet_values(
    value: &str,
    facet_value: &str,
    base_type: &BuiltInType,
) -> Option<Ordering> {
    match base_type {
        BuiltInType::DateTime => compare_datetime_values(value, facet_value),
        BuiltInType::Time => compare_time_values(value, facet_value),
        BuiltInType::Date
        | BuiltInType::GYear
        | BuiltInType::GYearMonth
        | BuiltInType::GMonth
        | BuiltInType::GMonthDay
        | BuiltInType::GDay => {
            // Fail closed on lexically-parseable but out-of-range operands, as
            // compare_datetime_values/compare_time_values do. A raw comparison of
            // an invalid facet bound would otherwise silently accept instances.
            if !is_valid_date_like(value, base_type) || !is_valid_date_like(facet_value, base_type)
            {
                return None;
            }
            let left = date_like_to_instant(value, base_type)?;
            let right = date_like_to_instant(facet_value, base_type)?;
            compare_temporal_instants(&left, &right)
        }
        _ => Some(compare_values(value, facet_value)),
    }
}

fn compare_datetime_values(value: &str, facet_value: &str) -> Option<Ordering> {
    // Fail closed on lexically-parseable but out-of-range values (e.g. year 0000,
    // month 99, hour 99). Facet values are stored as raw strings and are not
    // otherwise range-checked, so an invalid minInclusive/maxInclusive must not
    // yield a comparable ordering.
    if !is_valid_datetime(value) || !is_valid_datetime(facet_value) {
        return None;
    }
    let left = datetime_to_instant(value)?;
    let right = datetime_to_instant(facet_value)?;
    compare_temporal_instants(&left, &right)
}

fn compare_time_values(value: &str, facet_value: &str) -> Option<Ordering> {
    // Fail closed on out-of-range time strings (e.g. 99:99:99Z); see
    // compare_datetime_values for the rationale.
    if !is_valid_time(value) || !is_valid_time(facet_value) {
        return None;
    }
    let left = time_to_instant(value)?;
    let right = time_to_instant(facet_value)?;
    compare_temporal_instants(&left, &right)
}

fn datetime_to_instant(value: &str) -> Option<TemporalInstant> {
    let (date, time) = value.split_once('T')?;
    let (year, month, day) = parse_xsd_date_parts(date)?;
    let (hour, minute, second, fraction, offset_minutes) = parse_xsd_time_parts(time)?;
    let days = days_from_civil(year, month, day);
    let local_seconds =
        days * 86_400 + i128::from(hour) * 3_600 + i128::from(minute) * 60 + i128::from(second);
    Some(TemporalInstant {
        seconds: local_seconds - i128::from(offset_minutes.unwrap_or(0)) * 60,
        fraction: normalize_fraction(&fraction),
        has_tz: offset_minutes.is_some(),
    })
}

fn time_to_instant(value: &str) -> Option<TemporalInstant> {
    let (hour, minute, second, fraction, offset_minutes) = parse_xsd_time_parts(value)?;
    let local_seconds = i128::from(hour) * 3_600 + i128::from(minute) * 60 + i128::from(second);
    Some(TemporalInstant {
        seconds: local_seconds - i128::from(offset_minutes.unwrap_or(0)) * 60,
        fraction: normalize_fraction(&fraction),
        has_tz: offset_minutes.is_some(),
    })
}

/// Map a date-like temporal value (date, gYear, gYearMonth, gMonth, gMonthDay,
/// gDay) onto the timeline as the starting instant of its period, so ordering
/// honors timezone offsets and numeric years instead of lexical form.
fn date_like_to_instant(value: &str, base_type: &BuiltInType) -> Option<TemporalInstant> {
    let (body, offset_minutes) = split_tz_suffix(value);
    let (year, month, day) = match base_type {
        BuiltInType::Date => parse_xsd_date_parts(body)?,
        BuiltInType::GYear => (body.parse().ok()?, 1, 1),
        BuiltInType::GYearMonth => {
            let (year, month) = body.rsplit_once('-')?;
            (year.parse().ok()?, month.parse().ok()?, 1)
        }
        BuiltInType::GMonth => {
            // Accept --MM and the XSD 1.0 legacy --MM-- form.
            let month = body.strip_prefix("--")?.trim_end_matches("--");
            (G_TYPE_REFERENCE_YEAR, month.parse().ok()?, 1)
        }
        BuiltInType::GMonthDay => {
            let (month, day) = body.strip_prefix("--")?.split_once('-')?;
            (
                G_TYPE_REFERENCE_YEAR,
                month.parse().ok()?,
                day.parse().ok()?,
            )
        }
        BuiltInType::GDay => (
            G_TYPE_REFERENCE_YEAR,
            1,
            body.strip_prefix("---")?.parse().ok()?,
        ),
        _ => return None,
    };
    Some(TemporalInstant {
        seconds: days_from_civil(year, month, day) * 86_400
            - i128::from(offset_minutes.unwrap_or(0)) * 60,
        fraction: String::new(),
        has_tz: offset_minutes.is_some(),
    })
}

/// Compare two timeline instants per XSD's partial order: values that both
/// carry (or both omit) a timezone are totally ordered; a timezone-less value
/// against a timezoned one is determinate only when more than 14 hours apart.
/// Indeterminate comparisons return None, which the facet checks report as an
/// error (fail closed) instead of assuming UTC.
fn compare_temporal_instants(left: &TemporalInstant, right: &TemporalInstant) -> Option<Ordering> {
    match (left.has_tz, right.has_tz) {
        (true, true) | (false, false) => Some(compare_instant_parts(
            left.seconds,
            &left.fraction,
            right.seconds,
            &right.fraction,
        )),
        (false, true) => {
            // Timezone-less left could lie anywhere in [-14:00, +14:00]:
            // left < right only if even its latest interpretation is earlier,
            // and left > right only if even its earliest one is later.
            if compare_instant_parts(
                left.seconds + MAX_TZ_OFFSET_SECONDS,
                &left.fraction,
                right.seconds,
                &right.fraction,
            ) == Ordering::Less
            {
                Some(Ordering::Less)
            } else if compare_instant_parts(
                left.seconds - MAX_TZ_OFFSET_SECONDS,
                &left.fraction,
                right.seconds,
                &right.fraction,
            ) == Ordering::Greater
            {
                Some(Ordering::Greater)
            } else {
                None
            }
        }
        (true, false) => compare_temporal_instants(right, left).map(Ordering::reverse),
    }
}

fn compare_instant_parts(
    left_seconds: i128,
    left_fraction: &str,
    right_seconds: i128,
    right_fraction: &str,
) -> Ordering {
    match left_seconds.cmp(&right_seconds) {
        Ordering::Equal => compare_fraction(left_fraction, right_fraction),
        ord => ord,
    }
}

fn normalize_fraction(fraction: &str) -> String {
    fraction.trim_end_matches('0').to_string()
}

fn compare_fraction(left: &str, right: &str) -> Ordering {
    // Digit-wise comparison with an implied '0' past the shorter end. The
    // fractional part is attacker-sized (validation allows any number of
    // digits), so nothing is allocated: the shared prefix is an ordered byte
    // slice comparison (lowered to SIMD-optimized memcmp) and the leftover
    // tail only decides the ordering if it contains a non-zero digit. Both
    // strings are ASCII digits by the time they reach a comparison.
    let left = left.as_bytes();
    let right = right.as_bytes();
    let shared = left.len().min(right.len());
    match left[..shared].cmp(&right[..shared]) {
        Ordering::Equal => {}
        ord => return ord,
    }
    if left[shared..].iter().any(|&b| b != b'0') {
        Ordering::Greater
    } else if right[shared..].iter().any(|&b| b != b'0') {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

fn parse_xsd_date_parts(date: &str) -> Option<(i128, u32, u32)> {
    let (negative, rest) = date
        .strip_prefix('-')
        .map(|s| (true, s))
        .unwrap_or((false, date));
    let mut parts = rest.split('-');
    let year_str = parts.next()?;
    let month_str = parts.next()?;
    let day_str = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let year: i128 = year_str.parse().ok()?;
    let month: u32 = month_str.parse().ok()?;
    let day: u32 = day_str.parse().ok()?;
    Some((if negative { -year } else { year }, month, day))
}

fn parse_xsd_time_parts(time: &str) -> Option<(u32, u32, u32, String, Option<i32>)> {
    let (time, offset_minutes) = split_tz_suffix(time);
    let mut parts = time.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second_part = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let (second_str, frac_str) = second_part.split_once('.').unwrap_or((second_part, ""));
    let second: u32 = second_str.parse().ok()?;
    if !frac_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((hour, minute, second, frac_str.to_string(), offset_minutes))
}

/// Split an optional trailing timezone (`Z` or `+hh:mm`/`-hh:mm`) from a
/// temporal lexical form. Returns the remaining body and the offset in
/// minutes; a missing timezone is `None`, not UTC. Unrecognized suffixes are
/// left in the body, where the callers' numeric parsing fails closed (the
/// overall lexical form is range-validated separately before comparison).
fn split_tz_suffix(s: &str) -> (&str, Option<i32>) {
    if let Some(stripped) = s.strip_suffix('Z') {
        return (stripped, Some(0));
    }
    if s.len() >= 6 {
        let tz_start = s.len() - 6;
        let tz = &s.as_bytes()[tz_start..];
        if !tz.is_ascii() {
            return (s, None);
        }
        let sign = match tz[0] {
            b'+' => 1,
            b'-' => -1,
            _ => return (s, None),
        };
        if tz[3] != b':' {
            return (s, None);
        }
        if let (Ok(hours), Ok(minutes)) = (
            s[tz_start + 1..tz_start + 3].parse::<i32>(),
            s[tz_start + 4..].parse::<i32>(),
        ) {
            return (&s[..tz_start], Some(sign * (hours * 60 + minutes)));
        }
    }
    (s, None)
}

fn days_from_civil(year: i128, month: u32, day: u32) -> i128 {
    let mut y = year;
    let m = i128::from(month);
    y -= i128::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i128::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn push_facet_compare_error(
    facet_name: &str,
    value: &str,
    facet_value: &str,
    doc: &Document,
    node: NodeId,
    errors: &mut Vec<ValidationError>,
) {
    errors.push(ValidationError {
        message: format!(
            "Cannot compare value '{}' with {} {} for this datatype",
            value, facet_name, facet_value
        ),
        line: Some(doc.node_line(node)),
        column: Some(doc.node_column(node)),
    });
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
    lenient: bool,
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
            if v.len() % 2 != 0 || !v.chars().all(|c| c.is_ascii_hexdigit()) {
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
            } else if v.len() % 4 != 0 {
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
            // `anyURI` validation here is intentionally minimal: after XSD whitespace normalization,
            // strict mode rejects values containing a space (and thus any collapsed whitespace).
            // libxml2 is more permissive and accepts spaces too; in lenient mode we match it
            // (this also allows whitespace-separated tokens that reach here as a single anyURI
            // value to validate). Strict mode
            // keeps the space check.
            let v = text.trim();
            if !lenient && v.contains(' ') {
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
            } else if let Some(colon_pos) = v.find(':') {
                let prefix = &v[..colon_pos];
                let resolver = build_resolver_for_node(doc, node);
                if resolver.resolve(prefix).is_none() {
                    errors.push(ValidationError {
                        message: format!("QName prefix '{}' is not bound", prefix),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
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
            match XsdRegex::compile(pattern) {
                Ok(re) if !re.is_match(text) => {
                    errors.push(ValidationError {
                        message: format!("Value '{}' does not match pattern '{}'", text, pattern),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
                Ok(_) => {}
                Err(e) => errors.push(ValidationError {
                    message: format!("Pattern facet '{}' could not be compiled: {}", pattern, e),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                }),
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
            let text_normalized = if is_temporal_type(base_type) {
                normalize_datetime_tz(text.trim())
            } else {
                text.trim().to_string()
            };
            let match_found = values.iter().any(|v| {
                let v_normalized = if is_temporal_type(base_type) {
                    normalize_datetime_tz(v.trim())
                } else {
                    v.trim().to_string()
                };
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
        Facet::MinInclusive(min) => match compare_facet_values(text.trim(), min, base_type) {
            Some(Ordering::Less) => {
                errors.push(ValidationError {
                    message: format!("Value '{}' is less than minInclusive {}", text.trim(), min),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
            Some(_) => {}
            None => push_facet_compare_error("minInclusive", text.trim(), min, doc, node, errors),
        },
        Facet::MaxInclusive(max) => match compare_facet_values(text.trim(), max, base_type) {
            Some(Ordering::Greater) => {
                errors.push(ValidationError {
                    message: format!("Value '{}' exceeds maxInclusive {}", text.trim(), max),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
            Some(_) => {}
            None => push_facet_compare_error("maxInclusive", text.trim(), max, doc, node, errors),
        },
        Facet::MinExclusive(min) => match compare_facet_values(text.trim(), min, base_type) {
            Some(Ordering::Less | Ordering::Equal) => {
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
            Some(_) => {}
            None => push_facet_compare_error("minExclusive", text.trim(), min, doc, node, errors),
        },
        Facet::MaxExclusive(max) => match compare_facet_values(text.trim(), max, base_type) {
            Some(Ordering::Greater | Ordering::Equal) => {
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
            Some(_) => {}
            None => push_facet_compare_error("maxExclusive", text.trim(), max, doc, node, errors),
        },
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
        Facet::Pattern(pattern) => match XsdRegex::compile(pattern) {
            Ok(re) if !re.is_match(text) => {
                errors.push(ValidationError {
                    message: format!("Value '{}' does not match pattern '{}'", text, pattern),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
            Ok(_) => {}
            Err(e) => errors.push(ValidationError {
                message: format!("Pattern facet '{}' could not be compiled: {}", pattern, e),
                line: Some(doc.node_line(node)),
                column: Some(doc.node_column(node)),
            }),
        },
        Facet::WhiteSpace(_) => {
            // White space normalization is applied during parsing
        }
    }
}
