use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use tower_lsp::lsp_types::{Position, Range, TextDocumentContentChangeEvent, Url};
use zshcs::completion::{infer_completion_kind, parse_candidate_line};
use zshcs::document::{DocumentState, position_to_byte_offset};

fn bench_parse_candidate_line(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_candidate_line");

    let sample_cases = [
        ("plain", "plain_text_candidate"),
        ("tab_desc", "status\tshow working tree status"),
        ("options", "--help\tdisplay this help and exit"),
        ("var_env", "$HOME\tuser home directory"),
        ("path_dir", "/usr/local/bin/\texecutable search directory"),
        ("dot_file", ".zshrc\tzsh configuration file"),
        ("function", "my_func\tshell function"),
        ("ansi_color", "\x1b[32m--verbose\x1b[0m\tenable verbose"),
        ("unicode_emoji", "✨ feat\t新機能の追加"),
        ("multi_tab", "cmd\topt1\topt2\topt3"),
    ];

    for (name, line) in sample_cases {
        group.bench_with_input(BenchmarkId::new("single_line", name), &line, |b, line| {
            let mut items = Vec::with_capacity(16);
            b.iter(|| {
                items.clear();
                parse_candidate_line(black_box(line), &mut items);
            });
        });
    }

    // Batch throughput benchmark (1,000 items)
    let batch_lines: Vec<&str> = (0..1000)
        .map(|i| {
            if i % 2 == 0 {
                "status\tshow working tree status"
            } else {
                "--help\tdisplay this help and exit"
            }
        })
        .collect();

    group.throughput(Throughput::Elements(batch_lines.len() as u64));
    group.bench_function("batch_1000_candidates", |b| {
        let mut items = Vec::with_capacity(1000);
        b.iter(|| {
            items.clear();
            for line in &batch_lines {
                parse_candidate_line(black_box(line), &mut items);
            }
        });
    });

    group.finish();
}

fn bench_infer_completion_kind(c: &mut Criterion) {
    let mut group = c.benchmark_group("infer_completion_kind");

    let sample_cases = [
        ("flag_short", "-v", None),
        ("flag_long", "--help", Some("show help")),
        ("env_var", "$HOME", Some("user home directory")),
        ("dir_slash", "/usr/local/bin/", None),
        ("file_dot", ".zshrc", Some("zsh config")),
        ("func_cmd", "git", Some("builtin command")),
        ("plain_text", "status", Some("show status")),
    ];

    for (name, label, detail) in sample_cases {
        group.bench_function(BenchmarkId::new("infer_kind", name), |b| {
            b.iter(|| {
                let _ = infer_completion_kind(black_box(label), black_box(detail));
            });
        });
    }

    group.finish();
}

fn bench_document_conversions(c: &mut Criterion) {
    let mut group = c.benchmark_group("document_conversions");

    let ascii_doc = "function hello() {\n  echo 'Hello, World!'\n  return 0\n}\n".repeat(50);
    let unicode_doc =
        "# 日本語コメント\nfunction 挨拶() {\n  echo '👋 こんにちは世界 ✨'\n}\n".repeat(50);

    let ascii_pos = Position::new(25, 10);
    let unicode_pos = Position::new(25, 10);

    group.bench_function("position_to_byte_offset_ascii", |b| {
        b.iter(|| {
            let _ = position_to_byte_offset(black_box(&ascii_doc), black_box(ascii_pos));
        });
    });

    group.bench_function("position_to_byte_offset_unicode", |b| {
        b.iter(|| {
            let _ = position_to_byte_offset(black_box(&unicode_doc), black_box(unicode_pos));
        });
    });

    let uri: Url = "file:///tmp/bench.zsh".parse().unwrap();
    let initial_text = "echo 'initial text'\n".repeat(100);

    group.bench_function("document_state_apply_incremental_changes", |b| {
        b.iter_batched(
            || DocumentState::new(uri.clone(), 1, initial_text.clone()),
            |mut doc| {
                let changes = vec![TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(10, 5), Position::new(10, 12))),
                    range_length: None,
                    text: "modified content".to_string(),
                }];
                let _ = doc.apply_changes(2, changes);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_parse_candidate_line,
    bench_infer_completion_kind,
    bench_document_conversions
);
criterion_main!(benches);
