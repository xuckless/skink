//! Skink columns are Unicode scalar values ("chars"); LSP columns are UTF-16
//! code units (the only position encoding Skink negotiates). These helpers
//! convert within a single line (no trailing newline).

/// Number of UTF-16 code units in `line`.
pub fn utf16_len(line: &str) -> u32 {
    line.chars().map(|c| c.len_utf16() as u32).sum()
}

/// UTF-16 column of the char column `col` (clamped to the end of the line).
pub fn char_to_utf16(line: &str, col: usize) -> u32 {
    line.chars().take(col).map(|c| c.len_utf16() as u32).sum()
}

/// Char column of the UTF-16 column `col16`. Columns past the end clamp to the
/// line length; a column pointing into the middle of a surrogate pair rounds
/// down to that character's start.
pub fn utf16_to_char(line: &str, col16: u32) -> usize {
    let mut units = 0u32;
    for (i, c) in line.chars().enumerate() {
        let next = units + c.len_utf16() as u32;
        if col16 < next {
            return i;
        }
        units = next;
    }
    line.chars().count()
}

/// Byte offset of char column `col` (clamped to `line.len()`).
pub fn char_to_byte(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_identity() {
        let l = "let x = 1;";
        assert_eq!(char_to_utf16(l, 4), 4);
        assert_eq!(utf16_to_char(l, 4), 4);
        assert_eq!(utf16_len(l), 10);
    }

    #[test]
    fn bmp_non_ascii_is_one_unit() {
        let l = "héllo 你好";
        assert_eq!(char_to_utf16(l, 7), 7);
        assert_eq!(utf16_to_char(l, 7), 7);
    }

    #[test]
    fn astral_is_two_units() {
        let l = "a😀b😀c";
        assert_eq!(utf16_len(l), 7);
        assert_eq!(char_to_utf16(l, 0), 0);
        assert_eq!(char_to_utf16(l, 1), 1);
        assert_eq!(char_to_utf16(l, 2), 3);
        assert_eq!(char_to_utf16(l, 3), 4);
        assert_eq!(char_to_utf16(l, 5), 7);
        assert_eq!(utf16_to_char(l, 1), 1);
        assert_eq!(utf16_to_char(l, 3), 2);
        assert_eq!(utf16_to_char(l, 4), 3);
        assert_eq!(utf16_to_char(l, 7), 5);
    }

    #[test]
    fn mid_surrogate_rounds_down() {
        assert_eq!(utf16_to_char("a😀b", 2), 1);
    }

    #[test]
    fn clamps_past_end() {
        assert_eq!(char_to_utf16("ab", 99), 2);
        assert_eq!(utf16_to_char("ab", 99), 2);
        assert_eq!(utf16_to_char("", 5), 0);
    }

    #[test]
    fn byte_offsets() {
        assert_eq!(char_to_byte("aé😀b", 0), 0);
        assert_eq!(char_to_byte("aé😀b", 2), 3);
        assert_eq!(char_to_byte("aé😀b", 3), 7);
        assert_eq!(char_to_byte("aé😀b", 9), 8);
    }

    #[test]
    fn roundtrip_every_char_boundary() {
        let l = "x😀é你y🎉z";
        for c in 0..=l.chars().count() {
            assert_eq!(utf16_to_char(l, char_to_utf16(l, c)), c);
        }
    }
}
