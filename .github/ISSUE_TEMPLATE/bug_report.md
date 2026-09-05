---
name: Bug report
about: Something behaves wrongly, loses data, or panics
labels: bug
---

**What happened**

**What you expected**

**Reproduce**

```sql
-- the smallest script that shows it; `tessera --memory -f repro.sql` if possible
```

**Environment**
- TesseraDB version / commit:
- OS and filesystem:
- Rust version (`rustc -V`):

**Did the database survive?** If a file is involved and you can share it,
attach `<db>` and `<db>.wal` (they may contain your data — redact first).
