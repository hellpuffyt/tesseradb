# Architecture

TesseraDB is four modules and about 2,500 lines.

```
 SQL text ──► lexer.rs ──► parser.rs ──► Stmt
                                          │
                     ┌────────────────────┴───────────────────┐
                     │ engine.rs                              │
                     │  Session ── Pending (buffered writes)  │
                     │  Db ── RwLock<Inner>                   │
                     │        tables: name → Table            │
                     │          versions: Vec<Version>        │
                     │          indexes: col → BTreeMap       │
                     │        streams: name → Vec<Event>      │
                     │        last_txn                        │
                     └──────────────┬─────────────────────────┘
                                    │ commit / checkpoint / recover
                     ┌──────────────┴─────────────────────────┐
                     │ storage.rs                             │
                     │  <db>      page file (slotted pages)   │
                     │  <db>.wal  CRC32 frames, fsync/commit  │
                     └────────────────────────────────────────┘
```

## Data model

A **table** is a schema plus a list of **versions**. A version is
`(row_id, xmin, xmax, values)`:

- `row_id` identifies the logical row across its versions.
- `xmin` is the transaction that created the version.
- `xmax` is the transaction that ended it, or `0` while it is live.

A version is **visible at snapshot `s`** iff `xmin <= s && (xmax == 0 || xmax > s)`.
Because commits are serialised and transaction ids are assigned in commit
order, a snapshot is just an integer — the id of the last committed
transaction — and `AS OF TXN n` is the same visibility test with `s = n`.

`UPDATE` is `end the old version (xmax = txn)` + `insert a new version
(xmin = txn)`. `DELETE` only sets `xmax`. Nothing is ever overwritten, which is
what makes `HISTORY` and `AS OF` free.

A **stream** is an ordered list of `(seq, txn, value)`. `APPEND` is a
transactional op like any other; `seq` is assigned at commit.

## Concurrency and the commit protocol

- `Db` holds `RwLock<Inner>`. Every read statement takes the read guard and
  evaluates against a snapshot; readers never block each other.
- A `Session` buffers writes in a `Pending`: the logical ops to log, the rows
  it has inserted (with temporary ids), and the `(table, row_id)` pairs it has
  deleted. Reads inside the transaction overlay the buffer on the committed
  state, so a session sees its own writes.
- `COMMIT` takes `commit_lock` (serialising commits), then the write guard, and:
  1. **validates** — no duplicate primary keys against the *current* state,
     and every deleted row is still live (otherwise: *write conflict*);
  2. **logs** — encodes the ops as one WAL frame and `fsync`s it;
  3. **applies** — mutates the in-memory tables with `txn = last_txn + 1`;
  4. **bumps** `last_txn`, making the snapshot visible to new readers atomically.

This is optimistic concurrency: two sessions updating the same row both
proceed, the second to commit fails. It is the right trade for an embedded
database whose writers are usually short and rarely contend.

Autocommit statements are a one-op transaction through the same path.

## Storage

### Page file

Page 0 is the header: magic `TSRA`, format version, page size, page count,
`last_txn` and a CRC of those fields. Pages 1..n are **slotted pages**:

```
[n_slots u16][free_end u16][slot0: off u16, len u16][slot1]…    …records…
```

Records grow down from the end of the page; slots grow up. Four record kinds:
schema (with `next_row_id`), row version, stream event, and secondary index
definition. A record larger than a page is written as a 13-byte stub
`[0xFF][first_page u64][len u32]` followed by raw **overflow pages**; the stub
precedes its pages so a sequential reader knows what to skip.

The page file is only written at **checkpoint**: the engine serialises every
record, writes `<db>.tmp`, `fsync`s, renames it over `<db>` (atomic on every
supported OS), and truncates the WAL. Checkpoints run on close, on demand
(`.checkpoint` / `Db::checkpoint`), and automatically once the WAL exceeds
8 MiB.

### Write-ahead log

Each commit appends one frame: `[len u32][crc32 u32][payload]`, where the
payload is `txn`, an op count, and the logical ops (create table, create
index, insert, delete, append). `fsync` happens before the commit is
acknowledged.

### Recovery

`Db::open` loads the page file (rejecting bad magic, bad header CRC, or a
page count that disagrees with the file size), then replays WAL frames in
order, **skipping any frame whose txn ≤ header.last_txn** (already
checkpointed — this makes a crash between rename and WAL truncation harmless)
and **stopping at the first frame whose length or CRC is wrong** (a torn
write at the tail). Indexes are rebuilt in memory as records load.

## Indexes

Every table with a `PRIMARY KEY` has an index on it; `CREATE INDEX ON t (col)`
adds a secondary one. An index maps a value to the list of version indices
holding it (all versions, live or not — visibility is checked at read time, so
`AS OF` queries use indexes too). The planner uses an index when the `WHERE`
clause contains a top-level `col = literal` conjunct; `SELECT`, `UPDATE` and
`DELETE` all go through the same probe.

## Query execution

There is no separate planner/optimiser. `scan()` picks candidates (index probe
or full scan), filters by visibility and predicate, overlays the session's
pending writes, then `ORDER BY` sorts and `LIMIT` truncates. Expressions are
evaluated by a small tree-walking evaluator with SQL three-valued logic for
`NULL`.

## Deliberate simplifications (and their upgrade paths)

| Simplification | Ceiling | Upgrade path |
|---|---|---|
| Whole database resident in memory | ~ hundreds of MiB | Page cache with pinning; versions referenced by (page, slot) |
| Checkpoint rewrites every page | Checkpoint cost ∝ db size | Dirty-page tracking, in-place page writes |
| Indexes rebuilt on open | Open time ∝ db size | Persist B-tree pages |
| Single-column primary keys, no joins | Expressiveness | Composite keys are a parser + index-key change; joins are a nested-loop scan first |
| No history GC | File grows forever | `VACUUM TO TXN n` drops versions with `xmax <= n` |
