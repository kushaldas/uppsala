//! Date/time validation helpers for XSD built-in date and time types.
//!
//! Implements format validation for all XSD date/time types:
//! - `xs:dateTime` (YYYY-MM-DDThh:mm:ss[.sss][timezone])
//! - `xs:date` (YYYY-MM-DD[timezone])
//! - `xs:time` (hh:mm:ss[.sss][timezone])
//! - `xs:duration` (PnYnMnDTnHnMnS)
//! - `xs:gYear` ([-]CCYY[timezone])
//! - `xs:gYearMonth` ([-]CCYY-MM[timezone])
//! - `xs:gMonth` (--MM[timezone])
//! - `xs:gMonthDay` (--MM-DD[timezone])
//! - `xs:gDay` (---DD[timezone])
//!
//! Timezone is always optional and can be `Z`, `+hh:mm`, or `-hh:mm`.
//! Also provides a `normalize_datetime_tz` function for normalizing timezone
//! representations for enumeration comparison.

/// Validate XSD duration format: PnYnMnDTnHnMnS
///
/// Rules:
/// - Must start with optional '-' then 'P'
/// - At least one date or time component must follow 'P'
/// - If 'T' is present, at least one time component must follow it
/// - Numbers must be non-negative integers (except seconds which may have fractional part)
pub(crate) fn is_valid_duration(s: &str) -> bool {
    let s = if s.starts_with('-') { &s[1..] } else { s };
    if !s.starts_with('P') || s.len() < 2 {
        return false;
    }
    let rest = &s[1..];

    // Split on 'T' to get date part and optional time part
    let (date_part, time_part) = if let Some(t_pos) = rest.find('T') {
        (&rest[..t_pos], Some(&rest[t_pos + 1..]))
    } else {
        (rest, None)
    };

    let mut has_any_component = false;

    // Parse date part: nY, nM, nD (in order)
    let mut remaining = date_part;
    for designator in ['Y', 'M', 'D'] {
        if let Some(pos) = remaining.find(designator) {
            let num = &remaining[..pos];
            if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            has_any_component = true;
            remaining = &remaining[pos + 1..];
        }
    }
    // There should be nothing left in the date part
    if !remaining.is_empty() {
        return false;
    }

    // Parse time part: nH, nM, nS (or n.nS)
    if let Some(tp) = time_part {
        if tp.is_empty() {
            return false; // T without any time components is invalid
        }
        let mut remaining = tp;
        let mut has_time_component = false;
        for designator in ['H', 'M', 'S'] {
            if let Some(pos) = remaining.find(designator) {
                let num = &remaining[..pos];
                if num.is_empty() {
                    return false;
                }
                // Seconds may have fractional part
                if designator == 'S' {
                    let parts: Vec<&str> = num.split('.').collect();
                    if parts.len() > 2 {
                        return false;
                    }
                    if !parts[0].chars().all(|c| c.is_ascii_digit()) || parts[0].is_empty() {
                        return false;
                    }
                    if parts.len() == 2
                        && (!parts[1].chars().all(|c| c.is_ascii_digit()) || parts[1].is_empty())
                    {
                        return false;
                    }
                } else if !num.chars().all(|c| c.is_ascii_digit()) {
                    return false;
                }
                has_time_component = true;
                remaining = &remaining[pos + 1..];
            }
        }
        if !remaining.is_empty() || !has_time_component {
            return false;
        }
        has_any_component = true;
    }

    has_any_component
}

/// Validate gYear format: [-]CCYY[Z|(+|-)hh:mm]
///
/// The year must be at least 4 digits (CCYY). Leading '-' for negative years
/// (BCE dates) is permitted. Timezone suffix is optional.
pub(crate) fn is_valid_gyear(s: &str) -> bool {
    let s = strip_timezone(s);
    let s = if s.starts_with('-') { &s[1..] } else { s };
    s.len() >= 4 && s.chars().all(|c| c.is_ascii_digit())
}

/// Validate gYearMonth format: [-]CCYY-MM[Z|(+|-)hh:mm]
///
/// Year must be at least 4 digits, month must be 01-12.
pub(crate) fn is_valid_gyearmonth(s: &str) -> bool {
    let s = strip_timezone(s);
    let (s, _neg) = if s.starts_with('-') {
        (&s[1..], true)
    } else {
        (s, false)
    };
    // Find last '-' which separates year from month
    if let Some(dash_pos) = s.rfind('-') {
        if dash_pos < 4 {
            return false;
        }
        let year = &s[..dash_pos];
        let month = &s[dash_pos + 1..];
        if year.len() < 4 || !year.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if month.len() != 2 || !month.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if let Ok(m) = month.parse::<u32>() {
            (1..=12).contains(&m)
        } else {
            false
        }
    } else {
        false
    }
}

/// Validate gMonth format: --MM[Z|(+|-)hh:mm]
///
/// Note: XSD 1.0 also allowed --MM-- (with trailing --), so we accept both
/// for backwards compatibility.
pub(crate) fn is_valid_gmonth(s: &str) -> bool {
    let s = strip_timezone(s);
    if !s.starts_with("--") || s.len() < 4 {
        return false;
    }
    let month_str = &s[2..4];
    if !month_str.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // Accept --MM or --MM-- (XSD 1.0 legacy)
    let rest = &s[4..];
    if !rest.is_empty() && rest != "--" {
        return false;
    }
    if let Ok(m) = month_str.parse::<u32>() {
        (1..=12).contains(&m)
    } else {
        false
    }
}

/// Maximum days in a month (gMonthDay does not specify a year, so Feb allows 29).
fn max_days_for_month(month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 29,
        _ => 0,
    }
}

/// Maximum days in a month for a specific year (handles leap years).
///
/// February has 29 days in leap years, 28 otherwise.
fn max_days_for_month_year(month: u32, year: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Check if a year is a leap year.
///
/// A year is a leap year if divisible by 4, except centuries unless also divisible by 400.
fn is_leap_year(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Validate gMonthDay format: --MM-DD[Z|(+|-)hh:mm]
///
/// Month must be 01-12, day must be valid for that month (Feb allows up to 29
/// since no year is specified).
pub(crate) fn is_valid_gmonthday(s: &str) -> bool {
    let s = strip_timezone(s);
    if !s.starts_with("--") || s.len() < 7 {
        return false;
    }
    let month_str = &s[2..4];
    if s.as_bytes()[4] != b'-' {
        return false;
    }
    let day_str = &s[5..7];
    if !month_str.chars().all(|c| c.is_ascii_digit())
        || !day_str.chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    // Must be exactly 7 chars (after timezone stripping)
    if s.len() != 7 {
        return false;
    }
    let month = match month_str.parse::<u32>() {
        Ok(m) if (1..=12).contains(&m) => m,
        _ => return false,
    };
    let day = match day_str.parse::<u32>() {
        Ok(d) if d >= 1 => d,
        _ => return false,
    };
    day <= max_days_for_month(month)
}

/// Validate gDay format: ---DD[Z|(+|-)hh:mm]
///
/// Day must be 01-31.
pub(crate) fn is_valid_gday(s: &str) -> bool {
    let s = strip_timezone(s);
    if !s.starts_with("---") || s.len() < 5 {
        return false;
    }
    let day_str = &s[3..5];
    if day_str.len() != 2 || !day_str.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // Must be exactly 5 chars after timezone stripping
    if s.len() != 5 {
        return false;
    }
    if let Ok(d) = day_str.parse::<u32>() {
        (1..=31).contains(&d)
    } else {
        false
    }
}

/// Normalize timezone representations in date/time strings so that
/// `Z`, `+00:00`, and `-00:00` are treated as equivalent for enumeration
/// comparison. Also normalizes trailing fractional-zero seconds (e.g.
/// `.000` is removed) so that `2001-01-01T00:00:00.000Z` equals
/// `2001-01-01T00:00:00Z`.
pub(crate) fn normalize_datetime_tz(s: &str) -> String {
    let mut val = String::from(s);
    // Normalize timezone: replace +00:00 or -00:00 with Z
    if val.ends_with("+00:00") || val.ends_with("-00:00") {
        let end = val.len() - 6;
        val.truncate(end);
        val.push('Z');
    }
    // Normalize trailing fractional zeros in seconds: e.g. .000 before Z or tz
    // Find the seconds fractional part and strip trailing zeros
    // Pattern: ...ss.000Z or ...ss.000+hh:mm or ...ss.000
    // We look for the fractional seconds part
    if let Some(dot_pos) = val.rfind('.') {
        // Determine where the fractional part ends (before Z or timezone or end)
        let after_dot = &val[dot_pos + 1..];
        let frac_end = after_dot
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after_dot.len());
        let frac = &after_dot[..frac_end];
        let trimmed_frac = frac.trim_end_matches('0');
        if trimmed_frac.is_empty() {
            // Remove the dot and fractional part entirely
            let suffix = &after_dot[frac_end..];
            let mut new = val[..dot_pos].to_string();
            new.push_str(suffix);
            val = new;
        } else if trimmed_frac.len() < frac.len() {
            let suffix = &after_dot[frac_end..];
            let mut new = val[..dot_pos + 1].to_string();
            new.push_str(trimmed_frac);
            new.push_str(suffix);
            val = new;
        }
    }
    val
}

/// Validate dateTime format: YYYY-MM-DDThh:mm:ss[.sss][Z|(+|-)hh:mm]
///
/// Splits on 'T' and validates the date part and time part independently.
pub(crate) fn is_valid_datetime(s: &str) -> bool {
    // YYYY-MM-DDThh:mm:ss[.sss][Z|(+|-)hh:mm]
    if let Some(t_pos) = s.find('T') {
        let date_part = &s[..t_pos];
        let time_part = &s[t_pos + 1..];
        is_valid_date(date_part) && is_valid_time(time_part)
    } else {
        false
    }
}

/// Validate date format: [-]YYYY-MM-DD[Z|(+|-)hh:mm]
///
/// Checks that year is at least 4 digits and not 0000, month is 01-12,
/// day is valid for that month/year (including leap year handling).
/// MS tests: date003/004/009, dateTime011 — reject invalid ranges.
pub(crate) fn is_valid_date(s: &str) -> bool {
    // YYYY-MM-DD[Z|(+|-)hh:mm]
    let s = strip_timezone(s);
    let parts: Vec<&str> = s.split('-').collect();
    // Handle negative years
    if s.starts_with('-') {
        if parts.len() < 4 {
            return false;
        }
        // parts[0] is empty, parts[1] is year, parts[2] month, parts[3] day
        if parts[1].len() < 4 || !parts[1].chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if parts[2].len() != 2 || parts[3].len() != 2 {
            return false;
        }
        let year: u32 = match parts[1].parse() {
            Ok(y) => y,
            Err(_) => return false,
        };
        let month: u32 = match parts[2].parse() {
            Ok(m) => m,
            Err(_) => return false,
        };
        let day: u32 = match parts[3].parse() {
            Ok(d) => d,
            Err(_) => return false,
        };
        // year 0000 is invalid in XSD (no year zero)
        if year == 0 {
            return false;
        }
        if !(1..=12).contains(&month) {
            return false;
        }
        if day < 1 || day > max_days_for_month_year(month, year) {
            return false;
        }
        return true;
    }
    if parts.len() != 3 {
        return false;
    }
    if parts[0].len() < 4
        || !parts[0].chars().all(|c| c.is_ascii_digit())
        || parts[1].len() != 2
        || !parts[1].chars().all(|c| c.is_ascii_digit())
        || parts[2].len() != 2
        || !parts[2].chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = match parts[0].parse() {
        Ok(y) => y,
        Err(_) => return false,
    };
    let month: u32 = match parts[1].parse() {
        Ok(m) => m,
        Err(_) => return false,
    };
    let day: u32 = match parts[2].parse() {
        Ok(d) => d,
        Err(_) => return false,
    };
    // year 0000 is invalid in XSD (no year zero)
    if year == 0 {
        return false;
    }
    if !(1..=12).contains(&month) {
        return false;
    }
    if day < 1 || day > max_days_for_month_year(month, year) {
        return false;
    }
    true
}

/// Validate time format: hh:mm:ss[.sss][Z|(+|-)hh:mm]
///
/// Hours must be 00-24 (24:00:00 is valid as end-of-day midnight).
/// Minutes must be 00-59, seconds must be 00-59. Fractional seconds
/// are allowed. MS tests: time016/017/018 — reject invalid ranges.
pub(crate) fn is_valid_time(s: &str) -> bool {
    // hh:mm:ss[.sss][Z|(+|-)hh:mm]
    let s = strip_time_timezone(s);
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 3 {
        return false;
    }
    // Allow seconds with fractional part
    let seconds_parts: Vec<&str> = parts[2].split('.').collect();
    if parts[0].len() != 2
        || parts[1].len() != 2
        || seconds_parts[0].len() != 2
        || !parts[0].chars().all(|c| c.is_ascii_digit())
        || !parts[1].chars().all(|c| c.is_ascii_digit())
        || !seconds_parts[0].chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    let hours: u32 = match parts[0].parse() {
        Ok(h) => h,
        Err(_) => return false,
    };
    let minutes: u32 = match parts[1].parse() {
        Ok(m) => m,
        Err(_) => return false,
    };
    let seconds: u32 = match seconds_parts[0].parse() {
        Ok(s) => s,
        Err(_) => return false,
    };
    // 24:00:00 is allowed as midnight end-of-day, but nothing else with hour=24
    if hours == 24 {
        return minutes == 0 && seconds == 0;
    }
    hours <= 23 && minutes <= 59 && seconds <= 59
}

/// Strip timezone from a time-only string (hh:mm:ss[.sss][Z|(+|-)hh:mm]).
///
/// Returns the time string without any timezone suffix.
fn strip_time_timezone(s: &str) -> &str {
    if s.ends_with('Z') {
        return &s[..s.len() - 1];
    }
    // Look for timezone offset: +hh:mm or -hh:mm at the end
    // A timezone offset has the form [+-]dd:dd at the end (6 chars)
    if s.len() >= 6 {
        let tz_start = s.len() - 6;
        let c = s.as_bytes()[tz_start];
        if (c == b'+' || c == b'-') && s.as_bytes()[tz_start + 3] == b':' {
            return &s[..tz_start];
        }
    }
    s
}

/// Strip timezone suffix from date strings.
///
/// Timezone is Z, +hh:mm, or -hh:mm at the end.
/// MS tests: gYearMonth003, gYear006, gMonthDay003, gDay003, gMonth004 —
/// the old `pos > 8` heuristic failed for short types like gYear, gDay.
pub(crate) fn strip_timezone(s: &str) -> &str {
    if s.ends_with('Z') {
        return &s[..s.len() - 1];
    }
    // Check for +hh:mm or -hh:mm at the end (exactly 6 chars: [+-]dd:dd)
    if s.len() >= 6 {
        let tz_start = s.len() - 6;
        let b = s.as_bytes();
        if (b[tz_start] == b'+' || b[tz_start] == b'-')
            && b[tz_start + 1].is_ascii_digit()
            && b[tz_start + 2].is_ascii_digit()
            && b[tz_start + 3] == b':'
            && b[tz_start + 4].is_ascii_digit()
            && b[tz_start + 5].is_ascii_digit()
        {
            return &s[..tz_start];
        }
    }
    s
}
