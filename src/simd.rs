//! SIMD-accelerated byte scanning for parser hot loops.
//!
//! On x86_64 (where SSE2 is guaranteed), text content and attribute values are
//! scanned 16 bytes at a time instead of 1. Other architectures use a scalar
//! fallback with inline byte comparisons.

/// Scan `data` for content delimiter bytes (`<`, `&`, `\r`, `]`).
///
/// Returns `(bytes_advanced, needs_validation)` where `needs_validation` is true
/// if any non-ASCII byte (>= 0x80) or illegal control character (< 0x20 except
/// TAB, LF) was encountered in the scanned range.
pub(crate) fn scan_content_delimiters(data: &[u8]) -> (usize, bool) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is guaranteed on all x86_64 processors.
        unsafe { scan_content_sse2(data) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        scan_content_scalar(data)
    }
}

/// Scan `data` for attribute delimiter bytes (`&`, `<`) or the closing `quote` byte.
///
/// Returns `(bytes_advanced, needs_validation)` where `needs_validation` is true
/// if any non-ASCII byte or illegal control character was encountered.
pub(crate) fn scan_attr_delimiters(data: &[u8], quote: u8) -> (usize, bool) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is guaranteed on all x86_64 processors.
        unsafe { scan_attr_sse2(data, quote) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        scan_attr_scalar(data, quote)
    }
}

// ---------------------------------------------------------------------------
// SSE2 implementations (x86_64 only)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn scan_content_sse2(data: &[u8]) -> (usize, bool) {
    use std::arch::x86_64::*;

    let mut pos = 0;
    let mut needs_validation = false;

    // Broadcast delimiter bytes to all 16 lanes
    let v_lt = _mm_set1_epi8(b'<' as i8);
    let v_amp = _mm_set1_epi8(b'&' as i8);
    let v_cr = _mm_set1_epi8(b'\r' as i8);
    let v_rsq = _mm_set1_epi8(b']' as i8);

    // For control-char detection: bytes <= 0x1F excluding TAB(0x09), LF(0x0A), CR(0x0D)
    let v_1f = _mm_set1_epi8(0x1F_u8 as i8);
    let v_tab = _mm_set1_epi8(0x09);
    let v_lf = _mm_set1_epi8(0x0A);

    while pos + 16 <= data.len() {
        let chunk = _mm_loadu_si128(data.as_ptr().add(pos) as *const __m128i);

        // OR together all delimiter equality checks
        let eq_cr = _mm_cmpeq_epi8(chunk, v_cr);
        let delimiters = _mm_or_si128(
            _mm_or_si128(_mm_cmpeq_epi8(chunk, v_lt), _mm_cmpeq_epi8(chunk, v_amp)),
            _mm_or_si128(eq_cr, _mm_cmpeq_epi8(chunk, v_rsq)),
        );
        let delim_mask = _mm_movemask_epi8(delimiters) as u32;

        // Check for bytes needing XML char validation (skip once flag is set)
        if !needs_validation {
            // Non-ASCII: high bit set (byte >= 0x80)
            let hi_bits = _mm_movemask_epi8(chunk) as u32;
            // Control chars: byte <= 0x1F, excluding TAB, LF, and CR
            // (CR is a delimiter so it won't matter, but excluding it avoids
            // false positives when \r is the stopping delimiter)
            let le_1f = _mm_cmpeq_epi8(_mm_min_epu8(chunk, v_1f), chunk);
            let allowed = _mm_or_si128(
                _mm_cmpeq_epi8(chunk, v_tab),
                _mm_or_si128(_mm_cmpeq_epi8(chunk, v_lf), eq_cr),
            );
            let bad_ctrl = _mm_andnot_si128(allowed, le_1f);
            let ctrl_bits = _mm_movemask_epi8(bad_ctrl) as u32;
            if hi_bits != 0 || ctrl_bits != 0 {
                needs_validation = true;
            }
        }

        if delim_mask != 0 {
            return (pos + delim_mask.trailing_zeros() as usize, needs_validation);
        }
        pos += 16;
    }

    // Scalar tail for remaining < 16 bytes
    let (tail_advance, tail_flag) = scan_content_scalar(&data[pos..]);
    (pos + tail_advance, needs_validation || tail_flag)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn scan_attr_sse2(data: &[u8], quote: u8) -> (usize, bool) {
    use std::arch::x86_64::*;

    let mut pos = 0;
    let mut needs_validation = false;

    // Broadcast delimiter bytes
    let v_amp = _mm_set1_epi8(b'&' as i8);
    let v_lt = _mm_set1_epi8(b'<' as i8);
    let v_quote = _mm_set1_epi8(quote as i8);

    // For control-char detection: bytes <= 0x1F excluding TAB, LF, CR
    let v_1f = _mm_set1_epi8(0x1F_u8 as i8);
    let v_tab = _mm_set1_epi8(0x09);
    let v_lf = _mm_set1_epi8(0x0A);
    let v_cr = _mm_set1_epi8(0x0D);

    while pos + 16 <= data.len() {
        let chunk = _mm_loadu_si128(data.as_ptr().add(pos) as *const __m128i);

        // OR together delimiter equality checks (quote, &, <)
        let delimiters = _mm_or_si128(
            _mm_cmpeq_epi8(chunk, v_quote),
            _mm_or_si128(_mm_cmpeq_epi8(chunk, v_amp), _mm_cmpeq_epi8(chunk, v_lt)),
        );
        let delim_mask = _mm_movemask_epi8(delimiters) as u32;

        if !needs_validation {
            let hi_bits = _mm_movemask_epi8(chunk) as u32;
            let le_1f = _mm_cmpeq_epi8(_mm_min_epu8(chunk, v_1f), chunk);
            let allowed = _mm_or_si128(
                _mm_cmpeq_epi8(chunk, v_tab),
                _mm_or_si128(_mm_cmpeq_epi8(chunk, v_lf), _mm_cmpeq_epi8(chunk, v_cr)),
            );
            let bad_ctrl = _mm_andnot_si128(allowed, le_1f);
            let ctrl_bits = _mm_movemask_epi8(bad_ctrl) as u32;
            if hi_bits != 0 || ctrl_bits != 0 {
                needs_validation = true;
            }
        }

        if delim_mask != 0 {
            return (pos + delim_mask.trailing_zeros() as usize, needs_validation);
        }
        pos += 16;
    }

    // Scalar tail
    let (tail_advance, tail_flag) = scan_attr_scalar(&data[pos..], quote);
    (pos + tail_advance, needs_validation || tail_flag)
}

// ---------------------------------------------------------------------------
// Scalar fallback (used on non-x86_64 and for SIMD tail bytes)
// ---------------------------------------------------------------------------

fn scan_content_scalar(data: &[u8]) -> (usize, bool) {
    let mut pos = 0;
    let mut needs_validation = false;
    while pos < data.len() {
        let b = data[pos];
        if b == b'<' || b == b'&' || b == b'\r' || b == b']' {
            break;
        }
        if b >= 0x80 || (b < 0x20 && b != 0x09 && b != 0x0A) {
            needs_validation = true;
        }
        pos += 1;
    }
    (pos, needs_validation)
}

fn scan_attr_scalar(data: &[u8], quote: u8) -> (usize, bool) {
    let mut pos = 0;
    let mut needs_validation = false;
    while pos < data.len() {
        let b = data[pos];
        if b == quote || b == b'&' || b == b'<' {
            break;
        }
        if b >= 0x80 || (b < 0x20 && b != 0x09 && b != 0x0A && b != 0x0D) {
            needs_validation = true;
        }
        pos += 1;
    }
    (pos, needs_validation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_stops_at_lt() {
        let data = b"hello<world";
        let (pos, flag) = scan_content_delimiters(data);
        assert_eq!(pos, 5);
        assert!(!flag);
    }

    #[test]
    fn content_stops_at_amp() {
        let data = b"hello&world";
        let (pos, flag) = scan_content_delimiters(data);
        assert_eq!(pos, 5);
        assert!(!flag);
    }

    #[test]
    fn content_stops_at_cr() {
        let data = b"hello\rworld";
        let (pos, flag) = scan_content_delimiters(data);
        assert_eq!(pos, 5);
        assert!(!flag);
    }

    #[test]
    fn content_stops_at_bracket() {
        let data = b"hello]world";
        let (pos, flag) = scan_content_delimiters(data);
        assert_eq!(pos, 5);
        assert!(!flag);
    }

    #[test]
    fn content_scans_full_ascii() {
        let data = b"hello world 12345";
        let (pos, flag) = scan_content_delimiters(data);
        assert_eq!(pos, data.len());
        assert!(!flag);
    }

    #[test]
    fn content_detects_non_ascii() {
        let data = "hello wörld<".as_bytes();
        let (pos, flag) = scan_content_delimiters(data);
        // 'ö' is 2 bytes in UTF-8, so "hello wörld" = 12 bytes, then '<' at byte 12
        assert_eq!(pos, 12);
        assert!(flag);
    }

    #[test]
    fn content_detects_control_char() {
        // "hello" (5) + \x01 (1) + "world" (5) + "<" (1) = 12 bytes; '<' at index 11
        let data = b"hello\x01world<";
        let (pos, flag) = scan_content_delimiters(data);
        assert_eq!(pos, 11);
        assert!(flag);
    }

    #[test]
    fn content_allows_tab_and_lf() {
        let data = b"hello\tworld\n<";
        let (pos, flag) = scan_content_delimiters(data);
        assert_eq!(pos, 12);
        assert!(!flag);
    }

    #[test]
    fn content_empty_input() {
        let (pos, flag) = scan_content_delimiters(b"");
        assert_eq!(pos, 0);
        assert!(!flag);
    }

    #[test]
    fn content_long_text_with_delimiter() {
        // 32 clean bytes then a delimiter — exercises SIMD + tail
        let mut data = vec![b'a'; 32];
        data.push(b'<');
        let (pos, flag) = scan_content_delimiters(&data);
        assert_eq!(pos, 32);
        assert!(!flag);
    }

    #[test]
    fn content_long_text_no_delimiter() {
        let data = vec![b'x'; 100];
        let (pos, flag) = scan_content_delimiters(&data);
        assert_eq!(pos, 100);
        assert!(!flag);
    }

    #[test]
    fn attr_stops_at_double_quote() {
        let data = b"hello\"world";
        let (pos, flag) = scan_attr_delimiters(data, b'"');
        assert_eq!(pos, 5);
        assert!(!flag);
    }

    #[test]
    fn attr_stops_at_single_quote() {
        let data = b"hello'world";
        let (pos, flag) = scan_attr_delimiters(data, b'\'');
        assert_eq!(pos, 5);
        assert!(!flag);
    }

    #[test]
    fn attr_stops_at_amp() {
        let data = b"hello&world";
        let (pos, flag) = scan_attr_delimiters(data, b'"');
        assert_eq!(pos, 5);
        assert!(!flag);
    }

    #[test]
    fn attr_stops_at_lt() {
        let data = b"hello<world";
        let (pos, flag) = scan_attr_delimiters(data, b'"');
        assert_eq!(pos, 5);
        assert!(!flag);
    }

    #[test]
    fn attr_allows_cr_without_flagging() {
        // CR is not a stop byte for attr scan, and is allowed whitespace
        let data = b"hello\rworld\"";
        let (pos, flag) = scan_attr_delimiters(data, b'"');
        assert_eq!(pos, 11);
        assert!(!flag);
    }

    #[test]
    fn attr_detects_non_ascii() {
        let data = "héllo\"".as_bytes();
        let (pos, flag) = scan_attr_delimiters(data, b'"');
        // 'é' is 2 bytes, so "héllo" = 6 bytes
        assert_eq!(pos, 6);
        assert!(flag);
    }

    #[test]
    fn attr_detects_control_char() {
        let data = b"hel\x02lo\"";
        let (pos, flag) = scan_attr_delimiters(data, b'"');
        assert_eq!(pos, 6);
        assert!(flag);
    }

    #[test]
    fn attr_long_text() {
        let mut data = vec![b'z'; 50];
        data.push(b'"');
        let (pos, flag) = scan_attr_delimiters(&data, b'"');
        assert_eq!(pos, 50);
        assert!(!flag);
    }
}
