# Benchmarks

All numbers measured with `cargo bench --quick` on a 2024 Mac Mini (M-series, macOS 15). Rust 1.94 release profile. Criterion 0.5 with HTML reports.

Reproduce:

```bash
cargo bench -p clawnicle-bench --bench journal_write -- --quick
cargo bench -p clawnicle-bench --bench runtime_call -- --quick
```

## Journal write latency

These measure the raw cost of persisting one event to a SQLite WAL journal. The architecture.md target is p99 < 5 ms; current measurements are well under that.

| Scenario | Time |
|---|---|
| `Journal::append` with small payload (`{"n": int}`) | **~21 µs** |
| `Journal::append` with 2 KB payload | **~34 µs** |
| `Journal::lookup_completed_tool_call` — hit in a workflow of 1 000 events | **~114 µs** |

The lookup number uses `json_extract` on the `payload` column, so it scales with total event count of the queried workflow — about 114 ns per event scanned. A production workload with workflows larger than ~10k steps will want an index on `json_extract(payload, '$.step_id')` or a denormalized `step_id` column.

## Runtime overhead

These are end-to-end costs of the user-facing API. Each number includes the cost of the underlying journal writes.

| Scenario | Time |
|---|---|
| `Context::call` — fresh closure, journals Started + Completed | **~1.4 ms** |
| `Context::call` — cached step, journal short-circuit | **~3.7 µs** |
| `Context::complete_llm` — cached prompt hash, journal short-circuit | **~4.5 µs** |

The fresh-call cost (~1.4 ms) reflects two journal appends plus async/tokio overhead. The cached-call cost (~3.7 µs) is the number that matters for replay: **a workflow with 100 completed steps re-opens and short-circuits in under half a millisecond.**

## Notes

- The fresh-call benchmark uses a single-process Tokio runtime and `block_on`; async ceremony accounts for a noticeable share of the 1.4 ms. A fully async harness would shave some of that off, but the journal writes dominate.
- SQLite WAL `synchronous = NORMAL` is the trade-off chosen in the schema: ACID properties are preserved within a connection, but a crash immediately after a commit can lose the tail of recent events. For stricter durability, flip to `FULL` at the cost of roughly 10× on append latency.
- Lookup-hit performance degrades linearly with workflow event count. At 10 000 events it would be ~1 ms; at 100 000 events, ~10 ms. A v1 optimization is a denormalized `step_id` column with an index.
- These numbers are illustrative, not promises. A future CI benchmark run against a pinned hardware profile is tracked for post-v0.
