# Security

## Threat model

TesseraDB is an **embedded** library: it runs inside your process, with your
process's privileges, on files your process can already read and write. It has
no network surface and no authentication layer — those belong to the
application embedding it.

What it does defend against:

| Threat | Defence | Evidence |
|---|---|---|
| Crash or power loss mid-commit | WAL frames are CRC32-checked; recovery discards the torn tail. Page file replaced by atomic rename. | `tests/recovery.rs` |
| Corrupt or hostile database file | Every decode is bounds-checked (`Reader`); bad magic / header CRC / page count / slot offsets are errors, never panics or out-of-bounds reads. | `tests/recovery.rs::corrupt_page_file_is_rejected_loudly`, `tests/fuzz.rs::random_wal_bytes_never_panic_recovery` |
| Hostile SQL text (from users, LLM output, …) | Parser and evaluator are pure functions of their input; no string-built SQL inside the engine; integer arithmetic is checked (overflow and division by zero are errors). | `tests/fuzz.rs::random_token_soup_never_panics`, `tests/sql.rs::expressions` |
| Lost updates under concurrency | Optimistic validation at commit: conflicting writes fail loudly. | `tests/concurrency.rs` |
| Memory-unsafety | 100% safe Rust, zero `unsafe`, zero dependencies (`cargo geiger` reports none; CI runs `cargo audit` on the empty lock file to keep it that way). | `grep -r unsafe src` |

What it does **not** defend against:

- An attacker with write access to the database or WAL file can alter or
  delete data. The CRC is a corruption check, not an integrity MAC. Use
  filesystem permissions or an encrypted volume.
- Denial of service via huge inputs: a `SELECT` with no `WHERE` on a large
  table allocates the result; a 1 GiB string literal is parsed into memory.
  Bound input sizes at your application boundary.
- Two processes opening the same file concurrently. There is no file lock
  yet (see `ROADMAP.md`); the second writer's checkpoint could clobber the
  first's. Only one process may open a database at a time.
- Timing side channels. Nothing here is constant-time.

## Reporting a vulnerability

Open a GitHub security advisory on this repository (Security → Report a
vulnerability) or email the maintainer listed in `Cargo.toml`. Please include a
reproducer. You will get an acknowledgement within 7 days.

## Supported versions

Only the latest `0.x` release receives fixes.
