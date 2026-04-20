# Clawnicle: durable execution for LLM agents, in Rust

Every LLM-agent framework I have touched over the last year falls over in roughly the same way in production.

Your agent is halfway through a multi-step task. Step 1 paid $0.08 to pull a document from Anthropic. Step 2 made a fetch that waited two seconds. Step 3 is mid-completion when the process dies, or the network drops, or the upstream provider returns a 500. You restart. The agent starts from the top. You pay for steps 1 and 2 again. You wait again. This happens over and over, and nobody admits it in the demo videos.

[Temporal](https://temporal.io/) solved this pattern for microservices a decade ago. Their durable-execution model records every side effect to an append-only log; when the workflow resumes, each step either short-circuits from the log or re-executes if it never completed. You get at-most-once semantics for expensive work without writing a single state machine by hand.

**Clawnicle** is that model, redesigned for LLM agents, in Rust. It is an ordinary async library, not a daemon. You keep writing agents the way you already do. The runtime journals every tool call and LLM completion, replays deterministically on restart, bounds token and cost budgets per workflow, and caches prompts by a stable hash so you never pay twice for the same completion. v0 is a five-crate workspace, 39 tests green, and a `cargo bench` that measures journal appends at ~21 µs and replay short-circuits at ~3.7 µs.

This post walks through the thesis, the event model, the replay engine, the LLM cache, and a few design decisions worth pulling out.

## What the API looks like

You write your workflow as a normal async function. Every tool call goes through a `Context`:

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

Kill the process between the DexScreener and Helius calls. Restart. The DexScreener call returns from the journal without re-executing the closure. The Helius call runs for the first time. The function never knows it was replayed.

The full scope is `cx.call`, `cx.call_with_retry`, `cx.complete_llm`, `cx.complete`, `cx.cached_final_output`. About what you would expect.

## The narrative demo

The bundled `resume-demo` binary is a four-step "research agent":

```
╭─ research agent ─────────────────────────────╮
│ topic: space travel                          │
╰──────────────────────────────────────────────╯

  1/4  Searching for articles      found 3 articles
  2/4  Extracting key points       extracted 5 points
  3/4  Writing summary             FAILED

  the workflow crashed: tool failed: writer service returned 503
  rerun with CLAWNICLE_HEAL=1 to resume.
  steps that already succeeded will be skipped.
```

On the second run:

```
  1/4  Searching for articles      skipped (already done)
  2/4  Extracting key points       skipped (already done)
  3/4  Writing summary             wrote 220-word summary
  4/4  Reviewing summary           looks good

  done. 2 steps skipped from the previous run.
```

That is the entire pitch, in human language. The rest of this post is the machinery underneath.

## The event journal

Every `cx.call`, `cx.complete_llm`, and `cx.complete` emits one or more events to a SQLite WAL journal. The schema is boring on purpose:

```sql
CREATE TABLE workflows (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  status      TEXT NOT NULL,
  input_hash  TEXT NOT NULL,
  parent_id   TEXT,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);

CREATE TABLE events (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  workflow_id  TEXT NOT NULL REFERENCES workflows(id),
  sequence     INTEGER NOT NULL,
  kind         TEXT NOT NULL,
  payload      TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  UNIQUE (workflow_id, sequence)
);
```

Per-workflow `sequence` is monotonic and enforced by a unique constraint. Status flips (`completed`, `failed`) happen in the same transaction as the terminal event, so "workflow is done" and "the WorkflowCompleted event is visible" are the same observation.

The event variants are a small enum:

```rust
pub enum EventPayload {
    WorkflowStarted  { name: String, input_hash: String },
    ToolCallStarted  { step_id, name, input_hash, attempt: u32 },
    ToolCallCompleted { step_id, output, duration_ms },
    ToolCallFailed   { step_id, error, duration_ms },
    LlmCallCompleted { step_id, model, prompt_hash, response, tokens_in, tokens_out },
    StepCompleted    { step_id, name, output },
    WorkflowCompleted { output },
    WorkflowFailed   { error },
}
```

Serialized as JSON, stored as TEXT, queried with `json_extract` where I need to filter by field.

## Replay, in one paragraph

When `Context::open_or_start` is called with a workflow id, the runtime looks up the workflow row. If the workflow is already in terminal state and the caller asks for `cached_final_output`, the workflow never re-executes; the final output comes straight from the terminating `WorkflowCompleted` event. Otherwise, the workflow function is re-invoked from the top. Each `cx.call(step_id, closure)` first asks the journal whether a `ToolCallCompleted` with this `step_id` already exists for this workflow. If it does, the cached output deserializes and returns; the closure is never called. If it doesn't, the closure runs, the outcome is journaled, and execution continues. Replay is not a special mode. The function body is the recipe; the journal is the source of truth.

The contract for users is short: any non-determinism has to flow through the runtime. `cx.call` and `cx.complete_llm` are the currently shipped suspension points; `cx.now` and `cx.random` are planned. Raw `SystemTime::now()` and `reqwest` break replay, and the runtime does not try to sandbox them in v0. Determinism is documented, not enforced.

## LLM completions and prompt caching

`cx.complete_llm(step_id, provider, request)` is the same story with a different key. Instead of looking for `ToolCallCompleted` with a matching `step_id`, it looks for `LlmCallCompleted` with a matching prompt hash. The hash is SHA-256 of the serialized `LlmRequest`:

```rust
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<LlmMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}
```

Two requests with the same model, messages, temperature, and max_tokens hash identically. Changing any one of those produces a new key, which is the invalidation mechanism. There is no explicit "bust this entry" API because I could not think of a case where you would want that without also changing one of the inputs.

The provider is any type implementing `LlmProvider`, a native async trait that returns `impl Future + Send`. There is a `MockProvider` for tests and an `AnthropicProvider` behind an `anthropic` feature flag. Adding OpenAI or any other Messages-like API is a couple of dozen lines of reqwest + serde glue.

Tokens from a fresh call are charged against the workflow budget. Cache hits are not re-charged. Budgets are per-session in v0; reconstructing spend from the journal on resume is straightforward but not wired up. The one concrete consequence is that a workflow which budget-fails mid-run, then resumes on a new process, will underreport historical spend until the first fresh charge. I took the simpler path on purpose.

## Retries, budgets, cancellation

`Context::call_with_retry(step_id, policy, closure)` runs the closure up to `policy.max_attempts` times, with exponential backoff bounded by `policy.max_backoff`. Each attempt appends its own `ToolCallStarted` (with the attempt number) and its own `ToolCallFailed` on error. A successful attempt writes a single `ToolCallCompleted` that terminates the sequence. The journal preserves the full retry history even though replay short-circuits at the first success.

Per-attempt timeout uses `tokio::time::timeout`. Timeouts look indistinguishable from any other failure in the journal; they show up as `ToolCallFailed { error: "timeout after 5s" }`.

`Budget` is three caps: max tokens, max USD in micros (1 USD = 1 000 000 micros, avoids floats), max wallclock milliseconds. `BudgetUsage` is the per-session counter. Enforcement happens at two points: before a new call, and between a failed attempt and its retry sleep. Deliberately not on success. A call that completes gets its output; the next call catches the breach. Otherwise a successful call whose duration happens to cross the cap would be reported as failed, which is the wrong signal.

`CancelToken` is a cloneable `Arc<AtomicBool>`. Clones share one flag; `token.cancel()` flips it. The runtime polls the flag at the same suspension points as budget. Cancellation is cooperative. Closures in flight are not interrupted. For hard interruption of a stuck closure, `RetryPolicy::with_timeout` is the escape hatch.

## Measured performance

On a Mac Mini (M-series, release profile), `cargo bench -p clawnicle-bench`:

| Scenario | Time |
|---|---|
| `Journal::append`, small payload | ~21 µs |
| `Journal::append`, 2 KB payload | ~34 µs |
| `Journal::lookup_completed_tool_call` among 1 000 events | ~114 µs |
| `Context::call` fresh (two appends + lookup + async) | ~1.4 ms |
| `Context::call` cached (journal lookup only) | ~3.7 µs |
| `Context::complete_llm` cached | ~4.5 µs |

The cached short-circuit at under 4 µs is the number that actually matters. A workflow with 100 completed steps re-opens and replays past them in under half a millisecond. That is the price of durability on this runtime.

The lookup bench uses `json_extract` on the `payload` column and scales with total event count in the queried workflow. About 114 ns per event scanned. Workloads with workflows above ~10 000 steps will want a denormalized `step_id` column with its own index; v1 material.

## A few design decisions worth pulling out

**Workflow is an async fn, not a trait.** Traits force users into an orthodoxy that closures do not, and I could not find a case where the trait bought anything at this scope.

**SQLite for v0.** I have production operational experience with SQLite WAL and none of the failure modes I cared about (crash consistency, concurrent reads during a writer, inspectability) needed Postgres. The `Journal` type is the only boundary that cares about SQLite; a Postgres backend is a drop-in for v1.

**Determinism is opt-in, not enforced.** I could have wrapped tokio in a sandbox, but every attempt at that I have read about ends in a tarpit. Document the contract, give escape hatches, add a lint later.

**Per-tool-call journal granularity.** Coarse enough to be cheap, fine enough to resume usefully. Event sourcing granularities are a tuning knob; this is the default.

**step_id is the user-provided idempotency key.** Hashing the input automatically is tempting but introduces surprise: two calls that the user thought were the same turn out to hash differently because of a timestamp or a random nonce inside the input. Better to ask the user, once, what "same call" means.

**LLM cache keys on request-hash, not step_id.** An LLM completion is deterministic (modulo temperature) on its inputs; two different call sites that happen to build the identical request should share the cached result. Keying on `step_id` would have been more familiar but less correct for this case.

**Native async trait via RPITIT, no `async-trait` macro.** One fewer dependency, no heap allocation on every trait call, cleaner error messages. The trade-off is one line of ceremony in the trait signature. Worth it.

## Prior art, and the gap

Temporal has a Rust SDK. It is alpha, general-purpose, and not agent-specific. [Restate](https://www.restate.dev/) is Rust-native but multi-language and model-agnostic. [rig-rs](https://rig.rs/) is agent-native but has zero durability; in-process state only, crashes lose everything. `oracle.omen` is durable but has no LLM integration and is niche. `dbos`, `inngest`, and friends are durable but Python and TypeScript first.

The lane that is open in April 2026: a Rust-native library that assumes the LLM agent shape (tool calls, model completions, prompt caching, token budgets) rather than trying to generalize across all durable workflows. That is the lane Clawnicle is in.

## What is next

- **A scheduler.** Today the runtime runs one workflow per process. Week 6 adds a pull loop that reads pending workflows from the SQLite queue, runs them as Tokio tasks, and scans for `running` workflows on startup to resume anything a prior process abandoned.

- **Sub-workflows.** `parent_id` is already in the schema. `cx.spawn_child(workflow_fn, input)` would wire a parent/child link, propagate budget, and let the parent either await the child or fire-and-forget.

- **Postgres backend.** Trivial with the `Journal` boundary already in place.

- **Language bindings.** Rust is the control plane; Python and TypeScript workflows are a separate concern. PyO3 first.

- **A real case study.** I run a 24/7 agent on a Mac Mini ("Zanmu") that does token monitoring and paper trading. Porting its scanner onto Clawnicle is the next real-world validation.

None of that is blocking for the launch. The hard systems work (journal, replay, retry, LLM cache) is done.

## Try it

Repo is at [github.com/matthewrahm/clawnicle](https://github.com/matthewrahm/clawnicle). Apache-2.0. The quickstart is three `cargo run` commands and the README walks through them. The crash-and-resume GIF is at the top of the README.

Issues, ideas, pull requests welcome. I am especially interested in workloads where the durability model breaks down in ways I have not foreseen.

---

*Written April 2026. Repo, docs, and benchmarks available at the link above. The launch HN submission lives [here](#).*
