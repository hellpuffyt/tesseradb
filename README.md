# TesseraDB

**An embedded database where the past is a first-class query.**

TesseraDB is a single-file, zero-dependency SQL database written in Rust. Every
row version is retained, so any query can be asked *as of* an earlier
transaction, and append-only event streams live in the same transactions and
the same write-ahead log as your tables.

```sql
CREATE TABLE task (id INT PRIMARY KEY, state TEXT, owner TEXT);
INSERT INTO task VALUES (7, 'queued', 'planner');
UPDATE task SET state = 'running' WHERE id = 7;      -- txn 3
UPDATE task SET state = 'failed'  WHERE id = 7;      -- txn 4
APPEND TO audit 'task 7 failed: timeout';

SELECT state FROM task WHERE id = 7;                 -- failed
SELECT state FROM task AS OF TXN 3 WHERE id = 7;     -- running
HISTORY task WHERE id = 7;                           -- all three versions, with xmin/xmax
READ audit SINCE 0;                                  -- seq, txn, value
```

```
$ cargo install --path .
$ tessera app.db
TesseraDB 0.1.0 — type .help for help, .quit to exit
tessera> SELECT id, state FROM task ORDER BY id;
id │ state
───┼───────
7  │ failed
(1 row)
```

## What is it?

A relational engine with three things most embedded databases don't have
together:

| Capability | What it means |
|---|---|
| **Temporal by default** | Rows are never overwritten. `AS OF TXN n` reads any past snapshot; `HISTORY` shows every version of a row with the transaction that created and ended it. |
| **Event streams** | `APPEND TO stream …` / `READ stream SINCE n` are transactional, durable, ordered — and roll back with the tables they were written next to. |
| **Optimistic MVCC** | Readers never block. Writers buffer locally and validate at `COMMIT`; a conflicting concurrent write fails loudly instead of silently winning. |

It is one crate, no dependencies, ~2.5k lines, and it runs on Linux, macOS and
Windows.

## Who is it for?

- **AI agents and workflow engines** that need to answer "what did the world
  look like when the agent made that decision?" without building an audit table
  by hand.
- **Local-first applications** that want durable, transactional state and an
  event log in one file.
- **People learning how databases work.** The whole engine — pages, WAL,
  recovery, MVCC, indexes, a parser — is readable in an afternoon.

## Why does it exist?

Most embedded databases treat history as the application's problem: you add
`updated_at` columns, audit tables, triggers, and hope they stay in sync.
Systems that do version rows (Datomic, Dolt, XTDB) are large servers or JVM
runtimes. TesseraDB puts bitemporal-style *transaction time* into a file you can
`cargo add`, with an event stream next to it, because agent systems produce
both structured state and a stream of things that happened, and you usually
need to correlate them.

## What makes it different?

- **Every commit is a snapshot.** There is no "enable versioning" flag; time
  travel is how the storage works. Snapshot ids are transaction ids, so an
  event's `txn` column joins directly to the table state it was written with.
- **Streams and tables share a transaction.** `BEGIN; UPDATE …; APPEND TO …;
  COMMIT` is atomic. Roll back and the event is gone too.
- **Torn-write-safe by construction.** Every WAL frame carries a CRC; recovery
  replays up to the first bad frame and discards the rest. The page file is
  replaced atomically at checkpoint. Both are tested (`tests/recovery.rs`).
- **No dependencies.** Not even for CRC or hashing. Auditable end to end.

## Why is this not just a tutorial?

Tutorial databases stop at "parse SQL, keep a `Vec` of rows". TesseraDB has a
slotted-page file format with overflow chains for large rows, a checksummed WAL
with idempotent replay after checkpoint, MVCC visibility with a real
`(xmin, xmax)` model, optimistic conflict detection across threads, secondary
indexes that the planner actually uses for `UPDATE`/`DELETE` as well as
`SELECT`, and a fuzz suite that throws random token soup and random WAL bytes
at it. The recovery tests kill the process's view of the database *without*
checkpointing and assert on what comes back.

## Benchmarks

`cargo run --release --example bench` on a Windows 11 laptop (NVMe), 0.1.0:

| Operation | Disk (fsync per commit) | In memory |
|---|---|---|
| Insert, autocommit | 506 ops/s (fsync bound) | 564 k ops/s |
| Insert, 45 k rows in one transaction | 487 k ops/s | 500 k ops/s |
| Point lookup by primary key | 833 k ops/s | 798 k ops/s |
| Indexed lookup (500 matching rows) | 27.6 k ops/s | 26.2 k ops/s |
| Full scan, 50 k rows | 893 scans/s | 601 scans/s |
| Update by primary key, autocommit | 1.09 k ops/s | 388 k ops/s |
| `AS OF` scan, 50 k versions | 5.8 k scans/s | 3.1 k scans/s |
| Checkpoint, 55 k versions | 26 ms | 22 ms |

Autocommit on disk is dominated by `fsync`; batch writes in a transaction when
you can. Numbers are from one run and will vary by machine.

## Status and limits

This is a 0.1 research-grade engine, not a drop-in SQLite replacement.
Honest limits:

- The whole database is resident in memory; the page file is the durable
  format, not a cache. Fine up to a few hundred MiB; see `ROADMAP.md`.
- SQL is a deliberate subset: no joins, no aggregates beyond `COUNT(*)`, no
  `GROUP BY`, no `ALTER TABLE`, one primary key column per table.
- History is never garbage-collected yet (`VACUUM TO TXN n` is on the roadmap).
- One process at a time may open a file; there is no file lock yet.

## Build, test, run

```
cargo build --release          # binary at target/release/tessera
cargo test                     # unit, integration, recovery, concurrency, fuzz
cargo run --release --example bench
tessera app.db -f examples/agent_tasks.sql
```

Minimum Rust: 1.88 (edition 2024).

## Using it as a library

```rust
use tesseradb::{Db, Value};

let db = Db::open(std::path::Path::new("app.db"))?;
let mut s = db.session();
s.execute("CREATE TABLE kv (k TEXT PRIMARY KEY, v TEXT)")?;
s.execute("INSERT INTO kv VALUES ('greeting', 'hello')")?;
let rows = s.query("SELECT v FROM kv WHERE k = 'greeting'")?;
assert_eq!(rows[0][0], Value::Text("hello".into()));
db.checkpoint()?;
```

`Db` is `Send + Sync`; open it once, share it in an `Arc`, and give each thread
its own `Session`.

## Documentation

- [`docs/SQL.md`](docs/SQL.md) — the dialect, statement by statement
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — pages, WAL, MVCC, commit protocol
- [`TESTING.md`](TESTING.md) — what each test suite proves
- [`SECURITY.md`](SECURITY.md) — threat model and reporting
- [`ROADMAP.md`](ROADMAP.md) — what's next, in order
- [`CONTRIBUTING.md`](CONTRIBUTING.md)

## Why would you star or contribute?

Star it if you want a database you can read cover to cover and still trust with
a crash. Contribute if you want to implement a real feature end to end — a
`VACUUM`, a join, a file lock, a B-tree on disk — in a codebase small enough
that your change is visible in the architecture, not lost in it. Every issue in
the roadmap is scoped to be a single, reviewable PR.

## License

MIT. See [`LICENSE`](LICENSE).
