# Zanmu-scanner Port Plan

Plan to port [Zanmu]'s TypeScript token-scanner pipeline onto Clawnicle as a real-world reference workload. Not yet executed.

[Zanmu]: Matthew's 24/7 agent running on a Mac Mini, source at `~/zanmu-workspace/projects/ronin/packages/scanner/`.

## Why

The `summarize-agent` example proves Clawnicle works for a short LLM pipeline. It is not a proof that the runtime holds up under:

- **Long-lived workflows.** Scanner runs for hours on a single "polling session" that surfaces tokens and opens trades.
- **High tool-call density.** Every candidate goes through 4 gates, each with one or more external HTTP calls. A 30-second polling window can produce 200+ tool invocations.
- **Fan-out workloads.** Multiple tokens scanned concurrently per polling window. `Context::spawn_child` is the right primitive but `summarize-agent` does not exercise it.
- **Real failure modes.** Helius 429s, DexScreener timeouts, partial exits mid-trade. These are the problems that motivated Clawnicle; the port is the first time they get exercised end-to-end.

A working port produces three concrete artifacts:

1. A GitHub-visible example that recruiters can browse: `examples/token-scanner/` (intentionally NOT named "zanmu" or "ronin" so the repo does not read as crypto-branded).
2. Benchmark numbers under a realistic workload. The current benchmarks are micro (single journal append, single call). A scanner workflow with 30+ tool calls per run gives an end-to-end wall-clock number.
3. A case study section in the blog post: "here is what happened when I ran the existing Zanmu scanner pipeline on top of Clawnicle for 24 hours."

## Scope

**In scope:**

- Port the scanner's 4-gate pipeline to a Rust workflow that takes a token mint as input and returns a `Verdict`.
- Implement the three external data sources as tool calls: DexScreener fetch, Helius RPC fetch, PumpFun bonding-curve fetch. Each goes through `cx.call_with_retry` with a real retry policy.
- Use `cx.spawn_child` to run each candidate token as a sub-workflow of a per-polling-window parent.
- Use `Budget` to cap per-session API cost at a fixed USD value.
- Hook the completed verdict into a simple sink (stdout + SQLite table in the same DB the journal lives in).
- Integration test that runs the workflow against a mocked external API set and asserts crash-resume semantics on a killed mid-run process.

**Out of scope (keeps the example bounded):**

- Real trade execution. The port is paper-trading only; no actual Solana transactions.
- The full Zanmu agent stack (Telegram control plane, KAGE, autonomous logic). Only the scanner pipeline is ported.
- Migration of the existing production Zanmu database. The port runs on its own journal file.
- Renaming anything in the existing Zanmu codebase. The port lives next to it, does not replace it.

## Non-crypto counterpart

To preserve the repo's positioning (MANGO-readable first, crypto-adjacent second), ship a second example in the same PR:

- `examples/code-reviewer/` — a workflow that reads a git diff, runs N parallel `spawn_child` reviewers over file chunks, aggregates their comments, and produces a final review. Uses `complete_llm` and exercises the same sub-workflow + budget + retry primitives as the token scanner.

Both examples prove Clawnicle transferability without the repo looking domain-specific. The README points at the code-reviewer as the headline; the token-scanner is listed as "a second reference workload that exercises the same primitives under real-world API pressure."

## Architecture sketch

```rust
// examples/token-scanner/src/main.rs
#[tokio::main]
async fn main() -> Result<()> {
    let mut scheduler = Scheduler::new("scanner.clawnicle.db").with_concurrency(8);
    scheduler.register("scan_polling_window", scan_polling_window);
    scheduler.register("scan_token",          scan_token);

    // Every 30s, enqueue a polling-window parent workflow.
    loop {
        let window_id = format!("window-{}", now_ms());
        scheduler.submit(&window_id, "scan_polling_window", ...)?;
        scheduler.run_until_idle().await?;
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

async fn scan_polling_window(mut cx: Context, _input: Value) -> Result<Value> {
    let candidates = cx.call_with_retry(
        "fetch_pumpfun_candidates",
        RetryPolicy::exponential_3(),
        || async { pumpfun::fetch_recent_migrations().await },
    ).await?;

    let mut verdicts = Vec::new();
    for (i, mint) in candidates.into_iter().enumerate() {
        let child_id = format!("{}::{}", cx.workflow_id(), i);
        let v = cx.spawn_child(&child_id, "scan_token",
            &serde_json::json!({ "mint": mint }),
            scan_token_handler,
        ).await?;
        verdicts.push(v);
    }

    cx.complete(&verdicts)?;
    Ok(serde_json::to_value(verdicts)?)
}

async fn scan_token(mut cx: Context, input: Value) -> Result<Value> {
    let mint = input["mint"].as_str().unwrap();
    let dex     = cx.call_with_retry(...).await?;  // gate 1
    let helius  = cx.call_with_retry(...).await?;  // gate 2
    let pump    = cx.call_with_retry(...).await?;  // gate 3
    let sec     = cx.call_with_retry(...).await?;  // gate 4
    let verdict = run_gates(dex, helius, pump, sec);
    cx.complete(&verdict)?;
    Ok(serde_json::to_value(verdict)?)
}
```

The parent workflow is durable end-to-end. If the polling-window process is killed mid-way through scanning 50 candidates, a restart with the same `window_id` resumes: every already-completed child returns its cached verdict, the in-flight one retries, and unseen ones run for the first time.

## Estimated effort

~8 commits, 3-5 focused hours. Rough breakdown:

1. `examples/token-scanner/` scaffold: Cargo.toml, main.rs, types module, stub external-API modules.
2. Real DexScreener client (HTTP). Feature-gated behind `scanner-live` so tests can run without network.
3. Real Helius RPC client (WebSocket for price, HTTP for account lookup). Same feature flag.
4. Real PumpFun bonding-curve fetcher (Anchor account decode).
5. Stub RugCheck client (or skip — security gate becomes a local function).
6. `scan_token` workflow function + unit test under mocked tools.
7. `scan_polling_window` parent workflow + integration test using tempdir journal + mocked tools + simulated mid-run crash.
8. README example block + docs mention.

Parallel track (same PR or immediately after):

- `examples/code-reviewer/` (est. 4 commits, 2 hours).
- Benchmarks refresh: add a "realistic scanner workload" bench that chains `scan_token` 100 times under mocked tools and measures wall-clock.

## What this unlocks for the launch

The blog post currently ends at "here is a runnable demo." A successful port lets the post end at:

> *"I ran the Zanmu token-scanner pipeline on Clawnicle for 24 hours against live PumpFun data. 11 400 tool calls. 46 simulated mid-run crashes. Zero wasted LLM spend. Median polling-window wall-clock: X seconds. Full logs in the companion repo."*

That paragraph is the difference between "interesting library" and "battle-tested library" for a reader deciding whether to star or close the tab.

## Open questions before execution

- Does the production Zanmu scanner run in the same process as Clawnicle, or is the port a parallel fork? Recommend parallel — keeps the production agent running on the existing known-good code while the port is validated.
- Should the port use real Anthropic (via `--features anthropic`) for any of the gates, or keep the gates fully algorithmic? Recommend algorithmic; adding an LLM gate is a separate experiment.
- What is the success metric? Recommend: *24 hours of parallel running, comparing verdict stream parity between old Zanmu scanner and Clawnicle-ported scanner over the same PumpFun firehose.*
