# Clawnicle

**Durable agent runtime for Rust.** Write an LLM-agent workflow as an ordinary async Rust function; the runtime journals every tool call, LLM completion, and decision to an append-only event log. When the process crashes and the workflow reopens, execution resumes from the last successful step — without re-running work that already completed.

> Temporal's durable-execution model, redesigned for LLM agents.

![crash-and-resume demo](docs/resume-demo.gif)

*Three runs of the bundled demo: the first crashes at step-b; the second resumes (step-a short-circuits from the journal, step-b retries, step-c runs fresh); and `clawnicle events` shows the full trace including the failed attempt.*

---

## What it gives you

- **Crash-resume.** Kill the process mid-workflow, start it again, and every completed step short-circuits from the journal. Only the unfinished work re-runs.
- **Deterministic replay.** Journal is the source of truth; workflow function is the recipe. Given the same journal, replay produces the same result.
- **Idempotent tool calls.** Each `cx.call("step_id", …)` is keyed by `step_id`; a second call with the same id in the same workflow returns the cached output.
- **Retry with exponential backoff + per-attempt timeout.** `call_with_retry` journals every attempt, so the retry history is inspectable after the fact.
- **Per-workflow budgets.** Cap tokens, USD cost, or wallclock milliseconds. Enforced pre-call and pre-retry; the next call after the breach returns `Error::BudgetExceeded("tokens")`.
- **Cooperative cancellation.** Clone a `CancelToken`, call `cancel()` from anywhere, and the runtime bails at the next suspension point.
- **LLM-aware prompt caching.** `complete_llm(provider, request)` hashes `(model, messages, temperature, max_tokens)` and stores the response; re-requesting the same prompt returns the cached completion without touching the provider.
- **Inspection CLI.** `clawnicle list / show / events` walks any journal — no special tooling needed.

## Example

```rust
use clawnicle_core::{RetryPolicy, Result};
use clawnicle_runtime::Context;

async fn scan_token(cx: &mut Context, mint: &str) -> Result<Verdict> {
    let dex = cx
        .call_with_retry(
            &format!("dex_fetch:{mint}"),
            RetryPolicy::exponential_3(),
            || async { dexscreener::fetch(mint).await },
        )
        .await?;

    let helius = cx
        .call(&format!("helius_fetch:{mint}"), || async {
            helius::fetch(mint).await
        })
        .await?;

    let verdict = run_gates(&dex, &helius);
    cx.complete(&verdict)?;
    Ok(verdict)
}
```

Kill the process between the DexScreener and Helius calls. Restart. The DexScreener call short-circuits from the journal; Helius runs for the first time.

## Measured performance (Mac Mini, release profile)

| Scenario | Time |
|---|---|
| `Journal::append` (small payload) | ~21 µs |
| `Journal::append` (2 KB payload) | ~34 µs |
| `Context::call` fresh | ~1.4 ms |
| `Context::call` cached (replay short-circuit) | ~3.7 µs |
| `Context::complete_llm` cached | ~4.5 µs |

Reproduction: `cargo bench -p clawnicle-bench`. Full methodology + caveats in [`docs/benchmarks.md`](docs/benchmarks.md).

## Quickstart

```bash
cargo test

# Two-event workflow trace
cargo run -p hello-clawnicle

# Crash-and-resume demo (same one as the GIF above)
cargo run -p resume-demo                        # crashes at step-b
CLAWNICLE_HEAL=1 cargo run -p resume-demo       # resumes from cached step-a
cargo run -p resume-demo                        # already-complete, cached output

# Durable LLM bullet summary — works with zero config via MockProvider
echo "some text to summarize" | cargo run -p summarize-agent

# Same demo against real Anthropic
ANTHROPIC_API_KEY=sk-ant-… echo "text" | \
    cargo run -p summarize-agent --features anthropic

# Inspect any workflow's journal
cargo run -p clawnicle-cli -- --db resume-demo.clawnicle.db list
cargo run -p clawnicle-cli -- --db resume-demo.clawnicle.db show   demo-1
cargo run -p clawnicle-cli -- --db resume-demo.clawnicle.db events demo-1

# Benchmarks
cargo bench -p clawnicle-bench
```

## Layout

```
crates/
  clawnicle-core       event model, errors, retry/budget/cancel/LLM types
  clawnicle-journal    SQLite-backed append-only event log
  clawnicle-llm        LlmProvider trait + MockProvider + AnthropicProvider
  clawnicle-runtime    Context API + replay engine + LLM caching
  clawnicle-cli        `clawnicle` binary for inspecting journals
examples/
  hello                minimal two-event workflow trace
  resume-demo          three-step workflow that crashes and resumes
  summarize-agent      durable LLM bullet-summary agent
bench/                 criterion benchmarks
docs/
  architecture.md      event model, replay semantics, determinism contract
  benchmarks.md        measured numbers + reproduction steps
  resume-demo.tape     VHS script for the README GIF
```

## Prior art

Temporal has a Rust SDK (alpha, general-purpose). Restate is Rust-native but model-agnostic and multi-language. `rig-rs` is agent-native but has no durability. `oracle.omen` is durable but LLM-blind. Clawnicle is the intersection: **Rust-native, agent-first, journaled, LLM-aware.**

## Status

v0 in progress. Weeks 1–5 shipped: workspace, event schema, SQLite journal, Context API with replay, retry + budgets + cancellation, LLM provider abstraction with Anthropic impl, inspection CLI, criterion benchmarks, and two runnable demos. Remaining: scheduler for concurrent workflows, a second non-crypto reference workload, and a launch writeup.

## License

Apache-2.0. See [`LICENSE`](LICENSE).
