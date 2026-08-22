use std::time::Instant;
use zshcs::completion::{infer_completion_kind, parse_candidate_line};

fn main() {
    println!("=== zshcs Candidate Parser & Kind Inference Benchmark ===");

    let sample_lines = [
        "status\tshow working tree status",
        "--help\tdisplay this help and exit",
        "$HOME\tuser home directory",
        "/usr/local/bin/\texecutable search directory",
        ".zshrc\tzsh configuration file",
        "my_func\tshell function",
        "plain_text_candidate",
    ];

    let iterations = 100_000;
    let mut items = Vec::with_capacity(1000);

    // Warm up
    for _ in 0..10_000 {
        for line in &sample_lines {
            items.clear();
            parse_candidate_line(line, &mut items);
        }
    }

    // Benchmark parse_candidate_line
    let start = Instant::now();
    for _ in 0..iterations {
        for line in &sample_lines {
            items.clear();
            parse_candidate_line(line, &mut items);
        }
    }
    let elapsed = start.elapsed();
    let total_ops = iterations * sample_lines.len();
    let ns_per_op = elapsed.as_nanos() as f64 / total_ops as f64;
    let ops_per_sec = total_ops as f64 / elapsed.as_secs_f64();

    println!(
        "parse_candidate_line: {} ops in {:?} ({:.2} ns/op, {:.2} ops/sec)",
        total_ops, elapsed, ns_per_op, ops_per_sec
    );

    // Benchmark infer_completion_kind
    let start = Instant::now();
    for _ in 0..iterations {
        for line in &sample_lines {
            let (label, detail) = match line.split_once('\t') {
                Some((lbl, dtl)) => (lbl, Some(dtl)),
                None => (*line, None),
            };
            let _ = infer_completion_kind(label, detail);
        }
    }
    let elapsed = start.elapsed();
    let ns_per_op = elapsed.as_nanos() as f64 / total_ops as f64;
    let ops_per_sec = total_ops as f64 / elapsed.as_secs_f64();

    println!(
        "infer_completion_kind: {} ops in {:?} ({:.2} ns/op, {:.2} ops/sec)",
        total_ops, elapsed, ns_per_op, ops_per_sec
    );

    println!("=== Benchmark Complete ===");
}
