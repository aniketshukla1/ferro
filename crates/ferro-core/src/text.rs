//! Text helpers per BACKEND.md § 5.2: UTF-8-safe truncation and
//! UTF-8 byte ↔ UTF-16 unit conversions (API columns are UTF-16).

/// Truncate to at most `max_bytes`, backing off to a char boundary.
pub fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate to at most `max_units` UTF-16 code units, backing off to a boundary.
pub fn truncate_utf16(s: &str, max_units: usize) -> &str {
    let total: usize = s.encode_utf16().count();
    if total <= max_units {
        return s;
    }
    let mut units = 0usize;
    let mut end = 0usize;
    for (i, c) in s.char_indices() {
        let w = c.len_utf16();
        if units + w > max_units {
            break;
        }
        units += w;
        end = i + c.len_utf8();
    }
    &s[..end]
}

/// UTF-8 byte offset → UTF-16 unit offset within one line.
pub fn utf8_to_utf16_offset(line: &str, byte: usize) -> usize {
    let byte = byte.min(line.len());
    let mut b = 0usize;
    let mut u = 0usize;
    for c in line.chars() {
        if b >= byte {
            break;
        }
        b += c.len_utf8();
        u += c.len_utf16();
    }
    u
}

/// Convert a UTF-8 byte range to a UTF-16 range, clamped to the line.
pub fn utf8_range_to_utf16(line: &str, start: usize, end: usize) -> (usize, usize) {
    (
        utf8_to_utf16_offset(line, start),
        utf8_to_utf16_offset(line, end.min(line.len())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_on_char_boundaries() {
        // 'é' is 2 bytes in UTF-8, 1 unit in UTF-16.
        let s = "aébc";
        assert_eq!(truncate_utf8(s, 10), "aébc");
        assert_eq!(truncate_utf8(s, 3), "aé");
        assert_eq!(truncate_utf8(s, 2), "a");
        assert_eq!(truncate_utf8(s, 0), "");
        assert_eq!(truncate_utf16(s, 10), "aébc");
        assert_eq!(truncate_utf16(s, 2), "aé");
        assert_eq!(truncate_utf16(s, 1), "a");
    }

    #[test]
    fn offsets_convert() {
        // 'é'(2B,1U) + '😀'(4B,2U)
        let line = "aé😀b";
        assert_eq!(utf8_to_utf16_offset(line, 0), 0);
        assert_eq!(utf8_to_utf16_offset(line, 1), 1);
        assert_eq!(utf8_to_utf16_offset(line, 3), 2);
        assert_eq!(utf8_to_utf16_offset(line, 7), 4);
        assert_eq!(utf8_to_utf16_offset(line, 999), 5);
        assert_eq!(utf8_range_to_utf16(line, 1, 3), (1, 2));
    }
}
