//! Many writers and readers on one database: no lost updates, no torn
//! snapshots, and optimistic conflicts are reported rather than silently
//! overwriting.

use std::{sync::Arc, thread};

use tesseradb::{Db, Value};

#[test]
fn parallel_inserts_are_all_committed_once() {
    let db = Db::open_in_memory();
    db.session()
        .execute("CREATE TABLE t (id INT PRIMARY KEY, worker INT)")
        .unwrap();
    let handles: Vec<_> = (0..8)
        .map(|w| {
            let db = Arc::clone(&db);
            thread::spawn(move || {
                let mut s = db.session();
                for i in 0..250 {
                    s.execute(&format!("INSERT INTO t VALUES ({}, {w})", w * 1000 + i))
                        .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let mut s = db.session();
    assert_eq!(
        s.query("SELECT COUNT(*) FROM t").unwrap(),
        vec![vec![Value::Int(2000)]]
    );
    assert_eq!(db.last_txn(), 2001);
}

#[test]
fn readers_see_consistent_snapshots_while_writers_run() {
    let db = Db::open_in_memory();
    db.session()
        .execute("CREATE TABLE acct (id INT PRIMARY KEY, bal INT)")
        .unwrap();
    db.session()
        .execute("INSERT INTO acct VALUES (1, 500), (2, 500)")
        .unwrap();
    let writer = {
        let db = Arc::clone(&db);
        thread::spawn(move || {
            let mut s = db.session();
            for _ in 0..300 {
                // Move 1 unit between accounts atomically; total is always 1000.
                s.execute(
                    "BEGIN; UPDATE acct SET bal = bal - 1 WHERE id = 1; \
                     UPDATE acct SET bal = bal + 1 WHERE id = 2; COMMIT",
                )
                .unwrap();
            }
        })
    };
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let db = Arc::clone(&db);
            thread::spawn(move || {
                let mut s = db.session();
                for _ in 0..200 {
                    let rows = s.query("SELECT bal FROM acct").unwrap();
                    let total: i64 = rows
                        .iter()
                        .map(|r| match r[0] {
                            Value::Int(n) => n,
                            _ => 0,
                        })
                        .sum();
                    assert_eq!(total, 1000, "torn snapshot");
                }
            })
        })
        .collect();
    writer.join().unwrap();
    for r in readers {
        r.join().unwrap();
    }
    assert_eq!(
        db.session()
            .query("SELECT bal FROM acct WHERE id = 1")
            .unwrap(),
        vec![vec![Value::Int(200)]]
    );
}

#[test]
fn concurrent_update_of_same_row_conflicts() {
    let db = Db::open_in_memory();
    db.session()
        .execute("CREATE TABLE t (id INT PRIMARY KEY, v INT); INSERT INTO t VALUES (1, 0)")
        .unwrap();
    let mut a = db.session();
    let mut b = db.session();
    a.execute("BEGIN; UPDATE t SET v = 1 WHERE id = 1").unwrap();
    b.execute("BEGIN; UPDATE t SET v = 2 WHERE id = 1").unwrap();
    a.execute("COMMIT").unwrap();
    let err = b.execute("COMMIT").unwrap_err();
    assert!(err.contains("write conflict"), "{err}");
    assert_eq!(
        db.session().query("SELECT v FROM t").unwrap(),
        vec![vec![Value::Int(1)]]
    );
    // Duplicate keys across sessions are caught at commit too.
    a.execute("BEGIN; INSERT INTO t VALUES (9, 9)").unwrap();
    b.execute("BEGIN; INSERT INTO t VALUES (9, 8)").unwrap();
    a.execute("COMMIT").unwrap();
    assert!(
        b.execute("COMMIT")
            .unwrap_err()
            .contains("duplicate primary key")
    );
}
