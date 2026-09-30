//! Line-range edits for inline editing (API.md § 4.10). A range is replaced only while it still
//! reads as the editor saw it, and the file keeps its own line endings and final newline.

use std::io::Write;
use std::path::Path;

/// Files larger than this are not edited in place.
pub const MAX_EDIT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
    /// NUL bytes or invalid UTF-8.
    NotText,
    /// `start`/`end` outside the file.
    Range,
    /// The lines changed since the editor read them; carries what they read now.
    Stale(String),
}

#[derive(Debug)]
pub struct Edited {
    pub bytes: Vec<u8>,
    /// Lines in the file afterwards.
    pub lines: usize,
    /// Last line of the replaced range afterwards (`start - 1` when the range was removed).
    pub end: usize,
}

fn strip_eol(l: &str) -> &str {
    match l.strip_suffix('\n') {
        Some(l) => l.strip_suffix('\r').unwrap_or(l),
        None => l,
    }
}

/// The line ending new lines get: CRLF when every terminated line uses it, else LF.
fn eol_of(lines: &[&str]) -> &'static str {
    let mut terminated = lines.iter().filter(|l| l.ends_with('\n')).peekable();
    if terminated.peek().is_some() && terminated.all(|l| l.ends_with("\r\n")) {
        "\r\n"
    } else {
        "\n"
    }
}

/// Lines `start..=end` (1-based) joined with `\n`, or `None` when the range is outside the text.
pub fn range_text(text: &str, start: usize, end: usize) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    if start == 0 || end + 1 < start || end > lines.len() {
        return None;
    }
    Some(
        lines[start - 1..end]
            .iter()
            .map(|l| strip_eol(l))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Replace lines `start..=end` (1-based; `end == start - 1` inserts before `start`) with `text`.
/// `expected` is what the editor showed for the range; a mismatch is `Stale`. Line endings in
/// `expected` and `text` may be LF or CRLF. An empty `text` removes the lines.
pub fn replace_lines(
    bytes: &[u8],
    start: usize,
    end: usize,
    expected: &str,
    text: &str,
) -> Result<Edited, EditError> {
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Err(EditError::NotText);
    }
    let src = std::str::from_utf8(bytes).map_err(|_| EditError::NotText)?;
    let lines: Vec<&str> = src.split_inclusive('\n').collect();
    if start == 0 || end + 1 < start || end > lines.len() {
        return Err(EditError::Range);
    }
    let current = lines[start - 1..end]
        .iter()
        .map(|l| strip_eol(l))
        .collect::<Vec<_>>()
        .join("\n");
    if current != expected.replace("\r\n", "\n") {
        return Err(EditError::Stale(current));
    }
    let eol = eol_of(&lines);
    let text = text.replace("\r\n", "\n");
    let new: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    };
    // A file without a final newline keeps it that way when new lines land at its end.
    let ends_bare = lines.last().is_some_and(|l| !l.ends_with('\n'));
    let mut out = String::with_capacity(src.len() + text.len() + new.len() * 2);
    for l in &lines[..start - 1] {
        out.push_str(l);
    }
    // Appending after a bare last line: terminate it first.
    if !new.is_empty() && !out.is_empty() && !out.ends_with('\n') {
        out.push_str(eol);
    }
    for (i, l) in new.iter().enumerate() {
        out.push_str(l);
        let last_of_file = i + 1 == new.len() && end == lines.len();
        if !(last_of_file && ends_bare) {
            out.push_str(eol);
        }
    }
    for l in &lines[end..] {
        out.push_str(l);
    }
    let total = out.split_inclusive('\n').count();
    Ok(Edited {
        bytes: out.into_bytes(),
        lines: total,
        end: start + new.len() - 1,
    })
}

/// Write `bytes` to `path` through a temp file in the same directory and a rename, keeping the
/// file's permissions, so a crash never leaves it half-written.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let perms = std::fs::metadata(path)?.permissions();
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{name}.ferro-{nonce:x}.tmp"));
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::set_permissions(&tmp, perms)?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(src: &str, a: usize, b: usize, exp: &str, text: &str) -> Result<String, EditError> {
        replace_lines(src.as_bytes(), a, b, exp, text).map(|e| String::from_utf8(e.bytes).unwrap())
    }

    #[test]
    fn replaces_a_range_and_keeps_the_rest() {
        let src = "a\nb\nc\nd\n";
        assert_eq!(edit(src, 2, 3, "b\nc", "B").unwrap(), "a\nB\nd\n");
        assert_eq!(
            edit(src, 2, 2, "b", "b1\nb2\nb3").unwrap(),
            "a\nb1\nb2\nb3\nc\nd\n"
        );
        let e = replace_lines(src.as_bytes(), 2, 2, "b", "x\ny").unwrap();
        assert_eq!((e.lines, e.end), (5, 3));
    }

    #[test]
    fn keeps_crlf_and_a_missing_final_newline() {
        assert_eq!(
            edit("a\r\nb\r\nc", 3, 3, "c", "C\nD").unwrap(),
            "a\r\nb\r\nC\r\nD"
        );
        assert_eq!(edit("a\r\nb\r\n", 1, 1, "a", "A").unwrap(), "A\r\nb\r\n");
        // Mixed endings: new lines get LF.
        assert_eq!(edit("a\r\nb\n", 1, 1, "a", "A").unwrap(), "A\nb\n");
        assert_eq!(edit("a\nb", 2, 2, "b", "B").unwrap(), "a\nB");
    }

    #[test]
    fn inserts_and_removes() {
        assert_eq!(edit("a\nb\n", 2, 1, "", "x").unwrap(), "a\nx\nb\n");
        assert_eq!(edit("a\nb", 3, 2, "", "c").unwrap(), "a\nb\nc");
        assert_eq!(edit("a\nb\nc\n", 2, 2, "b", "").unwrap(), "a\nc\n");
        assert_eq!(edit("", 1, 0, "", "hello").unwrap(), "hello\n");
        let e = replace_lines(b"a\nb\nc\n", 2, 3, "b\nc", "").unwrap();
        assert_eq!((e.lines, e.end), (1, 1));
    }

    #[test]
    fn refuses_stale_ranges_binary_and_bad_ranges() {
        assert_eq!(
            edit("a\nb\n", 1, 1, "z", "x").unwrap_err(),
            EditError::Stale("a".into())
        );
        assert_eq!(edit("a\r\nb\r\n", 1, 2, "a\r\nb", "x").unwrap(), "x\r\n");
        assert_eq!(edit("a\n", 0, 0, "", "x").unwrap_err(), EditError::Range);
        assert_eq!(edit("a\n", 2, 2, "", "x").unwrap_err(), EditError::Range);
        assert_eq!(
            replace_lines(b"a\0b", 1, 1, "a\0b", "x").unwrap_err(),
            EditError::NotText
        );
        assert_eq!(
            replace_lines(&[0xff, 0xfe, b'\n'], 1, 1, "", "x").unwrap_err(),
            EditError::NotText
        );
        assert_eq!(range_text("a\nb\nc", 2, 3).as_deref(), Some("b\nc"));
    }

    #[test]
    fn writes_atomically_and_keeps_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        write_atomic(&p, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755);
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
