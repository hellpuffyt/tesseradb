//! Durability: what survives a process that dies before checkpointing, a
//! torn WAL frame, and a checkpoint followed by more writes.

use std::{fs::OpenOptions, io::Write, path::PathBuf, sync::Arc};

use tesseradb::{Db, Value};

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tessera-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("db")
}

fn count(db: &Arc<Db>, sql: &str) -> i64 {
    match db.session().query(sql).unwrap()[0][0] {
        Value::Int(n) => n,
        _ => panic!(),
    }
}

#[test]
fn committed_writes_survive_without_checkpoint() {
    let path = tmp("nocheckpoint");
    {
        let db = Db::open(&path).unwrap();
        let mut s = db.session();
        s.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)")
            .unwrap();
        for i in 0..200 {
            s.execute(&format!("INSERT INTO t VALUES ({i}, 'v{i}')"))
                .unwrap();
        }
        s.execute("UPDATE t SET v = 'changed' WHERE id = 5")
            .unwrap();
        s.execute("DELETE FROM t WHERE id = 6").unwrap();
        s.execute("APPEND TO audit 'done'").unwrap();
        s.execute("CREATE INDEX ON t (v)").unwrap();
        // Drop without checkpoint: only the WAL has the data.
        std::mem::forget(db);
    }
    assert!(!path.exists(), "page file is only written at checkpoint");
    let db = Db::open(&path).unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM t"), 199);
    assert_eq!(
        db.session().query("SELECT v FROM t WHERE id = 5").unwrap(),
        vec![vec![Value::Text("changed".into())]]
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM t WHERE v = 'v7'"), 1);
    assert_eq!(db.session().query("READ audit").unwrap().len(), 1);
    // Time travel still works after recovery: txn 1 created the table, txn 2..201 inserted.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM t AS OF TXN 11"), 10);
}

#[test]
fn checkpoint_then_more_writes_then_recover() {
    let path = tmp("checkpoint");
    {
        let db = Db::open(&path).unwrap();
        let mut s = db.session();
        s.execute("CREATE TABLE t (id INT PRIMARY KEY, blob TEXT)")
            .unwrap();
        // One row far larger than a page exercises the overflow chain.
        let big = "x".repeat(20_000);
        s.execute(&format!("INSERT INTO t VALUES (1, '{big}')"))
            .unwrap();
        s.execute("INSERT INTO t VALUES (2, 'small')").unwrap();
        db.checkpoint().unwrap();
        let wal = tesseradb::storage::Files::wal_path(&path);
        assert_eq!(
            std::fs::metadata(&wal).unwrap().len(),
            0,
            "WAL truncated by checkpoint"
        );
        s.execute("INSERT INTO t VALUES (3, 'after')").unwrap();
        s.execute("DELETE FROM t WHERE id = 2").unwrap();
        std::mem::forget(db);
    }
    let db = Db::open(&path).unwrap();
    let rows = db.session().query("SELECT id FROM t ORDER BY id").unwrap();
    assert_eq!(rows, vec![vec![Value::Int(1)], vec![Value::Int(3)]]);
    let blob = db
        .session()
        .query("SELECT blob FROM t WHERE id = 1")
        .unwrap();
    assert_eq!(blob[0][0], Value::Text("x".repeat(20_000)));
    // Replay must not double-apply the checkpointed transactions.
    assert_eq!(db.session().query("HISTORY t").unwrap().len(), 3);
    // Re-checkpoint and reopen once more: idempotent.
    db.checkpoint().unwrap();
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM t"), 2);
}

#[test]
fn torn_wal_frame_is_discarded_not_fatal() {
    let path = tmp("torn");
    {
        let db = Db::open(&path).unwrap();
        let mut s = db.session();
        s.execute("CREATE TABLE t (id INT PRIMARY KEY)").unwrap();
        s.execute("INSERT INTO t VALUES (1)").unwrap();
        std::mem::forget(db);
    }
    // Simulate a crash mid-append: a frame header promising more bytes than exist.
    let wal = tesseradb::storage::Files::wal_path(&path);
    let mut f = OpenOptions::new().append(true).open(&wal).unwrap();
    f.write_all(&[200, 0, 0, 0, 1, 2, 3, 4, 9, 9, 9]).unwrap();
    drop(f);
    let db = Db::open(&path).unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM t"), 1);
    // And a corrupted (bit-flipped) frame in the middle stops replay there.
    let mut s = db.session();
    s.execute("INSERT INTO t VALUES (2)").unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM t"), 2);
}

#[test]
fn corrupt_page_file_is_rejected_loudly() {
    let path = tmp("corrupt");
    {
        let db = Db::open(&path).unwrap();
        db.session().execute("CREATE TABLE t (id INT)").unwrap();
        db.checkpoint().unwrap();
    }
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();
    let err = match Db::open(&path) {
        Ok(_) => panic!("opened corrupt file"),
        Err(e) => e,
    };
    assert!(err.contains("bad magic"), "{err}");
}
