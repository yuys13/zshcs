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
}
