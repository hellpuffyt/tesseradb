//! TesseraDB — an embedded, temporal, event-native database.
//!
//! Every row version is retained, so any query can be answered *as of* a
//! past transaction (`SELECT … AS OF TXN n`, `HISTORY t WHERE …`), and
//! append-only streams (`APPEND TO s …` / `READ s SINCE n`) live next to
//! tables in the same transactions and the same write-ahead log.
//!
//! ```
//! let db = tesseradb::Db::open_in_memory();
//! let mut s = db.session();
//! s.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)").unwrap();
//! s.execute("INSERT INTO t VALUES (1, 'a')").unwrap();
//! let txn = db.last_txn();
//! s.execute("UPDATE t SET v = 'b' WHERE id = 1").unwrap();
//! let now = s.query("SELECT v FROM t").unwrap();
//! let then = s.query(&format!("SELECT v FROM t AS OF TXN {txn}")).unwrap();
//! assert_eq!(now[0][0], tesseradb::Value::Text("b".into()));
//! assert_eq!(then[0][0], tesseradb::Value::Text("a".into()));
//! ```

pub mod engine;
pub mod lexer;
pub mod parser;
pub mod storage;

use std::{cmp::Ordering, fmt};

pub use engine::{Db, Output, Session};

/// A cell value. Ordering is total (needed for indexes and ORDER BY):
/// `Null < Bool < numbers < Text`; Int and Float compare numerically.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl Eq for Value {}

impl Value {
    fn rank(&self) -> u8 {
        match self {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::Int(_) | Value::Float(_) => 2,
            Value::Text(_) => 3,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }
}

impl Ord for Value {
    fn cmp(&self, o: &Self) -> Ordering {
        match (self, o) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Text(a), Value::Text(b)) => a.cmp(b),
            (a, b) if a.rank() == 2 && b.rank() == 2 => {
                a.as_f64().unwrap().total_cmp(&b.as_f64().unwrap())
            }
            (a, b) => a.rank().cmp(&b.rank()),
        }
    }
}
impl PartialOrd for Value {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Text(s) => write!(f, "{s}"),
        }
    }
}

/// Binary encoding shared by pages and the WAL: one tag byte then payload.
pub(crate) fn put_value(buf: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Null => buf.push(0),
        Value::Bool(b) => {
            buf.push(1);
            buf.push(u8::from(*b));
        }
        Value::Int(i) => {
            buf.push(2);
            buf.extend_from_slice(&i.to_le_bytes());
        }
        Value::Float(x) => {
            buf.push(3);
            buf.extend_from_slice(&x.to_le_bytes());
        }
        Value::Text(s) => {
            buf.push(4);
            put_str(buf, s);
        }
    }
}

pub(crate) fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

/// Cursor over an encoded byte slice. Every read is bounds-checked so a
/// corrupt page or torn WAL frame yields an error, never a panic.
pub(crate) struct Reader<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let s = self.buf.get(self.pos..end).ok_or("truncated record")?;
        self.pos = end;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }
    pub fn str(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        let b = self.bytes(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| "invalid utf-8".to_string())
    }
    pub fn value(&mut self) -> Result<Value, String> {
        Ok(match self.u8()? {
            0 => Value::Null,
            1 => Value::Bool(self.u8()? != 0),
            2 => Value::Int(i64::from_le_bytes(self.bytes(8)?.try_into().unwrap())),
            3 => Value::Float(f64::from_le_bytes(self.bytes(8)?.try_into().unwrap())),
            4 => Value::Text(self.str()?),
            t => return Err(format!("bad value tag {t}")),
        })
    }
    pub fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }
}
