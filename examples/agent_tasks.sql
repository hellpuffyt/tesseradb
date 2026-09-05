-- An agent's task board with an audit stream. Run with:
--   tessera app.db -f examples/agent_tasks.sql
-- Then try:
--   tessera app.db -e "HISTORY task WHERE id = 1"
--   tessera app.db -e "SELECT state FROM task AS OF TXN 2 WHERE id = 1"
--   tessera app.db -e "READ audit"

CREATE TABLE task (id INT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL, attempts INT);
INSERT INTO task VALUES (1, 'implement WAL recovery', 'queued', 0);   -- txn 2
INSERT INTO task VALUES (2, 'write benchmarks', 'queued', 0),
                        (3, 'review security notes', 'queued', 0);

BEGIN;
UPDATE task SET state = 'running', attempts = attempts + 1 WHERE id = 1;
APPEND TO audit 'task 1 started by worker-a';
COMMIT;

BEGIN;
UPDATE task SET state = 'failed' WHERE id = 1;
APPEND TO audit 'task 1 failed: test timeout';
COMMIT;

BEGIN;
UPDATE task SET state = 'running', attempts = attempts + 1 WHERE id = 1;
APPEND TO audit 'task 1 retried by worker-b';
COMMIT;

UPDATE task SET state = 'done' WHERE id = 1;
CREATE INDEX ON task (state);

SELECT id, title, state, attempts FROM task ORDER BY id;
