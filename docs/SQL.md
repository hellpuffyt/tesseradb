# The Tessera SQL dialect

Keywords are case-insensitive. Statements are separated by `;`. Comments start
with `--`. Strings use single quotes; `''` escapes a quote.

## Types

| Type | Aliases | Notes |
|---|---|---|
| `INT` | `INTEGER`, `BIGINT` | 64-bit signed, checked arithmetic |
| `FLOAT` | `REAL`, `DOUBLE` | 64-bit; an INT literal is coerced into a FLOAT column |
| `TEXT` | `STRING`, `VARCHAR` | UTF-8; `+` concatenates |
| `BOOL` | `BOOLEAN` | `TRUE` / `FALSE` |

`NULL` is allowed in any column not marked `NOT NULL` (primary keys are
implicitly `NOT NULL`). Comparisons with `NULL` yield `NULL`, which `WHERE`
treats as false; `AND`/`OR` follow three-valued logic.

## Statements

### CREATE TABLE
```sql
CREATE TABLE name (col TYPE [PRIMARY KEY] [NOT NULL], ...)
```
At most one `PRIMARY KEY` column; it gets an index automatically.

### CREATE INDEX
```sql
CREATE INDEX ON table (col)
```
Equality probes (`col = literal`) on the column use the index in `SELECT`,
`UPDATE` and `DELETE`.

### INSERT
```sql
INSERT INTO t VALUES (v1, v2, ...), (...)
INSERT INTO t (colA, colB) VALUES (a, b)
```
Unlisted columns are `NULL`. Values are expressions (no column references).

### SELECT
```sql
SELECT * | col, col, ... | COUNT(*)
FROM t
[AS OF TXN n]
[WHERE expr]
[ORDER BY col [ASC|DESC]]
[LIMIT n]
```
`AS OF TXN n` evaluates the query against the snapshot right after
transaction `n` committed. `n = 0` is the empty database; a future `n` is an
error. `SHOW TXN` reports the current snapshot id.

### HISTORY
```sql
HISTORY t [WHERE expr]
```
Returns every version of the matching rows, live or ended, with three leading
columns: `_row` (logical row id), `_xmin` (creating txn), `_xmax` (ending txn,
`0` if live). The `WHERE` clause is evaluated against each version's values.

### UPDATE / DELETE
```sql
UPDATE t SET col = expr, ... [WHERE expr]
DELETE FROM t [WHERE expr]
```
Expressions in `SET` may reference the row's current columns.

### Transactions
```sql
BEGIN            -- also START TRANSACTION
COMMIT           -- also END
ROLLBACK         -- also ABORT
```
Outside a transaction every statement autocommits. Inside one, the session
sees its own writes; others see nothing until `COMMIT`. `COMMIT` fails with
*write conflict* if another session ended a row this transaction also ended,
or with *duplicate primary key* if one raced it to the same key. On failure
nothing is applied.

### Streams
```sql
APPEND TO stream expr
READ stream [SINCE seq] [LIMIT n]
```
Streams are created on first append. `READ` returns `seq`, `txn`, `value`
ordered by `seq`, starting after `seq`. `txn` is the transaction that appended
the event — the same id you can pass to `AS OF TXN` to see the tables as they
were at that moment.

### SHOW
```sql
SHOW TABLES      -- table, columns, versions
SHOW STREAMS     -- stream, events
SHOW TXN         -- last_txn
```

## Expressions

Precedence, lowest to highest: `OR`, `AND`, `NOT`, comparison
(`= != <> < <= > >=`), `+ -`, `* /`, unary `-`, parentheses.

Ordering across types (used by comparisons, `ORDER BY`, and indexes):
`NULL < BOOL < numbers < TEXT`; INT and FLOAT compare numerically.

## Shell

```
tessera app.db                 interactive
tessera app.db -e "SQL"        one statement or several separated by ;
tessera app.db -f file.sql     a script
tessera --memory ...           scratch database
.checkpoint  .help  .quit      shell commands
```
The shell checkpoints on exit. Exit code is 1 if the `-e`/`-f` script failed.
