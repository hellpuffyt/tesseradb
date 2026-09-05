# Testing

`cargo test` runs everything below in well under ten seconds. No test asserts
a constant; each one drives the engine through a real path and checks an
observable outcome.

| Suite | File | What it proves |
|---|---|---|
| SQL behaviour | `tests/sql.rs` | CRUD, ordering and limits, `AS OF` returns the historical value, `HISTORY` shows every version, transactions isolate and roll back, constraint and type errors leave no trace, secondary index results equal full-scan results, streams are transactional, expression semantics incl. `NULL` and checked arithmetic. |
| Durability | `tests/recovery.rs` | Committed writes survive when the process never checkpointed (WAL only); checkpoint + further writes + recovery neither loses nor double-applies anything; a 20 KB row round-trips through overflow pages; a torn WAL tail is discarded; a corrupt page file is rejected with a clear error. |
| Concurrency | `tests/concurrency.rs` | 8 threads × 250 autocommit inserts all land exactly once; readers running against a writer doing two-row transfers always see a total of 1000 (no torn snapshots); two sessions updating the same row → second commit reports a write conflict; duplicate keys across sessions are rejected at commit. |
| Fuzz | `tests/fuzz.rs` | 20 000 random statements from the grammar's token set never panic the engine; 5 000 random byte strings never panic the parser; 200 random WAL files never panic recovery. Deterministic (xorshift seed) so failures reproduce. |
| Doc test | `src/lib.rs` | The README's time-travel example compiles and runs. |

## Running subsets

```
cargo test --test recovery
cargo test --test concurrency -- --nocapture
cargo test time_travel
```

## Benchmarks

`cargo run --release --example bench` measures insert (autocommit and
batched), primary-key lookup, indexed lookup, full scan, update, `AS OF`
scan, and checkpoint, on disk and in memory. Results are printed as ops/s and
µs/op. They are not asserted in CI (machines differ); regressions are caught
by eye when a PR changes `engine.rs` or `storage.rs`.

## Adding a test

- Put behaviour tests in `tests/sql.rs`; anything touching files in
  `tests/recovery.rs`; anything with threads in `tests/concurrency.rs`.
- Use `Db::open_in_memory()` unless the test is about durability.
- Temporary files go under `std::env::temp_dir()` with the process id in the
  name so parallel test runs don't collide.
