# Clawnicle

Durable agent runtime for Rust. Write LLM agent workflows as ordinary async functions; the runtime journals every tool call, LLM call, and decision so the workflow survives process crashes, replays deterministically, and enforces token/cost/time budgets.

> Temporal's durable-execution model, redesigned for LLM agents.

**Status:** v0 in progress. Weeks 1–2 shipped: workspace, event schema, SQLite journal, `Context` API, replay engine with step-level short-circuit, and a runnable crash-and-resume demo.

## Why

Every agent framework today (LangGraph, CrewAI, custom loops) shares the same failure modes in production:

- Process dies mid-workflow → progress is lost
- Tool calls hang with no retry/timeout policy
- Reruns aren't reproducible
- No observability into past trajectories
- No global enforcement of token/USD/wallclock budgets

Clawnicle makes durability, idempotency, and replay the default, so your agent code can be ordinary async Rust.

```rust
async fn scan_token(cx: Context, token: TokenId) -> Result<Verdict> {
    let dex    = cx.call("dex_fetch",    &token).await?;
    let helius = cx.call("helius_fetch", &token).await?;
    let gates  = cx.step("run_gates",    || gates(&dex, &helius)).await?;
    if gates.passes {
        cx.spawn_child(open_trade, gates.trade_req).await?;
    }
    Ok(gates.verdict)
}
```

## Layout

```
crates/
  clawnicle-core       → event model, errors, shared types
  clawnicle-journal    → SQLite-backed append-only event log
  clawnicle-runtime    → Context API + replay engine
examples/
  hello                → minimal two-event workflow trace
  resume-demo          → three-step workflow that crashes and resumes
docs/
  architecture.md      → event model, replay semantics, determinism contract
```

## Quickstart

```bash
cargo test

# Two-event journal trace
cargo run -p hello-clawnicle

# Crash-and-resume demo — run once to see step-b fail, then rerun with
# CLAWNICLE_HEAL=1 to see step-a short-circuit from the journal and the
# workflow complete.
cargo run -p resume-demo
CLAWNICLE_HEAL=1 cargo run -p resume-demo
cargo run -p resume-demo  # already-complete path
```

Read [`docs/architecture.md`](docs/architecture.md) for the event model, replay semantics, and determinism contract.

## License

Apache-2.0.
