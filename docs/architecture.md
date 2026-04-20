# Clawnicle Architecture

This document describes the event model, replay semantics, and determinism contract that make Clawnicle workflows durable. It is meant to be read alongside the source — paths are relative to the repo root.

## The thesis in one paragraph

A Clawnicle workflow is an ordinary async Rust function that takes a `&mut Context`. Every tool call the workflow makes goes through `Context::call(step_id, async_closure)`. Each call is journaled as an append-only event. When the process crashes and the workflow is re-opened, the function runs again from the top — but every `call()` whose `step_id` already has a `ToolCallCompleted` event in the journal short-circuits from cache without invoking the closure. The journal is the source of truth; the function body is just a recipe for replaying it.

## Event model

Events live in `crates/clawnicle-core/src/event.rs`. The `EventPayload` enum is the canonical set:

| Variant | When emitted | Replay role |
|---|---|---|
| `WorkflowStarted` | First open of a new workflow id | Marks the beginning of the log. |
| `ToolCallStarted` | Before invoking a user closure via `Context::call` | Diagnostic; not used for replay short-circuit. |
| `ToolCallCompleted` | Closure returned `Ok(_)` | The cache entry. Replay reads `output` from here. |
| `ToolCallFailed` | Closure returned `Err(_)` | Informational; does not affect replay. Re-execution retries. |
| `LlmCallCompleted` | (future) an LLM completion finished | Cache entry for prompt-hash-based caching. |
| `StepCompleted` | (future) Local deterministic step finished | Like ToolCallCompleted but for local pure steps. |
| `WorkflowCompleted` | `Context::complete(&output)` called successfully | Terminates the workflow; `cached_final_output` reads this. |
| `WorkflowFailed` | (future) unrecoverable error | Terminates the workflow. |

Each event row in SQLite carries `(workflow_id, sequence, kind, payload_json, created_at)`. The `sequence` is monotonic per workflow — no global ordering assumption.

## Replay semantics

The core guarantee:

> For any workflow function `f`, if `f` was previously executed and step with `step_id = X` produced output `O`, then every subsequent re-execution of `f` observes output `O` at the same `call(X, ...)` site without re-invoking the closure.

Implementation (`crates/clawnicle-runtime/src/context.rs`):

```text
Context::call(step_id, closure):
    1. lookup_completed_tool_call(workflow_id, step_id)
    2. if cached: return deserialize(cached) — closure never runs
    3. else:
         a. append ToolCallStarted
         b. execute closure
         c. on Ok:  append ToolCallCompleted with output, return output
         d. on Err: append ToolCallFailed, return Error::Tool
```

### Consequences

- **Step ids are idempotency keys.** Two `call()`s with the same `step_id` in the same workflow are assumed to describe the same logical operation. This is the user's contract to uphold.
- **Failures are not cached.** `ToolCallFailed` does not short-circuit. On replay, a previously-failed step re-executes — which is usually what you want (transient errors should retry).
- **Replay must be deterministic before the cached step.** If the workflow function branches on `SystemTime::now()` or `rand::random()`, the function path leading to `call(X, ...)` may differ across runs, and the runtime cannot protect you. See the determinism contract below.

## Determinism contract

For replay to be safe, the following must hold for any workflow function:

1. **All non-determinism flows through the runtime.** The runtime-provided primitives (currently `Context::call`; future: `Context::now`, `Context::random`, `Context::sleep`) are the only sources of entropy. Raw `SystemTime::now()`, `std::thread::sleep()`, and direct `reqwest` calls bypass the journal and break replay.
2. **Control flow is a pure function of (input, journaled outputs).** Given the same starting input and the same sequence of `call()` outputs, the function must produce the same call graph.
3. **`step_id` is stable across runs.** If you interpolate variable data into the id (e.g. `format!("fetch:{}", mint)`), the variable must itself be deterministic at that call site.

Violations are the user's responsibility in v0 — the runtime does not enforce them. A future version may gate calls behind a sandboxed executor or lint for forbidden patterns.

## Journal storage

`crates/clawnicle-journal/src/schema.rs` defines two tables:

```sql
workflows (id, name, status, input_hash, parent_id, created_at, updated_at)
events    (id, workflow_id, sequence, kind, payload, created_at)
```

- `journal_mode = WAL` for concurrent reads during a writer.
- Per-workflow `sequence` is monotonic; `UNIQUE (workflow_id, sequence)` enforces this.
- `status` is one of `running | completed | failed`. `WorkflowCompleted` / `WorkflowFailed` events transactionally flip the status in the same commit as the event insert.
- `parent_id` is reserved for sub-workflows (spawned via a future `Context::spawn_child`).

Storage is abstracted behind `clawnicle_journal::Journal`. Swapping to Postgres or FoundationDB is a v1 change; nothing above the journal crate knows about SQLite.

## Retries

`Context::call_with_retry(step_id, policy, closure)` runs the closure up to `policy.max_attempts` times, with exponential backoff bounded by `policy.max_backoff` between attempts. Each attempt appends its own `ToolCallStarted` event (with an incrementing `attempt` field), and its own `ToolCallFailed` event on error. When an attempt succeeds, a single `ToolCallCompleted` event terminates the sequence — the journal preserves the full retry history for observability.

Per-attempt timeout is implemented with `tokio::time::timeout`. A timeout is indistinguishable from any other failure from the journal's perspective: `ToolCallFailed { error: "timeout after ...", ... }`.

Replay short-circuits at the first `ToolCallCompleted` for the step — even if it was reached after 4 failed attempts in the original run.

## Budgets

`Context::with_budget(Budget)` attaches caps on tokens, USD (tracked in micros, `1 USD == 1_000_000`), and wallclock milliseconds. `BudgetUsage` is a per-session counter updated by:

- `Context::charge_tokens(n)` — called by the LLM provider (Week 4) after each completion with `tokens_in + tokens_out`.
- `Context::charge_usd_micros(n)` — same, converted via model price table.
- wallclock — updated automatically after every tool-call attempt.

Enforcement points:

1. **Before a new call_inner.** If usage already exceeds any cap, the call fails with `Error::BudgetExceeded(field_name)` before journaling anything.
2. **Between a failed attempt and its retry.** Avoids burning budget retrying when already over.

Deliberately NOT enforced at call-success time: a call that completes gets its output; the next call catches the breach. A call whose own duration crosses the cap is not treated as failed.

Budget state is per-session — a process restart resets wallclock to zero. Tokens and USD could be reconstructed from journaled `LlmCallCompleted` events on resume; wallclock is inherently process-local. (Reconstruction is not implemented in v0.)

## LLM completions and prompt caching

`Context::complete_llm(step_id, provider, request)` takes any type implementing `clawnicle_llm::LlmProvider` (a native async trait using return-position `impl Future`). On the first call, the provider is invoked and an `LlmCallCompleted` event is appended carrying the model, SHA-256 prompt hash (stable hash of `(model, messages, temperature, max_tokens)`), response text, and token counts in/out.

On any subsequent call (same process or after restart) where a `LlmCallCompleted` event with the same prompt hash exists for this workflow, the cached response is returned and the provider is not touched.

Token counts from a fresh call are charged against the workflow budget via `charge_tokens`. Cache hits are NOT re-charged: budgets are per-session in v0, and reconstruction from the journal is a v1 concern. This means a workflow that budget-fails in the middle, is resumed on a new process, and reaches cached LLM calls will underreport usage versus the true historical spend — a trade-off for simplicity.

Invalidation of the cache is by changing the hash: editing the prompt, switching models, or changing temperature all produce a new key. There is no explicit "bust this cache entry" API; `step_id` uniqueness is not required for LLM calls (it is informational only — the prompt hash is the real key).

## Cancellation

`CancelToken` is a cloneable `Arc<AtomicBool>`. Any clone can flip the flag. Attach one via `Context::with_cancel_token`; the runtime polls the flag at the same suspension points as budget enforcement:

- Before a new `call_inner`.
- Between a failed attempt and its retry sleep.

Cancellation is cooperative — user closures in flight are not interrupted. For hard interruption of a stuck closure, use `RetryPolicy::with_timeout`. External callers (scheduler, CLI, signal handler) cancel by calling `token.cancel()` on any clone of the token.

## What is NOT in v0

- **No scheduler.** Workflows run in-process, one at a time. The scheduler arrives in Week 4.
- **No LLM caching.** `LlmCallCompleted` is reserved in the event enum but not wired up until the LLM provider lands in Week 4.
- **No sub-workflows.** `parent_id` is a placeholder column.
- **No distributed workers.** Single process only. Multi-node is post-v0.
- **No determinism enforcement.** Documented contract; no runtime checks.

## Why SQLite for v0

- Ramen has production operational experience with SQLite WAL from Zanmu.
- Single-file journals are trivially inspectable (`sqlite3 journal.db "SELECT * FROM events"`) — valuable during development.
- Zero operational overhead: no separate service, no migrations beyond the schema string.
- Benchmarking: journal-write p99 under 5ms on a Mac Mini is the v0 target; SQLite delivers this easily.
- When v0 outgrows SQLite, the `Journal` type is the only boundary that changes.
