# Clawnicle

Durable agent runtime for Rust. Write LLM agent workflows as ordinary async functions; the runtime journals every tool call, LLM call, and decision so the workflow survives process crashes, replays deterministically, and enforces token/cost/time budgets.

> Temporal's durable-execution model, redesigned for LLM agents.

**Status:** v0 in progress. Week 1 ships workspace, event schema, SQLite journal, and a hello example.

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
  clawnicle-core      → event model, errors, shared types
  clawnicle-journal   → SQLite-backed append-only event log
examples/
  hello               → minimal two-event workflow trace
docs/
  architecture.md     → event model, replay semantics, determinism contract
```

## Quickstart

```bash
cargo test
cargo run -p hello-clawnicle
```

## License

Apache-2.0.
