# Production readiness — tesseradb

Audit date: 2026-10-22. Tested commit: `beef7f4` on branch
`audit/production-readiness` (this file was added in the next commit; no
code changed after the tested commit).

## Scope

TesseraDB is an embedded, single-file, zero-dependency SQL database with
transaction-time versioning (`AS OF TXN`, `HISTORY`), transactional
append-only streams, optimistic MVCC, a CRC-framed WAL and atomic
checkpoints. Architecture: [`../ARCHITECTURE.md`](../ARCHITECTURE.md);
dialect: [`SQL.md`](SQL.md). Maturity: research-grade 0.1.0 (README "Status
and limits").

## Platforms and prerequisites

Rust ≥ 1.88. Verified here on macOS 27 (Apple silicon, rustc 1.98.0, APFS).
Default-branch CI covers ubuntu, windows, macos.

## Clean-clone verification (this audit)

```
git clone -b audit/production-readiness <repo> cc && cd cc
cargo build --release                                   # exit 0
cargo test                                              # exit 0: 27 passed, 0 failed, 0 ignored
cargo install --path . --root <scratch>                 # exit 0 (README uses plain `cargo install --path .`;
                                                        #  --root kept the audit out of ~/.cargo)
tessera app.db -f examples/agent_tasks.sql              # exit 0, 3-row task table
printf 'SELECT id, state FROM task ORDER BY id;\n.quit\n' | tessera app.db   # exit 0
tessera app.db -e "SELECT state FROM task AS OF TXN 2 WHERE id = 1"          # queued
cargo run --release --example bench                     # exit 0
```
The README's SQL walkthrough was replayed statement by statement: `failed`
now, `running` AS OF TXN 3, three HISTORY versions (xmin/xmax 2/3, 3/4, 4/0),
one audit event at txn 5 — all as documented. The CI "real execution" step
was replayed with `bash -eo pipefail`: pass.

## Commands executed (exit codes)

| Command | Before (main `feb9c71`) | After (`beef7f4`) |
|---|---|---|
| `cargo fmt --all -- --check` | 0 | 0 |
| `cargo clippy --all-targets -- -D warnings` | 0 | 0 |
| `cargo clippy --all-targets --all-features --locked -- -D warnings` | — | 0 |
| `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --locked` | — | 0 |
| `cargo test --release` | 0: 18 passed, 0 failed, 0 ignored | 0: 27 passed, 0 failed, 0 ignored |
| `cargo test` (debug) | — | 0: 27 passed |
| `cargo audit --db <advisory-db 2026-10-22> --no-fetch` | 0 | 0 (1 crate) |
| `gitleaks git` / `gitleaks dir` | — | no leaks found |

## CI status

Default branch (`main`, `feb9c71`): **success** (run 33985500965).
Audit branch: pending until pushed.

## Bugs found and fixed

| # | Severity | Defect | Fix | Regression test |
|---|---|---|---|---|
| 1 | High (data loss) | After a crash left a torn WAL frame, `Db::open` replayed up to it but left it in the file; later commits were appended (and fsynced) *behind* the garbage, and the next recovery stopped at the same place — silently losing every transaction since the first crash. | `read_wal` truncates the file to the last intact frame and fsyncs | `recovery::commits_after_recovering_a_torn_tail_survive_the_next_crash` (failed before: count 1 ≠ 2) |
| 2 | Medium (DoS / broken promise) | Hostile or corrupt page files and CRC-valid WAL frames panicked `Db::open`: slot table running past the page (index out of bounds), overflow chain `first + n` overflow, row with fewer values than columns (index out of bounds), index on unknown column (`unwrap` on `None`), `id + 1` / `row_id + 1` overflow | bounds/arity/overflow checks returning errors; `build_index`/`add_version` now return `Result` | `tests/hostile_files.rs` (8 tests; 7 panicked before) |
| 3 | Low (CI) | `! grep unsafe` could never fail under `bash -e` | `#![forbid(unsafe_code)]`; CI checks the attribute | — |
| 4 | Docs | SECURITY.md cited `cargo geiger` (not run) and a maintainer email in `Cargo.toml` (none exists) | reworded; private-advisory link | — |

## Security review

- Untrusted inputs: SQL text (fuzzed; checked arithmetic), database/WAL
  files (now hardened, see above). No network surface, no string-built SQL.
- Documented non-guarantees kept: no file lock (two processes can clobber
  each other), CRC is not a MAC, unbounded result sets / literals.
- History: 3 commits; no secret hits (pattern scan over all revisions +
  gitleaks). No rotation needed.
- Workflow: `permissions: contents: read`; actions pinned by tag (unchanged).

## Known limitations / unresolved risks

- `tessera -f script.sql` prints only the last statement's result.
- Truncating at the first bad frame makes the loss of any intact frames
  that followed a mid-file corruption permanent (they were already ignored
  by the documented recovery rule).
- If `apply` fails mid-way during a live commit after the WAL append (not
  reachable through validated SQL), memory and WAL could diverge until
  reopen.
- No file locking; whole database resident in memory (documented).
- Benchmarks in README are from a Windows laptop; this Mac measured e.g.
  248 autocommit inserts/s on disk (fsync-bound), 0.83 M PK lookups/s.
- License is MIT only (kept).

## Acceptance checklist

| Item | Expected | Command / test | Result | Actual | Reference |
|---|---|---|---|---|---|
| Build + tests from clean clone | exit 0 | `cargo build --release && cargo test` | pass | 27/27 | `tests/` |
| Time travel and history | historical values | `sql::time_travel_and_history`, README replay | pass | as documented | `src/engine.rs` |
| Transactions + streams atomic | rollback removes events | `sql::transactions_commit_and_rollback`, `sql::streams_are_transactional` | pass | ok | `src/engine.rs` |
| Durability without checkpoint | WAL replay | `recovery::committed_writes_survive_without_checkpoint` | pass | ok | `src/storage.rs` |
| Torn tail discarded and later commits durable | no loss | `recovery::torn_wal_frame_*`, `recovery::commits_after_recovering_*` | pass | ok | `src/storage.rs` |
| Hostile files rejected, no panic | `Err` | `tests/hostile_files.rs`, `fuzz::random_wal_bytes_*` | pass | ok | `src/storage.rs`, `src/engine.rs` |
| Optimistic concurrency | conflicts fail loudly, no lost updates | `tests/concurrency.rs` | pass | ok | `src/engine.rs` |
| Index used and correct | index == full scan | `sql::secondary_index_matches_full_scan` | pass | ok | `src/engine.rs` |
| CLI as documented | -e / -f / REPL | clean-clone commands | pass | ok | `src/main.rs` |
| Dependency/secret audit | clean | `cargo audit`, gitleaks | pass | clean | — |

## Classification proposal

**Partially Verified** — the documented scope passes from a clean clone on
macOS with regression tests for a real data-loss bug and hostile-file
panics; Linux/Windows rely on CI (green on `main`, pending here), crash
behaviour is simulated (process "forget" + byte edits), not power-cut
tested.
