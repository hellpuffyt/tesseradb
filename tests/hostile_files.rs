//! A corrupt or hostile database/WAL file must be rejected with an error —
//! never a panic, never an out-of-bounds read (SECURITY.md promise).
//!
//! Regression: before the 2026-10 audit each case below panicked inside
//! `Db::open` (slot table past the page end, index on an unknown column, row
//! with fewer values than columns, id/row-id/overflow-chain arithmetic
//! overflow).

use std::path::{Path, PathBuf};

use tesseradb::Db;
use tesseradb::storage::{Files, PAGE_SIZE, crc32};

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tessera-hostile-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("db")
}

fn s(b: &mut Vec<u8>, x: &str) {
    b.extend_from_slice(&(x.len() as u32).to_le_bytes());
    b.extend_from_slice(x.as_bytes());
}

/// Catalog record: table `t` (id) with one INT PRIMARY KEY column `id`.
fn table_rec(id: u32) -> Vec<u8> {
    let mut b = vec![1u8];
    b.extend_from_slice(&id.to_le_bytes());
    s(&mut b, "t");
    b.extend_from_slice(&1u64.to_le_bytes()); // next_row_id
    b.extend_from_slice(&1u16.to_le_bytes());
    s(&mut b, "id");
    b.extend_from_slice(&[0, 1]); // INT, primary key
    b
}

fn row_rec(table: u32, row_id: u64, vals: &[i64]) -> Vec<u8> {
    let mut b = vec![2u8];
    b.extend_from_slice(&table.to_le_bytes());
    b.extend_from_slice(&row_id.to_le_bytes());
    b.extend_from_slice(&1u64.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&(vals.len() as u16).to_le_bytes());
    for v in vals {
        b.push(2);
        b.extend_from_slice(&v.to_le_bytes());
    }
    b
}

fn write_pages(name: &str, records: &[Vec<u8>]) -> PathBuf {
    let path = tmp(name);
    let mut f = Files::open(&path).unwrap();
    f.checkpoint(records, 1).unwrap();
    path
}

fn open_fails(path: &Path) -> String {
    match Db::open(path) {
        Ok(_) => panic!("hostile file was accepted"),
        Err(e) => e,
    }
}

#[test]
fn row_with_too_few_values_is_rejected() {
    let p = write_pages("arity", &[table_rec(1), row_rec(1, 1, &[])]);
    open_fails(&p);
}

#[test]
fn index_on_unknown_column_is_rejected() {
    let mut idx = vec![4u8];
    idx.extend_from_slice(&1u32.to_le_bytes());
    s(&mut idx, "nope");
    let p = write_pages("index", &[table_rec(1), idx]);
    open_fails(&p);
}

#[test]
fn extreme_ids_are_rejected_not_overflowed() {
    open_fails(&write_pages("tableid", &[table_rec(u32::MAX)]));
    open_fails(&write_pages(
        "rowid",
        &[table_rec(1), row_rec(1, u64::MAX, &[7])],
    ));
}

#[test]
fn slot_table_past_the_page_end_is_rejected() {
    let p = write_pages("slots", &[table_rec(1)]);
    let mut bytes = std::fs::read(&p).unwrap();
    // First data page: blank it, then claim 65535 (empty) slots, so the
    // slot table itself runs past the end of the page.
    bytes[PAGE_SIZE..2 * PAGE_SIZE].fill(0);
    bytes[PAGE_SIZE..PAGE_SIZE + 2].copy_from_slice(&u16::MAX.to_le_bytes());
    std::fs::write(&p, bytes).unwrap();
    open_fails(&p);
}

#[test]
fn overflow_chain_pointing_past_the_file_is_rejected() {
    let mut stub = vec![0xFFu8];
    stub.extend_from_slice(&u64::MAX.to_le_bytes());
    stub.extend_from_slice(&u32::MAX.to_le_bytes());
    let p = write_pages("chain", &[stub]);
    open_fails(&p);
}

fn wal_with(name: &str, ops: &[Vec<u8>]) -> PathBuf {
    let path = tmp(name);
    let mut payload = 1u64.to_le_bytes().to_vec();
    payload.extend_from_slice(&(ops.len() as u32).to_le_bytes());
    for op in ops {
        payload.extend_from_slice(op);
    }
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&crc32(&payload).to_le_bytes());
    frame.extend_from_slice(&payload);
    let mut wal = path.as_os_str().to_owned();
    wal.push(".wal");
    std::fs::write(PathBuf::from(wal), frame).unwrap();
    path
}

fn create_table_op() -> Vec<u8> {
    let mut b = vec![1u8];
    s(&mut b, "t");
    b.extend_from_slice(&1u16.to_le_bytes());
    s(&mut b, "id");
    b.extend_from_slice(&[0, 1]);
    b
}

#[test]
fn wal_index_on_unknown_column_is_rejected() {
    let mut idx = vec![2u8];
    s(&mut idx, "t");
    s(&mut idx, "nope");
    open_fails(&wal_with("walindex", &[create_table_op(), idx]));
}

#[test]
fn wal_insert_with_wrong_arity_is_rejected() {
    let mut ins = vec![3u8];
    s(&mut ins, "t");
    ins.extend_from_slice(&0u16.to_le_bytes());
    open_fails(&wal_with("walarity", &[create_table_op(), ins]));
}

#[test]
fn a_valid_handmade_file_still_opens() {
    let p = write_pages("valid", &[table_rec(1), row_rec(1, 1, &[7])]);
    let db = Db::open(&p).unwrap();
    let rows = db.session().query("SELECT id FROM t").unwrap();
    assert_eq!(rows.len(), 1);
}
