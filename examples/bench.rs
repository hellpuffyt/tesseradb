//! `cargo run --release --example bench` — throughput of the core paths
//! on a real on-disk database (fsync per commit) and in memory.

use std::{path::PathBuf, time::Instant};

use tesseradb::Db;

fn report(name: &str, n: usize, t: Instant) {
    let secs = t.elapsed().as_secs_f64();
    println!(
        "{name:<44} {n:>8} ops  {:>10.0} ops/s  {:>8.1} µs/op",
        n as f64 / secs,
        secs * 1e6 / n as f64
    );
}

fn run(db: &std::sync::Arc<Db>, label: &str) {
    let mut s = db.session();
    s.execute("CREATE TABLE kv (k INT PRIMARY KEY, v TEXT, n INT)")
        .unwrap();

    let n = 5_000;
    let t = Instant::now();
    for i in 0..n {
        s.execute(&format!(
            "INSERT INTO kv VALUES ({i}, 'value-{i}', {})",
            i % 100
        ))
        .unwrap();
    }
    report(
        &format!("[{label}] insert, autocommit (1 fsync each)"),
        n,
        t,
    );

    let t = Instant::now();
    s.execute("BEGIN").unwrap();
    for i in n..n * 10 {
        s.execute(&format!(
            "INSERT INTO kv VALUES ({i}, 'value-{i}', {})",
            i % 100
        ))
        .unwrap();
    }
    s.execute("COMMIT").unwrap();
    report(&format!("[{label}] insert, one transaction"), n * 9, t);

    let t = Instant::now();
    for i in 0..n {
        s.query(&format!("SELECT v FROM kv WHERE k = {}", i * 7 % (n * 10)))
            .unwrap();
    }
    report(&format!("[{label}] point lookup by primary key"), n, t);

    let t = Instant::now();
    for _ in 0..20 {
        s.query("SELECT COUNT(*) FROM kv WHERE n = 42").unwrap();
    }
    report(&format!("[{label}] full scan of 50k rows"), 20, t);

    s.execute("CREATE INDEX ON kv (n)").unwrap();
    let t = Instant::now();
    for _ in 0..2_000 {
        s.query("SELECT COUNT(*) FROM kv WHERE n = 42").unwrap();
    }
    report(&format!("[{label}] indexed lookup (500 rows)"), 2_000, t);

    let t = Instant::now();
    for i in 0..n {
        s.execute(&format!("UPDATE kv SET n = n + 1 WHERE k = {i}"))
            .unwrap();
    }
    report(
        &format!("[{label}] update by primary key, autocommit"),
        n,
        t,
    );

    let t = Instant::now();
    for _ in 0..200 {
        s.query("SELECT COUNT(*) FROM kv AS OF TXN 2500").unwrap();
    }
    report(&format!("[{label}] time-travel scan (AS OF)"), 200, t);

    let t = Instant::now();
    db.checkpoint().unwrap();
    report(&format!("[{label}] checkpoint (55k versions)"), 1, t);
}

fn main() {
    let path: PathBuf = std::env::temp_dir().join(format!("tessera-bench-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(tesseradb::storage::Files::wal_path(&path));
    println!("TesseraDB bench — {}\n", path.display());
    run(&Db::open(&path).unwrap(), "disk");
    println!();
    run(&Db::open_in_memory(), "memory");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(tesseradb::storage::Files::wal_path(&path));
}
