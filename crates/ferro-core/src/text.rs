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

/// The last (at most) `max_bytes` of `s`, moving forward to a char boundary (output tails).
pub fn truncate_utf8_tail(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
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

/// Byte offsets of every line start in `bytes`, the first being 0. memchr's SIMD search does
/// the scan, so callers built for size (ferro-server) still read big files at full speed.
pub fn line_starts(bytes: &[u8]) -> Vec<u64> {
    let mut offs = Vec::with_capacity(bytes.len() / 48 + 1);
    offs.push(0);
    offs.extend(memchr::memchr_iter(b'\n', bytes).map(|i| (i + 1) as u64));
    offs
}

#[cfg(test)]
mod tests {
    #[test]
    fn line_starts_finds_every_line() {
        assert_eq!(line_starts(b""), vec![0]);
        assert_eq!(line_starts(b"one"), vec![0]);
        assert_eq!(line_starts(b"a\nbb\r\n\nccc"), vec![0, 2, 6, 7]);
        assert_eq!(
            line_starts(b"x\n"),
            vec![0, 2],
            "the trailing newline's empty line is the caller's to drop"
        );
        let long = "line\n".repeat(10_000);
        let offs = line_starts(long.as_bytes());
        assert_eq!((offs.len(), offs[9_999]), (10_001, 49_995));
    }

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
