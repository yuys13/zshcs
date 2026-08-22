mod common;

use std::sync::Arc;
use tokio::sync::Barrier;
use tower_lsp::lsp_types::{
    CompletionParams, DidChangeTextDocumentParams, DidOpenTextDocumentParams, ExecuteCommandParams,
    Position, Range, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Url, VersionedTextDocumentIdentifier,
    notification::{DidChangeTextDocument, DidOpenTextDocument, LogMessage},
    request,
};
use zshcs::completion::parse_candidate_line;

// =========================================================================
// 1. Extreme Fuzzing of `parse_candidate_line`
// =========================================================================

#[test]
fn test_fuzz_parse_candidate_line_null_bytes() {
    let mut items = Vec::new();

    // Line containing null bytes
    let line = "cand\0part1\0part2\tdesc\0part1\0part2";
    parse_candidate_line(line, &mut items);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "cand\0part1\0part2");
    assert_eq!(items[0].insert_text.as_deref(), None);
    assert_eq!(items[0].detail.as_deref(), Some("desc\0part1\0part2"));

    // Only null bytes
    items.clear();
    parse_candidate_line("\0\0\0\t\0\0\0", &mut items);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "\0\0\0");
    assert_eq!(items[0].detail.as_deref(), Some("\0\0\0"));
}

#[test]
fn test_fuzz_parse_candidate_line_deeply_nested_quotes() {
    let mut items = Vec::new();

    let nested_quotes = "\"\"\"'''\"\"\"'''```\"\"\"'''";
    let input = format!("{}\t{}", nested_quotes, nested_quotes);
    parse_candidate_line(&input, &mut items);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, nested_quotes);
    assert_eq!(items[0].detail.as_deref(), Some(nested_quotes));

    // Mismatched and deeply nested quotes with backslashes
    items.clear();
    let malformed = r#"\"\'\"\'\"\\\'\"\\\n\r\t"#;
    parse_candidate_line(malformed, &mut items);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, malformed);
    assert_eq!(items[0].detail, None);
}

#[test]
fn test_fuzz_parse_candidate_line_control_characters() {
    let mut items = Vec::new();

    // All ASCII control characters from 0x01 to 0x1F except tab (0x09)
    let mut ctrl_chars = String::new();
    for byte in 1..=0x1Fu8 {
        if byte != b'\t' && byte != b'\n' && byte != b'\r' {
            ctrl_chars.push(byte as char);
        }
    }
    ctrl_chars.push(0x7F as char); // DEL

    let input = format!("label_{ctrl_chars}\tdetail_{ctrl_chars}");
    parse_candidate_line(&input, &mut items);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, format!("label_{ctrl_chars}"));
    assert_eq!(
        items[0].detail.as_deref(),
        Some(format!("detail_{ctrl_chars}").as_str())
    );
}

#[test]
fn test_fuzz_parse_candidate_line_huge_inputs() {
    let mut items = Vec::new();

    // 1 MB candidate label with 1 MB detail
    let huge_label = "A".repeat(1_000_000);
    let huge_detail = "B".repeat(1_000_000);
    let line = format!("{}\t{}", huge_label, huge_detail);

    parse_candidate_line(&line, &mut items);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label.len(), 1_000_000);
    assert_eq!(items[0].detail.as_ref().map(|s| s.len()), Some(1_000_000));
}

#[test]
fn test_fuzz_parse_candidate_line_unusual_whitespace() {
    let mut items = Vec::new();

    // Non-breaking space (U+00A0), En-space (U+2002), Em-space (U+2003), Zero-width space (U+200B)
    let ws = "\u{00A0}\u{2002}\u{2003}\u{200B}";
    let line = format!("cand{}\tdesc{}", ws, ws);
    parse_candidate_line(&line, &mut items);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, format!("cand{}", ws));
    assert_eq!(
        items[0].detail.as_deref(),
        Some(format!("desc{}", ws).as_str())
    );

    // Zero width joiner / non-joiner, RTL overrides
    items.clear();
    let bidi_zwj = "\u{200D}\u{200C}\u{202E}rtl_override\u{202C}";
    parse_candidate_line(bidi_zwj, &mut items);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, bidi_zwj);

    // Combining characters overflow (1,000 combining marks on single base character)
    items.clear();
    let combining_heavy = format!("e{}", "\u{0301}".repeat(1000));
    parse_candidate_line(&combining_heavy, &mut items);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, combining_heavy);
}

#[test]
fn test_fuzz_parse_candidate_line_randomized_stress() {
    let mut items = Vec::new();

    // Deterministic pseudo-random generation with edge bytes
    let mut seed: u64 = 0xDEADBEEFCAFE;
    let mut prng = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (seed >> 33) as u32
    };

    let sample_chars: Vec<char> = vec![
        'a', 'Z', '0', '9', ' ', '\t', '\0', '\x1b', '$', '"', '\'', '\\', '/', '?', '&', '|', ';',
        'あ', '𩸽', '🚀', '👨', '\u{200D}', '\u{0301}',
    ];

    for _ in 0..10_000 {
        let len = (prng() % 100) as usize;
        let mut s = String::with_capacity(len);
        for _ in 0..len {
            let idx = (prng() as usize) % sample_chars.len();
            s.push(sample_chars[idx]);
        }
        // Must never panic
        parse_candidate_line(&s, &mut items);
    }
    assert_eq!(items.len(), 10_000);
}

// =========================================================================
// 2. Completion Daemon Interaction Stress & Concurrency Under Load
// =========================================================================

#[tokio::test]
async fn test_stress_daemon_high_candidate_volume_10k() {
    // Mock daemon emitting 10,000 candidates
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    for i in {1..10000}; do
        printf "candidate_%d\tdescription_%d\n" "$i" "$i"
    done
    printf "\x01EOC\x01\n"
done
"#;
    let (mut client_stream, _server_handle) = common::setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///stress_10k.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "cmd ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = common::get_completion_items(res);
    assert_eq!(items.len(), 10_000);
    assert_eq!(items[0].label, "candidate_1");
    assert_eq!(items[9999].label, "candidate_10000");
}

#[tokio::test]
async fn test_stress_daemon_heavy_stderr_logging() {
    // Daemon that prints 500 stderr lines before returning candidates
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    for i in {1..500}; do
        echo "log warning message $i" >&2
    done
    printf "result\tfinished\n"
    printf "\x01EOC\x01\n"
done
"#;
    let (mut client_stream, _server_handle) = common::setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///stress_stderr.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "cmd ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = common::get_completion_items(res);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "result");
}

#[tokio::test]
async fn test_stress_daemon_invalid_utf8_output_resilience() {
    // Daemon emitting invalid non-UTF8 bytes followed by crash/EOF
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    # Print invalid UTF-8 byte sequence 0xFF 0xFE
    printf "\xff\xfe\n"
done
"#;
    let (mut client_stream, _server_handle) = common::setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///invalid_utf8.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "cmd ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await;

    // Server must safely return Ok(None) without crashing or hanging
    assert!(res.is_ok());
    assert!(res.unwrap().is_none());
}

#[tokio::test]
async fn test_stress_concurrent_50_clients() {
    // Stress test with 50 concurrent client connections
    let count = 50;
    let barrier = Arc::new(Barrier::new(count));

    let handles: Vec<_> = (0..count)
        .map(|i| {
            let b = barrier.clone();
            tokio::spawn(async move {
                let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    printf "res\tdesc\n"
    printf "\x01EOC\x01\n"
done
"#;
                let (mut client_stream, _server_handle) =
                    common::setup_server_with_scripts(mock_script, "");
                let mut test_client = common::TestClient::new(&mut client_stream);

                let doc_uri = Url::parse(&format!("file:///client_{i}.zsh")).unwrap();
                test_client.init_and_open(&doc_uri, "git ").await;

                b.wait().await; // Synchronize starting burst

                let res = test_client
                    .send_request::<request::Completion>(CompletionParams {
                        text_document_position: TextDocumentPositionParams {
                            text_document: TextDocumentIdentifier { uri: doc_uri },
                            position: Position::new(0, 4),
                        },
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                        context: None,
                    })
                    .await
                    .unwrap()
                    .unwrap();

                let items = common::get_completion_items(res);
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].label, "res");
            })
        })
        .collect();

    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test]
async fn test_stress_rapid_interleaved_sync_and_completion_burst() {
    // Single client performing rapid back-to-back edits and completions
    let (mut client_stream, _server_handle) = common::setup_server_mock();
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///burst_interleaved.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "g").await;

    for i in 1..=10 {
        // DidChange append character
        test_client
            .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), i + 1),
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: format!("git status {i}"),
                }],
            })
            .await;
        test_client.read_notification::<LogMessage>().await;

        // Completion request
        let res = test_client
            .send_request::<request::Completion>(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: doc_uri.clone(),
                    },
                    position: Position::new(0, 4),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await
            .unwrap()
            .unwrap();

        let items = common::get_completion_items(res);
        assert!(items.iter().any(|item| item.label == "status"));
    }
}

#[tokio::test]
async fn test_stress_high_concurrency_channel_saturation() {
    // Channel capacity is 32 in Backend::new_with_scripts.
    // Test submitting 35 sequential requests on a single server instance with slight delay in mock daemon.
    let mock_delay_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    sleep 0.02
    printf "item\tdesc\n"
    printf "\x01EOC\x01\n"
done
"#;
    let (mut client_stream, _server_handle) =
        common::setup_server_with_scripts(mock_delay_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///channel_saturation.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "test ").await;

    // Send 35 sequential requests without crash or timeout
    for _ in 0..35 {
        let res = test_client
            .send_request::<request::Completion>(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: doc_uri.clone(),
                    },
                    position: Position::new(0, 5),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await
            .unwrap()
            .unwrap();

        let items = common::get_completion_items(res);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "item");
    }
}

#[tokio::test]
async fn test_stress_multi_document_concurrent_edits() {
    let (mut client_stream, _server_handle) = common::setup_server_mock();
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_a = Url::parse("file:///concurrent_doc_a.zsh").unwrap();
    let doc_b = Url::parse("file:///concurrent_doc_b.zsh").unwrap();

    test_client.init_and_open(&doc_a, "initial_a").await;

    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_b.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "initial_b".to_string(),
            },
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Rapid interleaved edits between doc A and doc B
    for i in 1..=20 {
        test_client
            .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier::new(doc_a.clone(), i + 1),
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(0, 0), Position::new(0, 0))),
                    range_length: None,
                    text: format!("a{i}_"),
                }],
            })
            .await;
        test_client.read_notification::<LogMessage>().await;

        test_client
            .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier::new(doc_b.clone(), i + 1),
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(0, 0), Position::new(0, 0))),
                    range_length: None,
                    text: format!("b{i}_"),
                }],
            })
            .await;
        test_client.read_notification::<LogMessage>().await;
    }

    // Verify document contents
    let res_a = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_a).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content_a: Option<String> = res_a.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert!(content_a.is_some());
    assert!(content_a.unwrap().contains("initial_a"));

    let res_b = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_b).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content_b: Option<String> = res_b.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert!(content_b.is_some());
    assert!(content_b.unwrap().contains("initial_b"));
}

// =========================================================================
// 3. Document Position Mapping Adversarial Stress Tests (`position_to_byte_offset`)
// =========================================================================

#[test]
fn test_stress_position_to_byte_offset_complex_unicode_sequences() {
    // 1. Regional indicator flag sequences: 🇯🇵 (Japan) + 🇺🇸 (US)
    // 🇯 (U+1F1EF, 4B, 2 UTF-16) + 🇵 (U+1F1F5, 4B, 2 UTF-16) = 8 bytes, 4 UTF-16 code units
    let flags_text = "🇯🇵🇺🇸";
    assert_eq!(
        zshcs::document::position_to_byte_offset(flags_text, Position::new(0, 0)),
        Some(0)
    );
    // Middle of first flag code point (surrogate 1): snaps to end of code point (byte 4)
    assert_eq!(
        zshcs::document::position_to_byte_offset(flags_text, Position::new(0, 1)),
        Some(4)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(flags_text, Position::new(0, 2)),
        Some(4)
    );
    // Second flag code point:
    assert_eq!(
        zshcs::document::position_to_byte_offset(flags_text, Position::new(0, 3)),
        Some(8)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(flags_text, Position::new(0, 4)),
        Some(8)
    );
    // Next flag (US):
    assert_eq!(
        zshcs::document::position_to_byte_offset(flags_text, Position::new(0, 5)),
        Some(12)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(flags_text, Position::new(0, 8)),
        Some(16)
    );

    // 2. Scotland tag sequence: 🏴󠁧󠁢󠁳󠁣󠁴󠁿 (U+1F3F4 + tag chars + cancel tag U+E007F)
    // 1 black flag (4B, 2u) + 5 tag chars (each 4B, 2u) + 1 cancel tag (4B, 2u) = 28 bytes, 14 UTF-16 code units
    let scotland = "🏴󠁧󠁢󠁳󠁣󠁴󠁿";
    assert_eq!(
        zshcs::document::position_to_byte_offset(scotland, Position::new(0, 0)),
        Some(0)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(scotland, Position::new(0, 14)),
        Some(28)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(scotland, Position::new(0, 100)),
        Some(28)
    );

    // 3. Devanagari text with virama and matras: नमस्ते ("namaste")
    // न (3B, 1u), म (3B, 1u), स (3B, 1u), ् (3B, 1u), त (3B, 1u), े (3B, 1u) = 18 bytes, 6 UTF-16 units
    let hindi = "नमस्ते";
    assert_eq!(
        zshcs::document::position_to_byte_offset(hindi, Position::new(0, 0)),
        Some(0)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(hindi, Position::new(0, 1)),
        Some(3)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(hindi, Position::new(0, 3)),
        Some(9)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(hindi, Position::new(0, 4)),
        Some(12)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(hindi, Position::new(0, 6)),
        Some(18)
    );

    // 4. Stacking diacritics / Zalgo text: e + 4 combining accents
    let zalgo = "e\u{0300}\u{0301}\u{0302}\u{0303}";
    // 'e' (1B, 1u), each combining char is 2 bytes, 1 UTF-16 code unit -> Total 9 bytes, 5 UTF-16 units
    assert_eq!(
        zshcs::document::position_to_byte_offset(zalgo, Position::new(0, 0)),
        Some(0)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(zalgo, Position::new(0, 1)),
        Some(1)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(zalgo, Position::new(0, 2)),
        Some(3)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(zalgo, Position::new(0, 3)),
        Some(5)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(zalgo, Position::new(0, 4)),
        Some(7)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(zalgo, Position::new(0, 5)),
        Some(9)
    );

    // 5. RTL text with directional marks: Right-to-Left Override (\u{202E}), Arabic, Hebrew, Pop Directional Format (\u{202C})
    let bidi_text = "\u{202E}مَرْحَبًا שָׁלוֹם\u{202C}";
    for char_pos in 0..30 {
        let offset =
            zshcs::document::position_to_byte_offset(bidi_text, Position::new(0, char_pos));
        assert!(offset.is_some());
        let off = offset.unwrap();
        assert!(off <= bidi_text.len());
        assert!(bidi_text.is_char_boundary(off));
    }
}

#[test]
fn test_stress_position_to_byte_offset_mixed_line_endings_and_huge_text() {
    // 1. Text with LF, CRLF, bare CR, consecutive blank lines
    let text = "Line1\r\n\r\nLine3\n\nLine5\rLine6\r\nLine7";
    // Line 0: "Line1" -> bytes 0..5 (\r\n at 5..7)
    assert_eq!(
        zshcs::document::position_to_byte_offset(text, Position::new(0, 0)),
        Some(0)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(text, Position::new(0, 5)),
        Some(5)
    );
    // Line 1: blank line (\r\n at 7..9)
    assert_eq!(
        zshcs::document::position_to_byte_offset(text, Position::new(1, 0)),
        Some(7)
    );
    // Line 2: "Line3" -> bytes 9..14 (\n at 14..15)
    assert_eq!(
        zshcs::document::position_to_byte_offset(text, Position::new(2, 0)),
        Some(9)
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(text, Position::new(2, 5)),
        Some(14)
    );
    // Line 3: blank line (\n at 15..16)
    assert_eq!(
        zshcs::document::position_to_byte_offset(text, Position::new(3, 0)),
        Some(15)
    );
    // Line 4: "Line5\rLine6" (bare CR does not split line in standard Rust .find('\n'))
    // line_offset is 16. The line continues until \r\n after Line6.
    assert_eq!(
        zshcs::document::position_to_byte_offset(text, Position::new(4, 0)),
        Some(16)
    );
    let line4_off = zshcs::document::position_to_byte_offset(text, Position::new(4, 11)).unwrap();
    assert!(text.is_char_boundary(line4_off));

    // 2. Large multi-line document (1,000 lines with mixed CJK, Emoji, and CRLF)
    let mut large_doc = String::new();
    for i in 0..1000 {
        if i % 2 == 0 {
            large_doc.push_str(&format!("行_{i}: 🚀 test 日本語\r\n"));
        } else {
            large_doc.push_str(&format!("Line_{i}: standard LF\n"));
        }
    }

    // Verify random positions across 1,000 lines
    for line_idx in [0, 1, 50, 100, 500, 999] {
        let offset =
            zshcs::document::position_to_byte_offset(&large_doc, Position::new(line_idx, 0));
        assert!(offset.is_some());
        let off = offset.unwrap();
        assert!(large_doc.is_char_boundary(off));

        let offset_end =
            zshcs::document::position_to_byte_offset(&large_doc, Position::new(line_idx, 100));
        assert!(offset_end.is_some());
        let off_end = offset_end.unwrap();
        assert!(large_doc.is_char_boundary(off_end));
        assert!(off <= off_end);
    }

    // Line 1000 is the trailing empty line after the 1000th newline
    assert_eq!(
        zshcs::document::position_to_byte_offset(&large_doc, Position::new(1000, 0)),
        Some(large_doc.len())
    );
    // Line 1001 is out of bounds
    assert_eq!(
        zshcs::document::position_to_byte_offset(&large_doc, Position::new(1001, 0)),
        None
    );
    assert_eq!(
        zshcs::document::position_to_byte_offset(&large_doc, Position::new(u32::MAX, 0)),
        None
    );
}

#[test]
fn test_fuzz_position_to_byte_offset_invariants() {
    // Deterministic PRNG
    let mut seed: u64 = 0x123456789ABCDEF0;
    let mut prng = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (seed >> 33) as u32
    };

    let sample_chars: Vec<char> = vec![
        'a', 'Z', '0', '\n', '\r', '\t', ' ', 'あ', '漢', '字', '𩸽', '𠮷', '🚀', '👨', '\u{200D}',
        '👩', '👧', '👦', '\u{0301}', '\u{202E}', '\u{202C}', 'م', 'ר',
    ];

    // Generate 500 diverse documents and probe 100 random positions on each (50,000 probes)
    for _ in 0..500 {
        let doc_len = (prng() % 200) as usize;
        let mut doc = String::with_capacity(doc_len);
        for _ in 0..doc_len {
            let idx = (prng() as usize) % sample_chars.len();
            doc.push(sample_chars[idx]);
        }

        for _ in 0..100 {
            let line = prng() % 50;
            let character = prng() % 50;
            let pos = Position::new(line, character);

            // Invariant 1: Must never panic
            let res = zshcs::document::position_to_byte_offset(&doc, pos);

            // Invariant 2: If Some(offset), offset <= doc.len()
            // Invariant 3: If Some(offset), doc.is_char_boundary(offset) is TRUE
            // Invariant 4: Slicing at offset never panics
            if let Some(offset) = res {
                assert!(
                    offset <= doc.len(),
                    "Offset {} exceeds doc length {} for doc: {:?}",
                    offset,
                    doc.len(),
                    doc
                );
                assert!(
                    doc.is_char_boundary(offset),
                    "Offset {} is NOT on a char boundary in doc: {:?}",
                    offset,
                    doc
                );
                let _prefix = &doc[..offset];
                let _suffix = &doc[offset..];
            }
        }
    }
}

// =========================================================================
// 4. Incremental Synchronization (`did_change`) Adversarial Fuzzing
// =========================================================================

#[tokio::test]
async fn test_stress_did_change_random_incremental_edits_fuzz() {
    let (mut client_stream, _server_handle) = common::setup_server_mock();
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///fuzz_incremental.zsh").unwrap();
    let mut expected_text = "initial text 🚀\nline 2 日本語\nline 3 𩸽\n".to_string();
    test_client.init_and_open(&doc_uri, &expected_text).await;

    let mut seed: u64 = 0x9876543210FEDCBA;
    let mut prng = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (seed >> 33) as u32
    };

    let sample_replacements = [
        "abc",
        "日本語",
        "🎉🚀",
        "\nnew line\n",
        "\r\nCRLF\r\n",
        "",
        "👨‍👩‍👧‍👦",
        "e\u{0301}",
    ];

    // Perform 200 random incremental mutations
    for version in 2..=200 {
        let lines_count = expected_text.matches('\n').count() + 1;
        let start_line = prng() % (lines_count as u32 + 2);
        let end_line = start_line + (prng() % 3);
        let start_char = prng() % 20;
        let end_char = if start_line == end_line {
            start_char + (prng() % 10)
        } else {
            prng() % 20
        };

        let replacement = sample_replacements[(prng() as usize) % sample_replacements.len()];
        let range = Range::new(
            Position::new(start_line, start_char),
            Position::new(end_line, end_char),
        );

        let start_off = zshcs::document::position_to_byte_offset(&expected_text, range.start);
        let end_off = zshcs::document::position_to_byte_offset(&expected_text, range.end);

        let will_succeed = if let (Some(s), Some(e)) = (start_off, end_off) {
            s <= e
        } else {
            false
        };

        if will_succeed {
            let s = start_off.unwrap();
            let e = end_off.unwrap();
            expected_text.replace_range(s..e, replacement);
        }

        // Send DidChange
        test_client
            .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), version),
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: Some(range),
                    range_length: None,
                    text: replacement.to_string(),
                }],
            })
            .await;

        // Read log notification
        let _ = test_client.read_notification::<LogMessage>().await;
    }

    // Verify document content matches expected state
    let res = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content: Option<String> = res.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(
        content,
        Some(expected_text),
        "Document content desynchronized during random incremental fuzzing"
    );
}

#[tokio::test]
async fn test_stress_did_change_malformed_ranges_and_split_surrogates() {
    let (mut client_stream, _server_handle) = common::setup_server_mock();
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///malformed_ranges.zsh").unwrap();
    let initial_text = "A𩸽B\nC🚀D\n";
    test_client.init_and_open(&doc_uri, initial_text).await;

    // 1. Inverted range (start line > end line)
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(1, 0), Position::new(0, 0))),
                range_length: None,
                text: "INVALID".to_string(),
            }],
        })
        .await;
    let log1_warn = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log1_warn.typ, tower_lsp::lsp_types::MessageType::WARNING);
    assert!(log1_warn.message.contains("invalid range"));
    let log1_info = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log1_info.typ, tower_lsp::lsp_types::MessageType::INFO);

    // 2. Inverted range (same line, start col > end col)
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 3),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 3), Position::new(0, 1))),
                range_length: None,
                text: "INVALID".to_string(),
            }],
        })
        .await;
    let log2_warn = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log2_warn.typ, tower_lsp::lsp_types::MessageType::WARNING);
    assert!(log2_warn.message.contains("invalid range"));
    let log2_info = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log2_info.typ, tower_lsp::lsp_types::MessageType::INFO);

    // 3. Split surrogate edit: Position pointing to offset 2 inside '𩸽' (first surrogate code unit is at 1..3 in UTF-16)
    // '𩸽' is UTF-16 index 1..3. Start at 2 (middle of surrogate) -> snaps safely to end of char (byte 5).
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 4),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 2), Position::new(0, 3))),
                range_length: None,
                text: "X".to_string(),
            }],
        })
        .await;
    let _ = test_client.read_notification::<LogMessage>().await;

    // Verify document is valid UTF-8 and content is modified without panic
    let res = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content: Option<String> = res.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert!(content.is_some());
    let doc_str = content.unwrap();
    assert_eq!(doc_str, "A𩸽XB\nC🚀D\n");
}
