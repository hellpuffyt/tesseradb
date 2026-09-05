# Changelog

All notable changes to TesseraDB. Format follows [Keep a Changelog](https://keepachangelog.com);
versions follow [SemVer](https://semver.org). On-disk format changes are called
out explicitly.

## [Unreleased]

## [0.1.0] — 2026-09-06

First release. On-disk format version 1.

### Added
- SQL subset: `CREATE TABLE` (INT/FLOAT/TEXT/BOOL, `PRIMARY KEY`, `NOT NULL`),
  `CREATE INDEX`, `INSERT`, `SELECT` (`WHERE`, `ORDER BY`, `LIMIT`,
  `COUNT(*)`), `UPDATE`, `DELETE`, `BEGIN`/`COMMIT`/`ROLLBACK`, `SHOW`.
- Temporal queries: `SELECT … AS OF TXN n` and `HISTORY table [WHERE …]`.
- Append-only streams: `APPEND TO s value`, `READ s [SINCE n] [LIMIT n]`.
- MVCC with optimistic conflict detection; concurrent readers never block.
- Slotted-page file format with overflow chains; CRC32-framed WAL with
  fsync-per-commit; atomic checkpoint; torn-tail-tolerant recovery.
- `tessera` shell with `-e`, `-f`, `--memory`, `.checkpoint`.
- Test suites: behaviour, recovery, concurrency, deterministic fuzz.
- Benchmark example.
