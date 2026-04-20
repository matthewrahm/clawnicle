//! Runtime-level benchmarks. Measures the overhead of `Context::call` and
//! `Context::complete_llm` on top of the raw journal write.

use clawnicle_core::{LlmMessage, LlmRequest, LlmResponse};
use clawnicle_journal::Journal;
use clawnicle_llm::MockProvider;
use clawnicle_runtime::Context;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tempfile::TempDir;
use tokio::runtime::Runtime;

fn bench_context_call(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let journal = Journal::open(dir.path().join("j.db")).unwrap();
    let mut cx = Context::open_or_start(journal, "wf", "bench", "h").unwrap();

    let mut counter = 0u64;
    c.bench_function("context_call_noop_closure", |b| {
        b.iter(|| {
            counter += 1;
            let step = format!("s{counter}");
            let out: u32 = rt
                .block_on(async {
                    cx.call::<u32, _, _, std::io::Error>(black_box(&step), || async {
                        Ok(black_box(1u32))
                    })
                    .await
                })
                .unwrap();
            black_box(out);
        });
    });
}

fn bench_context_call_cached(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let journal = Journal::open(dir.path().join("j.db")).unwrap();
    let mut cx = Context::open_or_start(journal, "wf", "bench", "h").unwrap();

    // prime the cache
    rt.block_on(async {
        cx.call::<u32, _, _, std::io::Error>("cached-step", || async { Ok(42u32) })
            .await
            .unwrap();
    });

    c.bench_function("context_call_cached_short_circuit", |b| {
        b.iter(|| {
            let out: u32 = rt
                .block_on(async {
                    cx.call::<u32, _, _, std::io::Error>(black_box("cached-step"), || async {
                        unreachable!("should be cached")
                    })
                    .await
                })
                .unwrap();
            black_box(out);
        });
    });
}

fn bench_complete_llm_cached(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let journal = Journal::open(dir.path().join("j.db")).unwrap();
    let mut cx = Context::open_or_start(journal, "wf", "bench", "h").unwrap();

    let provider = MockProvider::new(vec![LlmResponse {
        model: "m".into(),
        content: "hi".into(),
        tokens_in: 4,
        tokens_out: 1,
    }]);
    let req = LlmRequest::new("m", vec![LlmMessage::user("hi")]);

    rt.block_on(async {
        cx.complete_llm("s", &provider, &req).await.unwrap();
    });

    c.bench_function("complete_llm_cached_short_circuit", |b| {
        b.iter(|| {
            let r = rt
                .block_on(async {
                    cx.complete_llm(black_box("s"), &provider, black_box(&req))
                        .await
                })
                .unwrap();
            black_box(r);
        });
    });
}

criterion_group!(
    benches,
    bench_context_call,
    bench_context_call_cached,
    bench_complete_llm_cached
);
criterion_main!(benches);
