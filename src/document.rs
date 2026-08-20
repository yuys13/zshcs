use dashmap::DashMap;
use std::sync::Arc;
use tower_lsp::lsp_types::{Position, Range, TextDocumentContentChangeEvent, Url};

/// Errors related to document management and synchronization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentError {
    /// Document was not found in the manager.
    NotFound(Url),
    /// Invalid range specified for replacement.
    InvalidRange(Range),
    /// Outdated document version received.
    OutdatedVersion { current: i32, received: i32 },
}

impl std::fmt::Display for DocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocumentError::NotFound(uri) => write!(f, "document not found: {uri}"),
            DocumentError::InvalidRange(range) => write!(f, "invalid range {range:?}"),
            DocumentError::OutdatedVersion { current, received } => {
                write!(
                    f,
                    "outdated version received: current {current}, received {received}"
                )
            }
        }
    }
}

impl std::error::Error for DocumentError {}

/// Represents the in-memory state of an open document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentState {
    uri: Url,
    version: i32,
    text: String,
}

impl DocumentState {
    /// Creates a new document state.
    pub fn new(uri: Url, version: i32, text: String) -> Self {
        Self { uri, version, text }
    }

    /// Returns the URI of the document.
    pub fn uri(&self) -> &Url {
        &self.uri
    }

    /// Returns the current version of the document.
    pub fn version(&self) -> i32 {
        self.version
    }

    /// Returns the current text content of the document.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Safely applies incremental or full synchronization changes to the document text.
    ///
    /// Changes are validated and applied atomically: if any change fails (e.g. invalid range),
    /// the document state is left unmodified.
    pub fn apply_changes(
        &mut self,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) -> Result<(), DocumentError> {
        if version < self.version {
            return Err(DocumentError::OutdatedVersion {
                current: self.version,
                received: version,
            });
        }

        let mut new_text = self.text.clone();
        for change in changes {
            if let Some(range) = change.range {
                let start_offset = position_to_byte_offset(&new_text, range.start)
                    .ok_or(DocumentError::InvalidRange(range))?;
                let end_offset = position_to_byte_offset(&new_text, range.end)
                    .ok_or(DocumentError::InvalidRange(range))?;

                if start_offset > end_offset || end_offset > new_text.len() {
                    return Err(DocumentError::InvalidRange(range));
                }
                new_text.replace_range(start_offset..end_offset, &change.text);
            } else {
                new_text = change.text;
            }
        }

        self.text = new_text;
        self.version = version;
        Ok(())
    }

    /// Retrieves the text on the line of `position` from the start of the line up to `position`.
    pub fn get_line_prefix(&self, position: Position) -> Option<String> {
        let offset = position_to_byte_offset(&self.text, position)?;
        let line_start = self.text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
        Some(self.text[line_start..offset].to_string())
    }
}

/// Thread-safe manager for all open documents.
#[derive(Debug, Default, Clone)]
pub struct DocumentManager {
    documents: Arc<DashMap<Url, DocumentState>>,
}

impl DocumentManager {
    /// Creates a new, empty document manager.
    pub fn new() -> Self {
        Self {
            documents: Arc::new(DashMap::new()),
        }
    }

    /// Opens or registers a document with its initial content and version.
    pub fn open(&self, uri: Url, version: i32, text: String) {
        self.documents
            .insert(uri.clone(), DocumentState::new(uri, version, text));
    }

    /// Safely applies changes to an existing document.
    pub fn apply_changes(
        &self,
        uri: &Url,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) -> Result<(), DocumentError> {
        let mut doc = self
            .documents
            .get_mut(uri)
            .ok_or_else(|| DocumentError::NotFound(uri.clone()))?;
        doc.apply_changes(version, changes)
    }

    /// Closes a document and removes it from in-memory tracking.
    pub fn close(&self, uri: &Url) -> Option<DocumentState> {
        self.documents.remove(uri).map(|(_, doc)| doc)
    }

    /// Gets a read reference to a document's state.
    pub fn get(&self, uri: &Url) -> Option<dashmap::mapref::one::Ref<'_, Url, DocumentState>> {
        self.documents.get(uri)
    }

    /// Gets a mutable reference to a document's state.
    pub fn get_mut(
        &self,
        uri: &Url,
    ) -> Option<dashmap::mapref::one::RefMut<'_, Url, DocumentState>> {
        self.documents.get_mut(uri)
    }

    /// Returns the text content of a document if it is open.
    pub fn get_content(&self, uri: &Url) -> Option<String> {
        self.documents.get(uri).map(|doc| doc.text().to_string())
    }

    /// Retrieves the line prefix at a given position for a document.
    pub fn get_line_prefix(&self, uri: &Url, position: Position) -> Option<String> {
        self.documents
            .get(uri)
            .and_then(|doc| doc.get_line_prefix(position))
    }

    /// Checks if a document is currently open.
    pub fn contains(&self, uri: &Url) -> bool {
        self.documents.contains_key(uri)
    }

    /// Returns the number of open documents.
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    /// Returns true if no documents are open.
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

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

    #[test]
    fn test_document_error_display() {
        let uri = Url::parse("file:///test.zsh").unwrap();
        let err_not_found = DocumentError::NotFound(uri);
        assert_eq!(
            err_not_found.to_string(),
            "document not found: file:///test.zsh"
        );

        let range = Range::new(Position::new(0, 5), Position::new(0, 2));
        let err_range = DocumentError::InvalidRange(range);
        assert!(err_range.to_string().contains("invalid range"));

        let err_outdated = DocumentError::OutdatedVersion {
            current: 3,
            received: 2,
        };
        assert_eq!(
            err_outdated.to_string(),
            "outdated version received: current 3, received 2"
        );
    }

    #[test]
    fn test_document_state_new_and_getters() {
        let uri = Url::parse("file:///test.zsh").unwrap();
        let state = DocumentState::new(uri.clone(), 1, "hello world".to_string());
        assert_eq!(state.uri(), &uri);
        assert_eq!(state.version(), 1);
        assert_eq!(state.text(), "hello world");
    }

    #[test]
    fn test_document_state_apply_full_sync() {
        let uri = Url::parse("file:///test.zsh").unwrap();
        let mut state = DocumentState::new(uri.clone(), 1, "initial".to_string());

        let changes = vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "full replacement".to_string(),
        }];

        let result = state.apply_changes(2, changes);
        assert!(result.is_ok());
        assert_eq!(state.version(), 2);
        assert_eq!(state.text(), "full replacement");
    }

    #[test]
    fn test_document_state_apply_incremental_sync() {
        let uri = Url::parse("file:///test.zsh").unwrap();
        let mut state = DocumentState::new(uri.clone(), 1, "line1\nline2\nline3".to_string());

        let changes = vec![TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(1, 0), Position::new(1, 5))),
            range_length: None,
            text: "new line 2".to_string(),
        }];

        let result = state.apply_changes(2, changes);
        assert!(result.is_ok());
        assert_eq!(state.version(), 2);
        assert_eq!(state.text(), "line1\nnew line 2\nline3");
    }

    #[test]
    fn test_document_state_apply_multiple_incremental_changes() {
        let uri = Url::parse("file:///test.zsh").unwrap();
        let mut state = DocumentState::new(uri.clone(), 1, "foo bar baz".to_string());

        let changes = vec![
            TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 0), Position::new(0, 3))),
                range_length: None,
                text: "FOO".to_string(),
            },
            TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 8), Position::new(0, 11))),
                range_length: None,
                text: "BAZ".to_string(),
            },
        ];

        let result = state.apply_changes(2, changes);
        assert!(result.is_ok());
        assert_eq!(state.version(), 2);
        assert_eq!(state.text(), "FOO bar BAZ");
    }

    #[test]
    fn test_document_state_atomic_rollback_on_invalid_range() {
        let uri = Url::parse("file:///test.zsh").unwrap();
        let mut state = DocumentState::new(uri.clone(), 1, "stable text".to_string());

        let changes = vec![
            TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 0), Position::new(0, 6))),
                range_length: None,
                text: "modified".to_string(),
            },
            TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(50, 0), Position::new(50, 5))),
                range_length: None,
                text: "fail".to_string(),
            },
        ];

        let result = state.apply_changes(2, changes);
        assert!(result.is_err());
        assert_eq!(state.version(), 1);
        assert_eq!(state.text(), "stable text");
    }

    #[test]
    fn test_document_state_outdated_version_rejected() {
        let uri = Url::parse("file:///test.zsh").unwrap();
        let mut state = DocumentState::new(uri.clone(), 5, "current content".to_string());

        let changes = vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "stale content".to_string(),
        }];

        let result = state.apply_changes(4, changes);
        assert_eq!(
            result,
            Err(DocumentError::OutdatedVersion {
                current: 5,
                received: 4
            })
        );
        assert_eq!(state.version(), 5);
        assert_eq!(state.text(), "current content");
    }

    #[test]
    fn test_document_state_same_version_allowed() {
        let uri = Url::parse("file:///test.zsh").unwrap();
        let mut state = DocumentState::new(uri.clone(), 2, "content v2".to_string());

        let changes = vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "content v2 updated".to_string(),
        }];

        let result = state.apply_changes(2, changes);
        assert!(result.is_ok());
        assert_eq!(state.version(), 2);
        assert_eq!(state.text(), "content v2 updated");
    }

    #[test]
    fn test_document_state_get_line_prefix() {
        let uri = Url::parse("file:///test.zsh").unwrap();
        let state = DocumentState::new(
            uri.clone(),
            1,
            "first line\ngit status --short\n日本語のテスト".to_string(),
        );

        // Line 0
        assert_eq!(
            state.get_line_prefix(Position::new(0, 5)),
            Some("first".to_string())
        );

        // Line 1 middle
        assert_eq!(
            state.get_line_prefix(Position::new(1, 6)),
            Some("git st".to_string())
        );

        // Line 1 start
        assert_eq!(
            state.get_line_prefix(Position::new(1, 0)),
            Some("".to_string())
        );

        // Line 2 multibyte ("日本語" = 3 UTF-16 units)
        assert_eq!(
            state.get_line_prefix(Position::new(2, 3)),
            Some("日本語".to_string())
        );

        // Line 10 out of bounds
        assert_eq!(state.get_line_prefix(Position::new(10, 0)), None);
    }

    #[test]
    fn test_document_manager_crud_lifecycle() {
        let manager = DocumentManager::new();
        assert!(manager.is_empty());
        assert_eq!(manager.len(), 0);

        let uri1 = Url::parse("file:///doc1.zsh").unwrap();
        let uri2 = Url::parse("file:///doc2.zsh").unwrap();

        // 1. Open doc1
        manager.open(uri1.clone(), 1, "doc1 content".to_string());
        assert!(!manager.is_empty());
        assert_eq!(manager.len(), 1);
        assert!(manager.contains(&uri1));
        assert_eq!(manager.get_content(&uri1), Some("doc1 content".to_string()));

        // 2. Open doc2
        manager.open(uri2.clone(), 1, "doc2 content".to_string());
        assert_eq!(manager.len(), 2);
        assert!(manager.contains(&uri2));

        // 3. Apply changes to doc1
        let changes = vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "doc1 updated".to_string(),
        }];
        let res = manager.apply_changes(&uri1, 2, changes);
        assert!(res.is_ok());
        assert_eq!(manager.get_content(&uri1), Some("doc1 updated".to_string()));
        assert_eq!(manager.get_content(&uri2), Some("doc2 content".to_string()));

        // 4. Line prefix
        assert_eq!(
            manager.get_line_prefix(&uri1, Position::new(0, 4)),
            Some("doc1".to_string())
        );

        // 5. Close doc1
        let closed = manager.close(&uri1);
        assert!(closed.is_some());
        assert_eq!(closed.unwrap().text(), "doc1 updated");
        assert!(!manager.contains(&uri1));
        assert_eq!(manager.get_content(&uri1), None);
        assert_eq!(manager.len(), 1);

        // 6. Close non-existent
        assert!(manager.close(&uri1).is_none());

        // 7. Close doc2
        assert!(manager.close(&uri2).is_some());
        assert!(manager.is_empty());
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_document_manager_apply_changes_not_found() {
        let manager = DocumentManager::new();
        let uri = Url::parse("file:///not_found.zsh").unwrap();

        let res = manager.apply_changes(&uri, 1, vec![]);
        assert_eq!(res, Err(DocumentError::NotFound(uri)));
    }

    #[test]
    fn test_document_manager_get_line_prefix_not_found() {
        let manager = DocumentManager::new();
        let uri = Url::parse("file:///not_found.zsh").unwrap();

        assert_eq!(manager.get_line_prefix(&uri, Position::new(0, 0)), None);
    }

    #[test]
    fn test_document_state_extreme_versions() {
        let uri = Url::parse("file:///extreme.zsh").unwrap();
        let mut state = DocumentState::new(uri.clone(), i32::MIN, "start".to_string());
        assert_eq!(state.version(), i32::MIN);

        // Update to 0
        let res = state.apply_changes(0, vec![]);
        assert!(res.is_ok());
        assert_eq!(state.version(), 0);

        // Update to i32::MAX
        let res2 = state.apply_changes(i32::MAX, vec![]);
        assert!(res2.is_ok());
        assert_eq!(state.version(), i32::MAX);

        // Outdated attempt
        let res3 = state.apply_changes(i32::MAX - 1, vec![]);
        assert_eq!(
            res3,
            Err(DocumentError::OutdatedVersion {
                current: i32::MAX,
                received: i32::MAX - 1
            })
        );
    }

    #[test]
    fn test_document_manager_get_and_get_mut() {
        let manager = DocumentManager::new();
        let uri = Url::parse("file:///doc.zsh").unwrap();
        manager.open(uri.clone(), 1, "initial".to_string());

        {
            let doc_ref = manager.get(&uri).unwrap();
            assert_eq!(doc_ref.text(), "initial");
        }

        {
            let mut doc_mut = manager.get_mut(&uri).unwrap();
            doc_mut
                .apply_changes(
                    2,
                    vec![TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: "modified directly".to_string(),
                    }],
                )
                .unwrap();
        }

        assert_eq!(
            manager.get_content(&uri),
            Some("modified directly".to_string())
        );
    }

    #[tokio::test]
    async fn test_document_manager_concurrent_operations() {
        let manager = Arc::new(DocumentManager::new());
        let mut handles = Vec::new();

        for i in 0..20 {
            let mgr = Arc::clone(&manager);
            handles.push(tokio::spawn(async move {
                let uri = Url::parse(&format!("file:///concurrent_{i}.zsh")).unwrap();
                // Open
                mgr.open(uri.clone(), 1, format!("initial_{i}"));
                assert!(mgr.contains(&uri));

                // Apply changes
                for ver in 2..=10 {
                    let change = TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: format!("version_{ver}_{i}"),
                    };
                    mgr.apply_changes(&uri, ver, vec![change]).unwrap();
                }

                // Verify
                assert_eq!(mgr.get_content(&uri), Some(format!("version_10_{i}")));

                // Close
                let closed = mgr.close(&uri);
                assert!(closed.is_some());
                assert_eq!(closed.unwrap().text(), format!("version_10_{i}"));
                assert!(!mgr.contains(&uri));
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        assert!(manager.is_empty());
    }

    #[test]
    fn test_document_state_empty_changes() {
        let uri = Url::parse("file:///empty_changes.zsh").unwrap();
        let mut state = DocumentState::new(uri.clone(), 1, "original".to_string());

        // Apply empty change list with bumped version
        let res = state.apply_changes(2, vec![]);
        assert!(res.is_ok());
        assert_eq!(state.version(), 2);
        assert_eq!(state.text(), "original");
    }

    #[tokio::test]
    async fn test_document_manager_same_uri_concurrent_operations() {
        let manager = Arc::new(DocumentManager::new());
        let shared_uri = Url::parse("file:///shared_doc.zsh").unwrap();
        let mut handles = Vec::new();

        for i in 0..20 {
            let mgr = Arc::clone(&manager);
            let uri = shared_uri.clone();
            handles.push(tokio::spawn(async move {
                for iter in 0..50 {
                    let version = i * 100 + iter + 1;
                    // Mix of operations
                    match iter % 5 {
                        0 => {
                            mgr.open(
                                uri.clone(),
                                version,
                                format!("opened by task {i} iter {iter}"),
                            );
                        }
                        1 => {
                            let _ = mgr.apply_changes(
                                &uri,
                                version,
                                vec![TextDocumentContentChangeEvent {
                                    range: None,
                                    range_length: None,
                                    text: format!("edited by task {i} iter {iter}"),
                                }],
                            );
                        }
                        2 => {
                            let _ = mgr.get_content(&uri);
                        }
                        3 => {
                            let _ = mgr.get_line_prefix(&uri, Position::new(0, 5));
                        }
                        4 => {
                            let _ = mgr.close(&uri);
                        }
                        _ => unreachable!(),
                    }
                }
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }
    }

    #[test]
    fn test_document_state_cascading_batch_changes() {
        let uri = Url::parse("file:///cascade.zsh").unwrap();
        let mut state = DocumentState::new(uri, 1, "first line".to_string());

        // Batch of 3 changes executed sequentially
        let changes = vec![
            // 1. Append two new lines
            TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 10), Position::new(0, 10))),
                range_length: None,
                text: "\nsecond line\nthird line".to_string(),
            },
            // 2. Modify newly created line 1: "second line" -> "2nd line"
            TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(1, 0), Position::new(1, 11))),
                range_length: None,
                text: "2nd line".to_string(),
            },
            // 3. Append null byte + suffix to line 2
            TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(2, 10), Position::new(2, 10))),
                range_length: None,
                text: "\0_suffix".to_string(),
            },
        ];

        let res = state.apply_changes(2, changes);
        assert!(res.is_ok());
        assert_eq!(state.version(), 2);
        assert_eq!(state.text(), "first line\n2nd line\nthird line\0_suffix");
    }

    #[test]
    fn test_document_state_empty_doc_incremental_insert() {
        let uri = Url::parse("file:///empty_insert.zsh").unwrap();
        let mut state = DocumentState::new(uri, 1, "".to_string());

        let changes = vec![TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(0, 0), Position::new(0, 0))),
            range_length: None,
            text: "inserted text\nsecond".to_string(),
        }];

        let res = state.apply_changes(2, changes);
        assert!(res.is_ok());
        assert_eq!(state.version(), 2);
        assert_eq!(state.text(), "inserted text\nsecond");
    }
}
