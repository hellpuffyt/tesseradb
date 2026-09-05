# Roadmap

Ordered by value ÷ effort. Each item is scoped to one reviewable PR.

## 0.2 — operate safely

- [ ] **File lock** — refuse to open a database another process has open
  (`flock` / `LockFileEx`). Closes the one real data-loss hole in `SECURITY.md`.
- [ ] **`VACUUM TO TXN n`** — drop versions with `xmax <= n` and events older
  than `n`; the first way to make a file smaller.
- [ ] **`EXPLAIN`** — print whether a statement uses an index probe or a scan.
- [ ] **Composite primary keys** — index keys become tuples.

## 0.3 — query power

- [ ] Aggregates: `SUM`, `MIN`, `MAX`, `AVG`, `GROUP BY`.
- [ ] Inner join (nested loop, index-assisted when the join column is indexed).
- [ ] `ALTER TABLE … ADD COLUMN` (new versions carry the column; old ones read `NULL`).
- [ ] Range probes on indexes (`col > x AND col < y`).
- [ ] `AS OF` by wall-clock time: record a commit timestamp per txn.

## 0.4 — scale past memory

- [ ] Dirty-page tracking; checkpoint writes only changed pages.
- [ ] Persisted B-tree indexes so open time is O(1).
- [ ] Page cache with eviction; `Version` referenced by `(page, slot)`.

## Someday

- Stream subscriptions (`READ … FOLLOW`) with a wake-up channel.
- Encryption at rest via a caller-supplied cipher hook (never a home-grown one).
- A C ABI and a Python binding.

## Non-goals

- Client/server mode. Embed it, or put your own server in front.
- Full SQL compatibility. The dialect stays small enough to read in one sitting.
