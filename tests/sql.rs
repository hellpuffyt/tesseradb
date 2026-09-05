use tesseradb::{Db, Output, Value};

fn t(s: &str) -> Value {
    Value::Text(s.into())
}

#[test]
fn crud_roundtrip() {
    let db = Db::open_in_memory();
    let mut s = db.session();
    s.execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL, age INT)")
        .unwrap();
    assert_eq!(
        s.execute("INSERT INTO users VALUES (1, 'ada', 36), (2, 'linus', 28), (3, 'grace', 85)")
            .unwrap(),
        Output::Affected(3)
    );
    let rows = s
        .query("SELECT name FROM users WHERE age > 30 ORDER BY age DESC")
        .unwrap();
    assert_eq!(rows, vec![vec![t("grace")], vec![t("ada")]]);

    assert_eq!(
        s.execute("UPDATE users SET age = age + 1 WHERE name = 'ada'")
            .unwrap(),
        Output::Affected(1)
    );
    assert_eq!(
        s.query("SELECT age FROM users WHERE id = 1").unwrap(),
        vec![vec![Value::Int(37)]]
    );
    assert_eq!(
        s.execute("DELETE FROM users WHERE age < 30").unwrap(),
        Output::Affected(1)
    );
    assert_eq!(
        s.query("SELECT COUNT(*) FROM users").unwrap(),
        vec![vec![Value::Int(2)]]
    );
    assert_eq!(
        s.query("SELECT id, name FROM users ORDER BY id LIMIT 1")
            .unwrap(),
        vec![vec![Value::Int(1), t("ada")]]
    );
}

#[test]
fn time_travel_and_history() {
    let db = Db::open_in_memory();
    let mut s = db.session();
    s.execute("CREATE TABLE task (id INT PRIMARY KEY, state TEXT)")
        .unwrap();
    s.execute("INSERT INTO task VALUES (1, 'queued')").unwrap();
    let t1 = db.last_txn();
    s.execute("UPDATE task SET state = 'running' WHERE id = 1")
        .unwrap();
    let t2 = db.last_txn();
    s.execute("UPDATE task SET state = 'done' WHERE id = 1")
        .unwrap();
    s.execute("INSERT INTO task VALUES (2, 'queued')").unwrap();

    let at = |s: &mut tesseradb::Session, txn: u64| {
        s.query(&format!(
            "SELECT state FROM task AS OF TXN {txn} WHERE id = 1"
        ))
        .unwrap()
    };
    assert_eq!(at(&mut s, t1), vec![vec![t("queued")]]);
    assert_eq!(at(&mut s, t2), vec![vec![t("running")]]);
    assert_eq!(
        s.query("SELECT state FROM task WHERE id = 1").unwrap(),
        vec![vec![t("done")]]
    );
    assert_eq!(
        s.query(&format!("SELECT COUNT(*) FROM task AS OF TXN {t1}"))
            .unwrap(),
        vec![vec![Value::Int(1)]]
    );
    assert_eq!(
        s.query("SELECT COUNT(*) FROM task AS OF TXN 0").unwrap(),
        vec![vec![Value::Int(0)]]
    );
    let hist = s.query("HISTORY task WHERE id = 1").unwrap();
    assert_eq!(hist.len(), 3, "three versions of row 1");
    assert_eq!(hist[0][4], t("queued"));
    assert_eq!(hist[2][2], Value::Int(0), "latest version has no xmax");
    assert!(s.query("SELECT * FROM task AS OF TXN 999").is_err());
}

#[test]
fn transactions_commit_and_rollback() {
    let db = Db::open_in_memory();
    let mut s = db.session();
    s.execute("CREATE TABLE t (id INT PRIMARY KEY, v INT)")
        .unwrap();
    s.execute("BEGIN").unwrap();
    s.execute("INSERT INTO t VALUES (1, 10)").unwrap();
    s.execute("UPDATE t SET v = 11 WHERE id = 1").unwrap();
    // Visible inside the transaction…
    assert_eq!(
        s.query("SELECT v FROM t").unwrap(),
        vec![vec![Value::Int(11)]]
    );
    // …but not to another session.
    let mut other = db.session();
    assert_eq!(
        other.query("SELECT COUNT(*) FROM t").unwrap(),
        vec![vec![Value::Int(0)]]
    );
    s.execute("ROLLBACK").unwrap();
    assert_eq!(
        s.query("SELECT COUNT(*) FROM t").unwrap(),
        vec![vec![Value::Int(0)]]
    );

    s.execute("BEGIN; INSERT INTO t VALUES (2, 20); DELETE FROM t WHERE id = 2; INSERT INTO t VALUES (2, 21); COMMIT")
        .unwrap();
    assert_eq!(
        other.query("SELECT v FROM t").unwrap(),
        vec![vec![Value::Int(21)]]
    );
    // Only one committed version survives the in-txn insert/delete/insert.
    assert_eq!(s.query("HISTORY t").unwrap().len(), 1);
}

#[test]
fn constraints_and_errors() {
    let db = Db::open_in_memory();
    let mut s = db.session();
    s.execute("CREATE TABLE t (id INT PRIMARY KEY, name TEXT NOT NULL, x FLOAT)")
        .unwrap();
    s.execute("INSERT INTO t VALUES (1, 'a', 1)").unwrap();
    let dup = s.execute("INSERT INTO t VALUES (1, 'b', 2)").unwrap_err();
    assert!(dup.contains("duplicate primary key"), "{dup}");
    let null = s.execute("INSERT INTO t VALUES (2, NULL, 2)").unwrap_err();
    assert!(null.contains("NOT NULL"), "{null}");
    let ty = s.execute("INSERT INTO t VALUES ('x', 'b', 2)").unwrap_err();
    assert!(ty.contains("type mismatch"), "{ty}");
    assert!(
        s.execute("SELECT * FROM nope")
            .unwrap_err()
            .contains("no such table")
    );
    assert!(
        s.execute("SELECT nope FROM t")
            .unwrap_err()
            .contains("no such column")
    );
    assert!(
        s.execute("CREATE TABLE t (id INT)")
            .unwrap_err()
            .contains("already exists")
    );
    assert!(s.execute("SELEC 1").is_err());
    // An INT literal is coerced into a FLOAT column; the failed inserts left no trace.
    assert_eq!(
        s.query("SELECT x FROM t").unwrap(),
        vec![vec![Value::Float(1.0)]]
    );
    assert_eq!(
        s.query("SELECT COUNT(*) FROM t").unwrap(),
        vec![vec![Value::Int(1)]]
    );
}

#[test]
fn secondary_index_matches_full_scan() {
    let db = Db::open_in_memory();
    let mut s = db.session();
    s.execute("CREATE TABLE ev (id INT PRIMARY KEY, kind TEXT, n INT)")
        .unwrap();
    for i in 0..500 {
        s.execute(&format!(
            "INSERT INTO ev VALUES ({i}, 'k{}', {})",
            i % 7,
            i * 3
        ))
        .unwrap();
    }
    let before = s
        .query("SELECT id FROM ev WHERE kind = 'k3' ORDER BY id")
        .unwrap();
    s.execute("CREATE INDEX ON ev (kind)").unwrap();
    let after = s
        .query("SELECT id FROM ev WHERE kind = 'k3' ORDER BY id")
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(after.len(), 71);
    s.execute("UPDATE ev SET kind = 'k3' WHERE id = 0").unwrap();
    s.execute("DELETE FROM ev WHERE id = 3").unwrap();
    let now = s
        .query("SELECT id FROM ev WHERE kind = 'k3' AND n >= 0 ORDER BY id")
        .unwrap();
    assert_eq!(now.len(), 71, "one added (0), one removed (3)");
    assert_eq!(now[0], vec![Value::Int(0)]);
}

#[test]
fn streams_are_transactional() {
    let db = Db::open_in_memory();
    let mut s = db.session();
    s.execute("APPEND TO log 'boot'").unwrap();
    s.execute("BEGIN; APPEND TO log 'never'; ROLLBACK").unwrap();
    s.execute("BEGIN; APPEND TO log 'a'; APPEND TO log 'b'; COMMIT")
        .unwrap();
    let all = s.query("READ log").unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0][2], t("boot"));
    assert_eq!(all[1][1], all[2][1], "a and b share a txn");
    assert_eq!(
        s.query("READ log SINCE 2 LIMIT 5").unwrap(),
        vec![vec![Value::Int(3), all[2][1].clone(), t("b")]]
    );
    assert_eq!(
        s.query("SHOW STREAMS").unwrap(),
        vec![vec![t("log"), Value::Int(3)]]
    );
}

#[test]
fn expressions() {
    let db = Db::open_in_memory();
    let mut s = db.session();
    s.execute("CREATE TABLE t (a INT, b FLOAT, c TEXT, d BOOL)")
        .unwrap();
    s.execute(
        "INSERT INTO t VALUES (7, 2.5, 'x', true), (-3, 0.5, 'y', false), (NULL, NULL, NULL, NULL)",
    )
    .unwrap();
    assert_eq!(
        s.query("SELECT a FROM t WHERE a * 2 - 1 = 13").unwrap(),
        vec![vec![Value::Int(7)]]
    );
    assert_eq!(
        s.query("SELECT a FROM t WHERE (a < 0 OR d) AND c != 'z' ORDER BY a")
            .unwrap(),
        vec![vec![Value::Int(-3)], vec![Value::Int(7)]]
    );
    assert_eq!(
        s.query("SELECT COUNT(*) FROM t WHERE NOT d").unwrap(),
        vec![vec![Value::Int(1)]]
    );
    assert_eq!(
        s.query("SELECT COUNT(*) FROM t WHERE a = NULL").unwrap(),
        vec![vec![Value::Int(0)]]
    );
    assert!(
        s.execute("SELECT a FROM t WHERE a / 0 = 1")
            .unwrap_err()
            .contains("division by zero")
    );
    assert_eq!(
        s.query("SELECT c FROM t WHERE c + '!' = 'x!'").unwrap(),
        vec![vec![t("x")]]
    );
}
