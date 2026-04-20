//! Journal-write latency benchmark.
//!
//! Measures the cost of a single `Journal::append(ToolCallCompleted)` on a
//! SQLite WAL journal with a small payload. This is the p50/p99 target
//! called out in docs/architecture.md (target: p99 < 5ms on a Mac Mini).

use clawnicle_core::EventPayload;
use clawnicle_journal::Journal;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tempfile::TempDir;

fn bench_append_small(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let mut journal = Journal::open(dir.path().join("j.db")).unwrap();
    journal.start_workflow("wf", "bench", "h", None).unwrap();

    let mut counter = 0u64;
    c.bench_function("journal_append_small_payload", |b| {
        b.iter(|| {
            counter += 1;
            let payload = EventPayload::ToolCallCompleted {
                step_id: format!("s{counter}"),
                output: serde_json::json!({ "n": counter }),
                duration_ms: 1,
            };
            journal.append(black_box("wf"), black_box(&payload)).unwrap();
        });
    });
}

fn bench_append_medium(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let mut journal = Journal::open(dir.path().join("j.db")).unwrap();
    journal.start_workflow("wf", "bench", "h", None).unwrap();

    // ~2KB output payload
    let blob: String = "x".repeat(2048);
    let mut counter = 0u64;
    c.bench_function("journal_append_2kb_payload", |b| {
        b.iter(|| {
            counter += 1;
            let payload = EventPayload::ToolCallCompleted {
                step_id: format!("s{counter}"),
                output: serde_json::json!({ "blob": &blob }),
                duration_ms: 1,
            };
            journal.append(black_box("wf"), black_box(&payload)).unwrap();
        });
    });
}

fn bench_lookup_hit(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let mut journal = Journal::open(dir.path().join("j.db")).unwrap();
    journal.start_workflow("wf", "bench", "h", None).unwrap();

    for i in 0..1000 {
        journal
            .append(
                "wf",
                &EventPayload::ToolCallCompleted {
                    step_id: format!("step-{i}"),
                    output: serde_json::json!({ "i": i }),
                    duration_ms: 1,
                },
            )
            .unwrap();
    }

    c.bench_function("journal_lookup_hit_among_1000", |b| {
        b.iter(|| {
            let r = journal
                .lookup_completed_tool_call(black_box("wf"), black_box("step-500"))
                .unwrap();
            black_box(r);
        });
    });
}

criterion_group!(benches, bench_append_small, bench_append_medium, bench_lookup_hit);
criterion_main!(benches);
