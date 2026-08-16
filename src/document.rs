use tower_lsp::lsp_types::Position;

/// Converts an LSP `Position` (line and UTF-16 character offset) to a byte offset within `text`.
pub fn position_to_byte_offset(text: &str, position: Position) -> Option<usize> {
    let mut line_offset = 0;
    for _ in 0..position.line {
        line_offset = text[line_offset..].find('\n')? + line_offset + 1;
    }

    let line_text = &text[line_offset..].lines().next().unwrap_or("");
    let utf16_offset = position.character as usize;
    let mut byte_offset = 0;
    let mut utf16_count = 0;

    for (i, c) in line_text.char_indices() {
        if utf16_count >= utf16_offset {
            byte_offset = i;
            break;
        }
        utf16_count += c.len_utf16();
        if utf16_count >= utf16_offset {
            byte_offset = i + c.len_utf8();
            break;
        }
    }
    if utf16_count < utf16_offset {
        byte_offset = line_text.len();
    }

    Some(line_offset + byte_offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // "あ" is 3 bytes, 1 utf16 code unit
    // "a" is 1 byte, 1 utf16 code unit
    // "😊" is 4 bytes, 2 utf16 code units (surrogate pair)
    // "b" is 1 byte, 1 utf16 code unit
    // Start of line: char 0 -> byte 0
    #[case("あa😊b", 0, 0, Some(0))]
    // End of "あ": char 1 -> byte 3
    #[case("あa😊b", 0, 1, Some(3))]
    // End of "a": char 2 -> byte 4
    #[case("あa😊b", 0, 2, Some(4))]
    // Middle of "😊" (first surrogate): char 3 -> byte 8 (end of emoji)
    // Current implementation returns end of char if offset falls within it.
    #[case("あa😊b", 0, 3, Some(8))]
    // End of "😊": char 4 -> byte 8
    #[case("あa😊b", 0, 4, Some(8))]
    // End of "b": char 5 -> byte 9
    #[case("あa😊b", 0, 5, Some(9))]
    // Beyond line length -> clamped to end of line (byte 9)
    #[case("あa😊b", 0, 6, Some(9))]
    #[case("あa😊b", 0, 100, Some(9))]
    fn test_position_to_byte_offset_utf8_utf16(
        #[case] text: &str,
        #[case] line: u32,
        #[case] character: u32,
        #[case] expected: Option<usize>,
    ) {
        assert_eq!(
            position_to_byte_offset(text, Position::new(line, character)),
            expected
        );
    }

    #[rstest]
    // Normal case (start, middle, end of line 0)
    #[case("line1\nline2", 0, 0, Some(0))]
    #[case("line1\nline2", 0, 2, Some(2))]
    #[case("line1\nline2", 0, 5, Some(5))]
    // Next line (start, middle, end of line 1)
    #[case("line1\nline2", 1, 0, Some(6))]
    #[case("line1\nline2", 1, 2, Some(8))]
    #[case("line1\nline2", 1, 5, Some(11))]
    // Non-existent line
    #[case("line1\nline2", 2, 0, None)]
    // Position beyond line length (should return end of line)
    #[case("line1\nline2", 0, 100, Some(5))]
    #[case("line1\nline2", 1, 100, Some(11))]
    // Empty text
    #[case("", 0, 0, Some(0))]
    #[case("", 0, 1, Some(0))]
    // Single line without newline
    #[case("hello world", 0, 0, Some(0))]
    #[case("hello world", 0, 5, Some(5))]
    #[case("hello world", 0, 11, Some(11))]
    #[case("hello world", 0, 20, Some(11))]
    #[case("hello world", 1, 0, None)]
    fn test_position_to_byte_offset_edge_cases(
        #[case] text: &str,
        #[case] line: u32,
        #[case] character: u32,
        #[case] expected: Option<usize>,
    ) {
        assert_eq!(
            position_to_byte_offset(text, Position::new(line, character)),
            expected
        );
    }

    #[rstest]
    // 1. CRLF basic
    #[case("hello\r\nworld", 0, 0, Some(0))]
    #[case("hello\r\nworld", 0, 5, Some(5))]
    #[case("hello\r\nworld", 0, 6, Some(5))] // clamped
    #[case("hello\r\nworld", 1, 0, Some(7))]
    #[case("hello\r\nworld", 1, 5, Some(12))]
    #[case("hello\r\nworld", 2, 0, None)]
    // 2. CRLF with trailing newline
    #[case("foo\r\nbar\r\n", 0, 3, Some(3))]
    #[case("foo\r\nbar\r\n", 1, 0, Some(5))]
    #[case("foo\r\nbar\r\n", 1, 3, Some(8))]
    #[case("foo\r\nbar\r\n", 2, 0, Some(10))]
    #[case("foo\r\nbar\r\n", 2, 5, Some(10))]
    #[case("foo\r\nbar\r\n", 3, 0, None)]
    // 3. LF with trailing newline
    #[case("foo\nbar\n", 0, 3, Some(3))]
    #[case("foo\nbar\n", 1, 0, Some(4))]
    #[case("foo\nbar\n", 1, 3, Some(7))]
    #[case("foo\nbar\n", 2, 0, Some(8))]
    #[case("foo\nbar\n", 3, 0, None)]
    // 4. Mixed line endings (CRLF and LF)
    #[case("line1\r\nline2\nline3\r\n", 0, 5, Some(5))]
    #[case("line1\r\nline2\nline3\r\n", 1, 0, Some(7))]
    #[case("line1\r\nline2\nline3\r\n", 1, 5, Some(12))]
    #[case("line1\r\nline2\nline3\r\n", 2, 0, Some(13))]
    #[case("line1\r\nline2\nline3\r\n", 2, 5, Some(18))]
    #[case("line1\r\nline2\nline3\r\n", 3, 0, Some(20))]
    #[case("line1\r\nline2\nline3\r\n", 4, 0, None)]
    // 5. Consecutive blank lines (LF)
    #[case("a\n\n\nb", 0, 0, Some(0))]
    #[case("a\n\n\nb", 0, 1, Some(1))]
    #[case("a\n\n\nb", 1, 0, Some(2))]
    #[case("a\n\n\nb", 2, 0, Some(3))]
    #[case("a\n\n\nb", 3, 0, Some(4))]
    #[case("a\n\n\nb", 3, 1, Some(5))]
    #[case("a\n\n\nb", 4, 0, None)]
    // 6. Consecutive blank lines (CRLF)
    #[case("a\r\n\r\n\r\nb", 0, 0, Some(0))]
    #[case("a\r\n\r\n\r\nb", 0, 1, Some(1))]
    #[case("a\r\n\r\n\r\nb", 1, 0, Some(3))]
    #[case("a\r\n\r\n\r\nb", 2, 0, Some(5))]
    #[case("a\r\n\r\n\r\nb", 3, 0, Some(7))]
    #[case("a\r\n\r\n\r\nb", 3, 1, Some(8))]
    #[case("a\r\n\r\n\r\nb", 4, 0, None)]
    // 7. Blank lines only
    #[case("\n\n", 0, 0, Some(0))]
    #[case("\n\n", 1, 0, Some(1))]
    #[case("\n\n", 2, 0, Some(2))]
    #[case("\n\n", 3, 0, None)]
    #[case("\r\n\r\n", 0, 0, Some(0))]
    #[case("\r\n\r\n", 1, 0, Some(2))]
    #[case("\r\n\r\n", 2, 0, Some(4))]
    #[case("\r\n\r\n", 3, 0, None)]
    fn test_position_to_byte_offset_line_endings(
        #[case] text: &str,
        #[case] line: u32,
        #[case] character: u32,
        #[case] expected: Option<usize>,
    ) {
        assert_eq!(
            position_to_byte_offset(text, Position::new(line, character)),
            expected
        );
    }

    #[rstest]
    // 1. Multi-line CJK text
    // Line 0: "こんにちは" (5 chars, 15 bytes, 5 utf16 units)
    #[case("こんにちは\nカタカナ\n漢字と記号：！", 0, 0, Some(0))]
    #[case("こんにちは\nカタカナ\n漢字と記号：！", 0, 1, Some(3))]
    #[case("こんにちは\nカタカナ\n漢字と記号：！", 0, 3, Some(9))]
    #[case("こんにちは\nカタカナ\n漢字と記号：！", 0, 5, Some(15))]
    // Line 1: "カタカナ" (4 chars, 12 bytes, 4 utf16 units, line_offset = 16)
    #[case("こんにちは\nカタカナ\n漢字と記号：！", 1, 0, Some(16))]
    #[case("こんにちは\nカタカナ\n漢字と記号：！", 1, 2, Some(22))]
    #[case("こんにちは\nカタカナ\n漢字と記号：！", 1, 4, Some(28))]
    // Line 2: "漢字と記号：！" (7 chars, 21 bytes, 7 utf16 units, line_offset = 29)
    #[case("こんにちは\nカタカナ\n漢字と記号：！", 2, 0, Some(29))]
    #[case("こんにちは\nカタカナ\n漢字と記号：！", 2, 7, Some(50))]
    // 2. Surrogate pairs SIP/SMP (e.g. '𩸽' U+29E3D, 4B utf-8, 2 utf-16)
    // '魚' (3B, 1 utf16), '𩸽' (4B, 2 utf16), ' ' (1B, 1 utf16), '(' (1B, 1 utf16), 'ホ' (3B, 1 utf16), 'ッ' (3B, 1 utf16), 'ケ' (3B, 1 utf16), ')' (1B, 1 utf16)
    #[case("魚𩸽 (ホッケ)", 0, 0, Some(0))]
    #[case("魚𩸽 (ホッケ)", 0, 1, Some(3))]
    #[case("魚𩸽 (ホッケ)", 0, 2, Some(7))] // mid surrogate snaps to end of char
    #[case("魚𩸽 (ホッケ)", 0, 3, Some(7))]
    #[case("魚𩸽 (ホッケ)", 0, 4, Some(8))]
    #[case("魚𩸽 (ホッケ)", 0, 8, Some(18))]
    #[case("魚𩸽 (ホッケ)", 0, 9, Some(19))]
    // 3. Emojis with skin tone modifiers
    // 👍 (U+1F44D, 4B, 2 utf16) + 🏽 (U+1F3FD, 4B, 2 utf16) = 8 bytes, 4 utf16 units
    #[case("👍🏽", 0, 0, Some(0))]
    #[case("👍🏽", 0, 1, Some(4))]
    #[case("👍🏽", 0, 2, Some(4))]
    #[case("👍🏽", 0, 3, Some(8))]
    #[case("👍🏽", 0, 4, Some(8))]
    // 4. Emoji ZWJ sequences: 👨‍👩‍👧‍👦 (25 bytes, 11 utf16 units)
    #[case("👨‍👩‍👧‍👦", 0, 0, Some(0))]
    #[case("👨‍👩‍👧‍👦", 0, 1, Some(4))]
    #[case("👨‍👩‍👧‍👦", 0, 2, Some(4))]
    #[case("👨‍👩‍👧‍👦", 0, 3, Some(7))]
    #[case("👨‍👩‍👧‍👦", 0, 4, Some(11))]
    #[case("👨‍👩‍👧‍👦", 0, 5, Some(11))]
    #[case("👨‍👩‍👧‍👦", 0, 6, Some(14))]
    #[case("👨‍👩‍👧‍👦", 0, 7, Some(18))]
    #[case("👨‍👩‍👧‍👦", 0, 8, Some(18))]
    #[case("👨‍👩‍👧‍👦", 0, 9, Some(21))]
    #[case("👨‍👩‍👧‍👦", 0, 10, Some(25))]
    #[case("👨‍👩‍👧‍👦", 0, 11, Some(25))]
    #[case("👨‍👩‍👧‍👦", 0, 12, Some(25))]
    // 5. Combining diacritical marks
    // 'e' (1B, 1u), '\u{0301}' (2B, 1u), " and " (5B, 5u), 'か' (3B, 1u), '\u{3099}' (3B, 1u) -> Total: 14B, 9u
    #[case("e\u{0301} and か\u{3099}", 0, 0, Some(0))]
    #[case("e\u{0301} and か\u{3099}", 0, 1, Some(1))]
    #[case("e\u{0301} and か\u{3099}", 0, 2, Some(3))]
    #[case("e\u{0301} and か\u{3099}", 0, 7, Some(8))]
    #[case("e\u{0301} and か\u{3099}", 0, 8, Some(11))]
    #[case("e\u{0301} and か\u{3099}", 0, 9, Some(14))]
    #[case("e\u{0301} and か\u{3099}", 0, 10, Some(14))]
    fn test_position_to_byte_offset_multibyte_advanced(
        #[case] text: &str,
        #[case] line: u32,
        #[case] character: u32,
        #[case] expected: Option<usize>,
    ) {
        assert_eq!(
            position_to_byte_offset(text, Position::new(line, character)),
            expected
        );
    }

    #[rstest]
    // 1. Empty document
    #[case("", 0, 0, Some(0))]
    #[case("", 0, 1, Some(0))]
    #[case("", 0, 100, Some(0))]
    #[case("", 1, 0, None)]
    #[case("", 1, 1, None)]
    // 2. Single char documents
    #[case("x", 0, 0, Some(0))]
    #[case("x", 0, 1, Some(1))]
    #[case("x", 0, 2, Some(1))]
    #[case("x", 1, 0, None)]
    #[case("あ", 0, 0, Some(0))]
    #[case("あ", 0, 1, Some(3))]
    #[case("あ", 0, 2, Some(3))]
    #[case("あ", 1, 0, None)]
    #[case("🎉", 0, 0, Some(0))]
    #[case("🎉", 0, 1, Some(4))]
    #[case("🎉", 0, 2, Some(4))]
    #[case("🎉", 0, 3, Some(4))]
    #[case("🎉", 1, 0, None)]
    // 3. Out-of-bounds line numbers
    #[case("first\nsecond", 10, 0, None)]
    #[case("first\nsecond", u32::MAX, 0, None)]
    #[case("first\nsecond", u32::MAX, u32::MAX, None)]
    // 4. Extreme character offsets (u32::MAX)
    #[case("first\nsecond", 0, u32::MAX, Some(5))]
    #[case("first\nsecond", 1, u32::MAX, Some(12))]
    fn test_position_to_byte_offset_boundaries_and_out_of_bounds(
        #[case] text: &str,
        #[case] line: u32,
        #[case] character: u32,
        #[case] expected: Option<usize>,
    ) {
        assert_eq!(
            position_to_byte_offset(text, Position::new(line, character)),
            expected
        );
    }
}
