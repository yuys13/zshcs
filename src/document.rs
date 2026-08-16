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

    #[test]
    fn test_position_to_byte_offset_utf8_utf16() {
        let text = "あa😊b";
        // "あ" is 3 bytes, 1 utf16 code unit
        // "a" is 1 byte, 1 utf16 code unit
        // "😊" is 4 bytes, 2 utf16 code units (surrogate pair)
        // "b" is 1 byte, 1 utf16 code unit

        // End of "あ": char 1 -> byte 3
        assert_eq!(position_to_byte_offset(text, Position::new(0, 1)), Some(3));
        // End of "a": char 2 -> byte 4
        assert_eq!(position_to_byte_offset(text, Position::new(0, 2)), Some(4));
        // Middle of "😊" (first surrogate): char 3 -> byte 8 (end of emoji)
        // Current implementation returns end of char if offset falls within it.
        assert_eq!(position_to_byte_offset(text, Position::new(0, 3)), Some(8));
        // End of "😊": char 4 -> byte 8
        assert_eq!(position_to_byte_offset(text, Position::new(0, 4)), Some(8));
        // End of "b": char 5 -> byte 9
        assert_eq!(position_to_byte_offset(text, Position::new(0, 5)), Some(9));
    }

    #[test]
    fn test_position_to_byte_offset_edge_cases() {
        let text = "line1\nline2";
        // Normal case
        assert_eq!(position_to_byte_offset(text, Position::new(0, 5)), Some(5));
        // Next line
        assert_eq!(position_to_byte_offset(text, Position::new(1, 0)), Some(6));
        // Non-existent line
        assert_eq!(position_to_byte_offset(text, Position::new(2, 0)), None);
        // Position beyond line length (should return end of line)
        assert_eq!(
            position_to_byte_offset(text, Position::new(0, 100)),
            Some(5)
        );
        // Empty text
        assert_eq!(position_to_byte_offset("", Position::new(0, 0)), Some(0));
        assert_eq!(position_to_byte_offset("", Position::new(0, 1)), Some(0));
    }

    #[test]
    fn test_position_to_byte_offset_line_endings() {
        // 1. CRLF basic
        let crlf_text = "hello\r\nworld";
        assert_eq!(
            position_to_byte_offset(crlf_text, Position::new(0, 0)),
            Some(0)
        );
        assert_eq!(
            position_to_byte_offset(crlf_text, Position::new(0, 5)),
            Some(5)
        );
        assert_eq!(
            position_to_byte_offset(crlf_text, Position::new(0, 6)),
            Some(5)
        ); // clamped
        assert_eq!(
            position_to_byte_offset(crlf_text, Position::new(1, 0)),
            Some(7)
        );
        assert_eq!(
            position_to_byte_offset(crlf_text, Position::new(1, 5)),
            Some(12)
        );
        assert_eq!(
            position_to_byte_offset(crlf_text, Position::new(2, 0)),
            None
        );

        // 2. CRLF with trailing newline
        let crlf_trailing = "foo\r\nbar\r\n";
        assert_eq!(
            position_to_byte_offset(crlf_trailing, Position::new(0, 3)),
            Some(3)
        );
        assert_eq!(
            position_to_byte_offset(crlf_trailing, Position::new(1, 0)),
            Some(5)
        );
        assert_eq!(
            position_to_byte_offset(crlf_trailing, Position::new(1, 3)),
            Some(8)
        );
        assert_eq!(
            position_to_byte_offset(crlf_trailing, Position::new(2, 0)),
            Some(10)
        );
        assert_eq!(
            position_to_byte_offset(crlf_trailing, Position::new(2, 5)),
            Some(10)
        );
        assert_eq!(
            position_to_byte_offset(crlf_trailing, Position::new(3, 0)),
            None
        );

        // 3. LF with trailing newline
        let lf_trailing = "foo\nbar\n";
        assert_eq!(
            position_to_byte_offset(lf_trailing, Position::new(0, 3)),
            Some(3)
        );
        assert_eq!(
            position_to_byte_offset(lf_trailing, Position::new(1, 0)),
            Some(4)
        );
        assert_eq!(
            position_to_byte_offset(lf_trailing, Position::new(1, 3)),
            Some(7)
        );
        assert_eq!(
            position_to_byte_offset(lf_trailing, Position::new(2, 0)),
            Some(8)
        );
        assert_eq!(
            position_to_byte_offset(lf_trailing, Position::new(3, 0)),
            None
        );

        // 4. Mixed line endings (CRLF and LF)
        let mixed_text = "line1\r\nline2\nline3\r\n";
        assert_eq!(
            position_to_byte_offset(mixed_text, Position::new(0, 5)),
            Some(5)
        );
        assert_eq!(
            position_to_byte_offset(mixed_text, Position::new(1, 0)),
            Some(7)
        );
        assert_eq!(
            position_to_byte_offset(mixed_text, Position::new(1, 5)),
            Some(12)
        );
        assert_eq!(
            position_to_byte_offset(mixed_text, Position::new(2, 0)),
            Some(13)
        );
        assert_eq!(
            position_to_byte_offset(mixed_text, Position::new(2, 5)),
            Some(18)
        );
        assert_eq!(
            position_to_byte_offset(mixed_text, Position::new(3, 0)),
            Some(20)
        );
        assert_eq!(
            position_to_byte_offset(mixed_text, Position::new(4, 0)),
            None
        );

        // 5. Consecutive blank lines (LF)
        let consecutive_lf = "a\n\n\nb";
        assert_eq!(
            position_to_byte_offset(consecutive_lf, Position::new(0, 0)),
            Some(0)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_lf, Position::new(0, 1)),
            Some(1)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_lf, Position::new(1, 0)),
            Some(2)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_lf, Position::new(2, 0)),
            Some(3)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_lf, Position::new(3, 0)),
            Some(4)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_lf, Position::new(3, 1)),
            Some(5)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_lf, Position::new(4, 0)),
            None
        );

        // 6. Consecutive blank lines (CRLF)
        let consecutive_crlf = "a\r\n\r\n\r\nb";
        assert_eq!(
            position_to_byte_offset(consecutive_crlf, Position::new(0, 0)),
            Some(0)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_crlf, Position::new(0, 1)),
            Some(1)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_crlf, Position::new(1, 0)),
            Some(3)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_crlf, Position::new(2, 0)),
            Some(5)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_crlf, Position::new(3, 0)),
            Some(7)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_crlf, Position::new(3, 1)),
            Some(8)
        );
        assert_eq!(
            position_to_byte_offset(consecutive_crlf, Position::new(4, 0)),
            None
        );

        // 7. Blank lines only
        let blank_lf = "\n\n";
        assert_eq!(
            position_to_byte_offset(blank_lf, Position::new(0, 0)),
            Some(0)
        );
        assert_eq!(
            position_to_byte_offset(blank_lf, Position::new(1, 0)),
            Some(1)
        );
        assert_eq!(
            position_to_byte_offset(blank_lf, Position::new(2, 0)),
            Some(2)
        );
        assert_eq!(position_to_byte_offset(blank_lf, Position::new(3, 0)), None);

        let blank_crlf = "\r\n\r\n";
        assert_eq!(
            position_to_byte_offset(blank_crlf, Position::new(0, 0)),
            Some(0)
        );
        assert_eq!(
            position_to_byte_offset(blank_crlf, Position::new(1, 0)),
            Some(2)
        );
        assert_eq!(
            position_to_byte_offset(blank_crlf, Position::new(2, 0)),
            Some(4)
        );
        assert_eq!(
            position_to_byte_offset(blank_crlf, Position::new(3, 0)),
            None
        );
    }

    #[test]
    fn test_position_to_byte_offset_multibyte_advanced() {
        // 1. Multi-line CJK text
        let cjk_text = "こんにちは\nカタカナ\n漢字と記号：！";
        // Line 0: "こんにちは" (5 chars, 15 bytes, 5 utf16 units)
        assert_eq!(
            position_to_byte_offset(cjk_text, Position::new(0, 0)),
            Some(0)
        );
        assert_eq!(
            position_to_byte_offset(cjk_text, Position::new(0, 1)),
            Some(3)
        );
        assert_eq!(
            position_to_byte_offset(cjk_text, Position::new(0, 3)),
            Some(9)
        );
        assert_eq!(
            position_to_byte_offset(cjk_text, Position::new(0, 5)),
            Some(15)
        );

        // Line 1: "カタカナ" (4 chars, 12 bytes, 4 utf16 units, line_offset = 16)
        assert_eq!(
            position_to_byte_offset(cjk_text, Position::new(1, 0)),
            Some(16)
        );
        assert_eq!(
            position_to_byte_offset(cjk_text, Position::new(1, 2)),
            Some(22)
        );
        assert_eq!(
            position_to_byte_offset(cjk_text, Position::new(1, 4)),
            Some(28)
        );

        // Line 2: "漢字と記号：！" (7 chars, 21 bytes, 7 utf16 units, line_offset = 29)
        assert_eq!(
            position_to_byte_offset(cjk_text, Position::new(2, 0)),
            Some(29)
        );
        assert_eq!(
            position_to_byte_offset(cjk_text, Position::new(2, 7)),
            Some(50)
        );

        // 2. Surrogate pairs SIP/SMP (e.g. '𩸽' U+29E3D, 4B utf-8, 2 utf-16)
        let surrogate_text = "魚𩸽 (ホッケ)";
        // '魚' (3B, 1 utf16), '𩸽' (4B, 2 utf16), ' ' (1B, 1 utf16), '(' (1B, 1 utf16), 'ホ' (3B, 1 utf16), 'ッ' (3B, 1 utf16), 'ケ' (3B, 1 utf16), ')' (1B, 1 utf16)
        assert_eq!(
            position_to_byte_offset(surrogate_text, Position::new(0, 0)),
            Some(0)
        );
        assert_eq!(
            position_to_byte_offset(surrogate_text, Position::new(0, 1)),
            Some(3)
        );
        assert_eq!(
            position_to_byte_offset(surrogate_text, Position::new(0, 2)),
            Some(7)
        ); // mid surrogate snaps to end of char
        assert_eq!(
            position_to_byte_offset(surrogate_text, Position::new(0, 3)),
            Some(7)
        );
        assert_eq!(
            position_to_byte_offset(surrogate_text, Position::new(0, 4)),
            Some(8)
        );
        assert_eq!(
            position_to_byte_offset(surrogate_text, Position::new(0, 8)),
            Some(18)
        );
        assert_eq!(
            position_to_byte_offset(surrogate_text, Position::new(0, 9)),
            Some(19)
        );

        // 3. Emojis with skin tone modifiers
        // 👍 (U+1F44D, 4B, 2 utf16) + 🏽 (U+1F3FD, 4B, 2 utf16) = 8 bytes, 4 utf16 units
        let skin_tone_text = "👍🏽";
        assert_eq!(
            position_to_byte_offset(skin_tone_text, Position::new(0, 0)),
            Some(0)
        );
        assert_eq!(
            position_to_byte_offset(skin_tone_text, Position::new(0, 1)),
            Some(4)
        );
        assert_eq!(
            position_to_byte_offset(skin_tone_text, Position::new(0, 2)),
            Some(4)
        );
        assert_eq!(
            position_to_byte_offset(skin_tone_text, Position::new(0, 3)),
            Some(8)
        );
        assert_eq!(
            position_to_byte_offset(skin_tone_text, Position::new(0, 4)),
            Some(8)
        );

        // 4. Emoji ZWJ sequences: 👨‍👩‍👧‍👦 (25 bytes, 11 utf16 units)
        let zwj_text = "👨‍👩‍👧‍👦";
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 0)),
            Some(0)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 1)),
            Some(4)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 2)),
            Some(4)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 3)),
            Some(7)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 4)),
            Some(11)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 5)),
            Some(11)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 6)),
            Some(14)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 7)),
            Some(18)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 8)),
            Some(18)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 9)),
            Some(21)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 10)),
            Some(25)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 11)),
            Some(25)
        );
        assert_eq!(
            position_to_byte_offset(zwj_text, Position::new(0, 12)),
            Some(25)
        );

        // 5. Combining diacritical marks
        let combining_text = "e\u{0301} and か\u{3099}";
        // 'e' (1B, 1u), '\u{0301}' (2B, 1u), " and " (5B, 5u), 'か' (3B, 1u), '\u{3099}' (3B, 1u) -> Total: 14B, 9u
        assert_eq!(
            position_to_byte_offset(combining_text, Position::new(0, 0)),
            Some(0)
        );
        assert_eq!(
            position_to_byte_offset(combining_text, Position::new(0, 1)),
            Some(1)
        );
        assert_eq!(
            position_to_byte_offset(combining_text, Position::new(0, 2)),
            Some(3)
        );
        assert_eq!(
            position_to_byte_offset(combining_text, Position::new(0, 7)),
            Some(8)
        );
        assert_eq!(
            position_to_byte_offset(combining_text, Position::new(0, 8)),
            Some(11)
        );
        assert_eq!(
            position_to_byte_offset(combining_text, Position::new(0, 9)),
            Some(14)
        );
        assert_eq!(
            position_to_byte_offset(combining_text, Position::new(0, 10)),
            Some(14)
        );
    }

    #[test]
    fn test_position_to_byte_offset_boundaries_and_out_of_bounds() {
        // 1. Empty document
        assert_eq!(position_to_byte_offset("", Position::new(0, 0)), Some(0));
        assert_eq!(position_to_byte_offset("", Position::new(0, 1)), Some(0));
        assert_eq!(position_to_byte_offset("", Position::new(0, 100)), Some(0));
        assert_eq!(position_to_byte_offset("", Position::new(1, 0)), None);
        assert_eq!(position_to_byte_offset("", Position::new(1, 1)), None);

        // 2. Single char documents
        assert_eq!(position_to_byte_offset("x", Position::new(0, 0)), Some(0));
        assert_eq!(position_to_byte_offset("x", Position::new(0, 1)), Some(1));
        assert_eq!(position_to_byte_offset("x", Position::new(0, 2)), Some(1));
        assert_eq!(position_to_byte_offset("x", Position::new(1, 0)), None);

        assert_eq!(position_to_byte_offset("あ", Position::new(0, 0)), Some(0));
        assert_eq!(position_to_byte_offset("あ", Position::new(0, 1)), Some(3));
        assert_eq!(position_to_byte_offset("あ", Position::new(0, 2)), Some(3));
        assert_eq!(position_to_byte_offset("あ", Position::new(1, 0)), None);

        assert_eq!(position_to_byte_offset("🎉", Position::new(0, 0)), Some(0));
        assert_eq!(position_to_byte_offset("🎉", Position::new(0, 1)), Some(4));
        assert_eq!(position_to_byte_offset("🎉", Position::new(0, 2)), Some(4));
        assert_eq!(position_to_byte_offset("🎉", Position::new(0, 3)), Some(4));
        assert_eq!(position_to_byte_offset("🎉", Position::new(1, 0)), None);

        // 3. Out-of-bounds line numbers
        let doc = "first\nsecond";
        assert_eq!(position_to_byte_offset(doc, Position::new(10, 0)), None);
        assert_eq!(
            position_to_byte_offset(doc, Position::new(u32::MAX, 0)),
            None
        );
        assert_eq!(
            position_to_byte_offset(doc, Position::new(u32::MAX, u32::MAX)),
            None
        );

        // 4. Extreme character offsets (u32::MAX)
        assert_eq!(
            position_to_byte_offset(doc, Position::new(0, u32::MAX)),
            Some(5)
        );
        assert_eq!(
            position_to_byte_offset(doc, Position::new(1, u32::MAX)),
            Some(12)
        );
    }
}
