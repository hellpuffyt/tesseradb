//! Deterministic grammar fuzzing: random statement soup must never panic,
//! and random byte soup must never make the parser or WAL decoder panic.
//! (A `cargo fuzz` harness needs nightly; this runs on stable in CI.)

use tesseradb::{Db, parser::parse};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[(self.next() % xs.len() as u64) as usize]
    }
}

const ATOMS: &[&str] = &[
    "SELECT", "INSERT", "INTO", "VALUES", "FROM", "WHERE", "AND", "OR", "NOT", "(", ")", ",", ";",
    "=", "<", ">=", "!=", "*", "+", "-", "/", "'s'", "''", "1", "0", "-5", "2.5", "NULL", "TRUE",
    "t", "id", "v", "CREATE", "TABLE", "INT", "TEXT", "PRIMARY", "KEY", "UPDATE", "SET", "DELETE",
    "BEGIN", "COMMIT", "ROLLBACK", "AS", "OF", "TXN", "ORDER", "BY", "LIMIT", "APPEND", "TO",
    "READ", "SINCE", "HISTORY", "SHOW", "TABLES", "COUNT", "INDEX", "ON", "DESC",
];

#[test]
fn random_token_soup_never_panics() {
    let db = Db::open_in_memory();
    let mut s = db.session();
    s.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)")
        .unwrap();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..20_000 {
        let n = 1 + (rng.next() % 12) as usize;
        let sql: Vec<&str> = (0..n).map(|_| rng.pick(ATOMS)).collect();
        let sql = sql.join(" ");
        let _ = s.execute(&sql); // Ok or Err, never panic
        if s.in_transaction() && rng.next().is_multiple_of(4) {
            let _ = s.execute("ROLLBACK");
        }
    }
}

#[test]
fn random_bytes_never_panic_the_parser() {
    let mut rng = Rng(42);
    for _ in 0..5_000 {
        let n = (rng.next() % 64) as usize;
        let bytes: Vec<u8> = (0..n).map(|_| (rng.next() % 256) as u8).collect();
        let text = String::from_utf8_lossy(&bytes);
        let _ = parse(&text);
    }
}

#[test]
fn random_wal_bytes_never_panic_recovery() {
    let mut rng = Rng(7);
    let dir = std::env::temp_dir().join(format!("tessera-fuzz-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..200 {
        let path = dir.join(format!("db{i}"));
        let n = (rng.next() % 512) as usize;
        let bytes: Vec<u8> = (0..n).map(|_| (rng.next() % 256) as u8).collect();
        std::fs::write(tesseradb::storage::Files::wal_path(&path), &bytes).unwrap();
        let _ = Db::open(&path); // corrupt frames are discarded or rejected, never a panic
    }
}
