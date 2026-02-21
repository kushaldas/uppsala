//! Decimal string comparison utilities for XSD validation.
//!
//! Provides arbitrary-precision decimal comparison by operating directly on string
//! representations. This avoids floating-point rounding issues that would arise from
//! converting to `f64`. The comparison is used by facet validation (minInclusive,
//! maxInclusive, minExclusive, maxExclusive) for decimal-based types.

use std::cmp::Ordering;

/// Compare two values for ordering. First tries numeric decimal comparison;
/// if either value is not a pure decimal, falls back to lexicographic comparison.
/// This handles date/time types like gMonthDay (--MM-DD), date, dateTime, etc.
pub(crate) fn compare_values(a: &str, b: &str) -> Ordering {
    compare_decimal_strings(a, b).unwrap_or_else(|| a.cmp(b))
}

/// Check if a string is a valid decimal number (optional sign, digits, optional dot+digits).
///
/// Returns `true` for strings like "123", "-45.67", "+0.5", "0".
/// Returns `false` for empty strings, strings with multiple dots, or non-numeric characters.
fn is_decimal_string(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    let s = s
        .strip_prefix('-')
        .or_else(|| s.strip_prefix('+'))
        .unwrap_or(s);
    if s.is_empty() {
        return false;
    }
    let mut has_digit = false;
    let mut has_dot = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            has_digit = true;
        } else if c == '.' && !has_dot {
            has_dot = true;
        } else {
            return false;
        }
    }
    has_digit
}

/// Compare two decimal number strings with arbitrary precision.
///
/// Returns `Some(Ordering)` if both strings are valid decimal numbers,
/// or `None` if either string is not a valid decimal. Handles:
/// - Leading signs (`+`, `-`)
/// - Leading zeros in integer parts
/// - Trailing zeros in fractional parts
/// - Negative zero equals positive zero
fn compare_decimal_strings(a: &str, b: &str) -> Option<Ordering> {
    let a = a.trim();
    let b = b.trim();

    // Validate both inputs are actual decimal numbers
    if !is_decimal_string(a) || !is_decimal_string(b) {
        return None;
    }

    let (a_neg, a_abs) = if let Some(rest) = a.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = a.strip_prefix('+') {
        (false, rest)
    } else {
        (false, a)
    };

    let (b_neg, b_abs) = if let Some(rest) = b.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = b.strip_prefix('+') {
        (false, rest)
    } else {
        (false, b)
    };

    // Split into integer and fractional parts
    let (a_int, a_frac) = split_decimal(a_abs);
    let (b_int, b_frac) = split_decimal(b_abs);

    // Check if values are zero
    let a_is_zero = is_zero(a_int, a_frac);
    let b_is_zero = is_zero(b_int, b_frac);

    if a_is_zero && b_is_zero {
        return Some(Ordering::Equal);
    }

    // Handle sign differences
    if a_neg && !a_is_zero && (!b_neg || b_is_zero) {
        return Some(Ordering::Less);
    }
    if (!a_neg || a_is_zero) && b_neg && !b_is_zero {
        return Some(Ordering::Greater);
    }

    // Both same sign — compare absolute values
    let abs_cmp = compare_abs(a_int, a_frac, b_int, b_frac)?;

    if a_neg && b_neg {
        // Both negative: reverse comparison
        Some(abs_cmp.reverse())
    } else {
        Some(abs_cmp)
    }
}

/// Split a decimal string (without sign) into integer and fractional parts.
///
/// For "123.456" returns ("123", "456").
/// For "789" returns ("789", "").
fn split_decimal(s: &str) -> (&str, &str) {
    if let Some(dot) = s.find('.') {
        (&s[..dot], &s[dot + 1..])
    } else {
        (s, "")
    }
}

/// Check if a decimal value (split into integer and fractional parts) is zero.
///
/// A value is zero if all digits in both parts are '0'.
fn is_zero(int_part: &str, frac_part: &str) -> bool {
    int_part.chars().all(|c| c == '0') && frac_part.chars().all(|c| c == '0')
}

/// Compare the absolute values of two decimals given their integer and fractional parts.
///
/// First compares integer parts (by length after stripping leading zeros, then
/// lexicographically). If integer parts are equal, compares fractional parts
/// digit-by-digit, padding shorter fractions with trailing zeros.
fn compare_abs(a_int: &str, a_frac: &str, b_int: &str, b_frac: &str) -> Option<Ordering> {
    // Strip leading zeros from integer parts
    let a_int = a_int.trim_start_matches('0');
    let b_int = b_int.trim_start_matches('0');

    // Compare integer parts first by length, then lexicographically
    match a_int.len().cmp(&b_int.len()) {
        Ordering::Less => return Some(Ordering::Less),
        Ordering::Greater => return Some(Ordering::Greater),
        Ordering::Equal => match a_int.cmp(b_int) {
            Ordering::Less => return Some(Ordering::Less),
            Ordering::Greater => return Some(Ordering::Greater),
            Ordering::Equal => {}
        },
    }

    // Integer parts are equal — compare fractional parts
    // Pad with trailing zeros to same length
    let max_frac = a_frac.len().max(b_frac.len());
    for i in 0..max_frac {
        let a_digit = a_frac.as_bytes().get(i).copied().unwrap_or(b'0');
        let b_digit = b_frac.as_bytes().get(i).copied().unwrap_or(b'0');
        match a_digit.cmp(&b_digit) {
            Ordering::Less => return Some(Ordering::Less),
            Ordering::Greater => return Some(Ordering::Greater),
            Ordering::Equal => {}
        }
    }

    Some(Ordering::Equal)
}
