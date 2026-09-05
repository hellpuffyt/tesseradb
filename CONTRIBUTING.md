# Contributing

Thanks for looking. The codebase is small on purpose; please keep it that way.

## Ground rules

- **Zero dependencies stays zero.** If a feature needs a crate, open an issue
  first so we can talk about whether the feature belongs here.
- **No `unsafe`.** CI fails on it.
- **Every change to `engine.rs` or `storage.rs` comes with a test** in the
  matching suite (`TESTING.md` says which). Durability changes need a
  recovery test that actually reopens a file.
- **Format and lint:** `cargo fmt` and `cargo clippy --all-targets -- -D warnings`
  must be clean.
- Keep PRs to one idea. A `VACUUM` PR should not also rename things.

## Workflow

```
git clone https://github.com/hellpuffyt/tesseradb
cd tesseradb
cargo test
cargo run --release --example bench   # before and after, for engine changes
```

Open a PR against `main`. The template asks what you changed, how you tested
it, and whether the on-disk format changed (if it did, bump `FORMAT` in
`storage.rs` and say so in `CHANGELOG.md`).

## Where things live

| Want to… | Look in |
|---|---|
| Add a statement or clause | `parser.rs` (AST + grammar), then `engine.rs::Session::run` / `read` / `stage` |
| Change visibility, MVCC, commit | `engine.rs` — `Version::visible`, `Session::commit` |
| Change the file format | `storage.rs` and `engine.rs::Inner::{records, load_record}` |
| Add a shell command | `main.rs` |

## Reporting bugs

Use the bug template. A failing test is the best bug report; a SQL script that
reproduces it with `tessera --memory -f repro.sql` is the second best.
