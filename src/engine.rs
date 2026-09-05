//! Execution engine: catalog, MVCC row versions, indexes, transactions,
//! streams, WAL replay and checkpoints.
//!
//! Concurrency model: any number of sessions read concurrently under a
//! `RwLock` read guard at a snapshot (`last_txn`); writers buffer their
//! changes in the session and validate + apply them under the write guard
//! at COMMIT (optimistic concurrency — a conflicting concurrent write
//! fails the commit instead of blocking).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex, RwLock},
};

use crate::{
    Reader, Value,
    parser::{BinOp, ColType, ColumnDef, Expr, SelectItem, Stmt, parse},
    put_str, put_value,
    storage::Files,
};

const CHECKPOINT_WAL_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Version {
    pub row_id: u64,
    pub xmin: u64,
    pub xmax: u64,
    pub vals: Vec<Value>,
}

impl Version {
    fn visible(&self, snapshot: u64) -> bool {
        self.xmin <= snapshot && (self.xmax == 0 || self.xmax > snapshot)
    }
}

pub struct Table {
    pub id: u32,
    pub name: String,
    pub cols: Vec<ColumnDef>,
    pub versions: Vec<Version>,
    pub next_row_id: u64,
    /// column → key → version indices (all versions, filtered by visibility).
    pub indexes: BTreeMap<String, BTreeMap<Value, Vec<usize>>>,
    by_row: HashMap<u64, Vec<usize>>,
}

impl Table {
    fn col(&self, name: &str) -> Option<usize> {
        self.cols
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }
    fn pk(&self) -> Option<usize> {
        self.cols.iter().position(|c| c.primary_key)
    }
    fn add_version(&mut self, v: Version) {
        let idx = self.versions.len();
        self.by_row.entry(v.row_id).or_default().push(idx);
        for (col, index) in &mut self.indexes {
            let c = self.cols.iter().position(|d| &d.name == col).unwrap();
            index.entry(v.vals[c].clone()).or_default().push(idx);
        }
        self.versions.push(v);
        self.next_row_id = self
            .next_row_id
            .max(self.versions.last().unwrap().row_id + 1);
    }
    /// Marks the currently-live version of `row_id` as ended by `txn`.
    fn end_row(&mut self, row_id: u64, txn: u64) -> bool {
        if let Some(idxs) = self.by_row.get(&row_id) {
            for &i in idxs {
                if self.versions[i].xmax == 0 {
                    self.versions[i].xmax = txn;
                    return true;
                }
            }
        }
        false
    }
    fn build_index(&mut self, col: &str) {
        let c = self.col(col).unwrap();
        let mut index: BTreeMap<Value, Vec<usize>> = BTreeMap::new();
        for (i, v) in self.versions.iter().enumerate() {
            index.entry(v.vals[c].clone()).or_default().push(i);
        }
        self.indexes.insert(self.cols[c].name.clone(), index);
    }
}

#[derive(Debug, Clone)]
pub struct Event {
    pub seq: u64,
    pub txn: u64,
    pub value: Value,
}

/// A committed change, as written to the WAL.
#[derive(Debug, Clone)]
enum Op {
    CreateTable(String, Vec<ColumnDef>),
    CreateIndex(String, String),
    Insert(String, Vec<Value>),
    Delete(String, u64),
    Append(String, Value),
}

struct Inner {
    tables: BTreeMap<String, Table>,
    streams: BTreeMap<String, Vec<Event>>,
    last_txn: u64,
    next_table_id: u32,
    files: Option<Files>,
}

/// An open database. Cheap to share (`Arc`) across threads.
pub struct Db {
    inner: RwLock<Inner>,
    /// Serialises commits so txn ids are assigned in commit order.
    commit_lock: Mutex<()>,
}

/// Result of one statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Rows {
        cols: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
    Affected(usize),
    Ok(String),
}

impl Output {
    pub fn rows(self) -> Vec<Vec<Value>> {
        match self {
            Output::Rows { rows, .. } => rows,
            _ => Vec::new(),
        }
    }
}

/// Buffered, not-yet-committed writes of one session.
#[derive(Default)]
struct Pending {
    ops: Vec<Op>,
    /// Rows inserted in this txn, keyed by a temporary negative id.
    inserted: Vec<(String, i64, Vec<Value>)>,
    deleted: HashSet<(String, u64)>,
    next_tmp: i64,
    /// Tables/indexes created in this txn (visible to it before commit).
    new_tables: BTreeMap<String, Vec<ColumnDef>>,
    /// Primary keys staged for insert in this txn: (table, key).
    staged_pk: std::collections::BTreeSet<(String, Value)>,
}

pub struct Session {
    db: Arc<Db>,
    txn: Option<Pending>,
}

fn table_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

impl Db {
    pub fn open_in_memory() -> Arc<Db> {
        Arc::new(Db {
            inner: RwLock::new(Inner {
                tables: BTreeMap::new(),
                streams: BTreeMap::new(),
                last_txn: 0,
                next_table_id: 1,
                files: None,
            }),
            commit_lock: Mutex::new(()),
        })
    }

    /// Opens (or creates) a database file, running recovery from the WAL.
    pub fn open(path: &Path) -> Result<Arc<Db>, String> {
        let mut files = Files::open(path).map_err(|e| e.to_string())?;
        let (header, records) = files.load_pages()?;
        let db = Db::open_in_memory();
        {
            let mut inner = db.inner.write().unwrap();
            inner.last_txn = header.last_txn;
            for rec in &records {
                inner.load_record(rec)?;
            }
            let frames = files.read_wal().map_err(|e| e.to_string())?;
            for frame in frames {
                let (txn, ops) = decode_frame(&frame)?;
                if txn <= inner.last_txn {
                    continue; // already in the page file
                }
                inner.apply(txn, &ops)?;
                inner.last_txn = txn;
            }
            inner.files = Some(files);
        }
        Ok(db)
    }

    pub fn session(self: &Arc<Self>) -> Session {
        Session {
            db: Arc::clone(self),
            txn: None,
        }
    }

    /// Id of the newest committed transaction (the current snapshot).
    pub fn last_txn(&self) -> u64 {
        self.inner.read().unwrap().last_txn
    }

    /// Forces a checkpoint: page file rewritten, WAL truncated.
    pub fn checkpoint(&self) -> Result<(), String> {
        let mut inner = self.inner.write().unwrap();
        inner.checkpoint()
    }

    pub fn table_names(&self) -> Vec<String> {
        self.inner.read().unwrap().tables.keys().cloned().collect()
    }
}

impl Inner {
    fn load_record(&mut self, rec: &[u8]) -> Result<(), String> {
        let mut r = Reader::new(rec);
        match r.u8()? {
            1 => {
                let id = r.u32()?;
                let name = r.str()?;
                let next_row_id = r.u64()?;
                let n = r.u16()?;
                let mut cols = Vec::new();
                for _ in 0..n {
                    let cname = r.str()?;
                    let ty = match r.u8()? {
                        0 => ColType::Int,
                        1 => ColType::Float,
                        2 => ColType::Text,
                        3 => ColType::Bool,
                        t => return Err(format!("bad column type {t}")),
                    };
                    let flags = r.u8()?;
                    cols.push(ColumnDef {
                        name: cname,
                        ty,
                        primary_key: flags & 1 != 0,
                        not_null: flags & 2 != 0,
                    });
                }
                let mut t = new_table(id, name.clone(), cols);
                t.next_row_id = next_row_id;
                self.next_table_id = self.next_table_id.max(id + 1);
                self.tables.insert(table_key(&name), t);
            }
            2 => {
                let id = r.u32()?;
                let row_id = r.u64()?;
                let xmin = r.u64()?;
                let xmax = r.u64()?;
                let n = r.u16()?;
                let mut vals = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    vals.push(r.value()?);
                }
                let t = self
                    .tables
                    .values_mut()
                    .find(|t| t.id == id)
                    .ok_or("row for unknown table")?;
                t.add_version(Version {
                    row_id,
                    xmin,
                    xmax,
                    vals,
                });
            }
            3 => {
                let name = r.str()?;
                let seq = r.u64()?;
                let txn = r.u64()?;
                let value = r.value()?;
                self.streams
                    .entry(name)
                    .or_default()
                    .push(Event { seq, txn, value });
            }
            4 => {
                let id = r.u32()?;
                let col = r.str()?;
                let t = self
                    .tables
                    .values_mut()
                    .find(|t| t.id == id)
                    .ok_or("index for unknown table")?;
                t.build_index(&col);
            }
            k => return Err(format!("unknown record kind {k}")),
        }
        Ok(())
    }

    fn records(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for t in self.tables.values() {
            let mut b = vec![1u8];
            b.extend_from_slice(&t.id.to_le_bytes());
            put_str(&mut b, &t.name);
            b.extend_from_slice(&t.next_row_id.to_le_bytes());
            b.extend_from_slice(&(t.cols.len() as u16).to_le_bytes());
            for c in &t.cols {
                put_str(&mut b, &c.name);
                b.push(match c.ty {
                    ColType::Int => 0,
                    ColType::Float => 1,
                    ColType::Text => 2,
                    ColType::Bool => 3,
                });
                b.push(u8::from(c.primary_key) | (u8::from(c.not_null) << 1));
            }
            out.push(b);
            for col in t.indexes.keys() {
                if t.pk().is_some_and(|p| &t.cols[p].name == col) {
                    continue; // the PK index is implicit
                }
                let mut b = vec![4u8];
                b.extend_from_slice(&t.id.to_le_bytes());
                put_str(&mut b, col);
                out.push(b);
            }
        }
        for t in self.tables.values() {
            for v in &t.versions {
                let mut b = vec![2u8];
                b.extend_from_slice(&t.id.to_le_bytes());
                b.extend_from_slice(&v.row_id.to_le_bytes());
                b.extend_from_slice(&v.xmin.to_le_bytes());
                b.extend_from_slice(&v.xmax.to_le_bytes());
                b.extend_from_slice(&(v.vals.len() as u16).to_le_bytes());
                for x in &v.vals {
                    put_value(&mut b, x);
                }
                out.push(b);
            }
        }
        for (name, events) in &self.streams {
            for e in events {
                let mut b = vec![3u8];
                put_str(&mut b, name);
                b.extend_from_slice(&e.seq.to_le_bytes());
                b.extend_from_slice(&e.txn.to_le_bytes());
                put_value(&mut b, &e.value);
                out.push(b);
            }
        }
        out
    }

    fn checkpoint(&mut self) -> Result<(), String> {
        let records = self.records();
        let last = self.last_txn;
        if let Some(f) = self.files.as_mut() {
            f.checkpoint(&records, last).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Applies committed ops to memory (used by commit and WAL replay).
    fn apply(&mut self, txn: u64, ops: &[Op]) -> Result<(), String> {
        for op in ops {
            match op {
                Op::CreateTable(name, cols) => {
                    let id = self.next_table_id;
                    self.next_table_id += 1;
                    self.tables
                        .insert(table_key(name), new_table(id, name.clone(), cols.clone()));
                }
                Op::CreateIndex(table, col) => {
                    self.tables
                        .get_mut(&table_key(table))
                        .ok_or("no such table")?
                        .build_index(col);
                }
                Op::Insert(table, vals) => {
                    let t = self
                        .tables
                        .get_mut(&table_key(table))
                        .ok_or("no such table")?;
                    let row_id = t.next_row_id;
                    t.add_version(Version {
                        row_id,
                        xmin: txn,
                        xmax: 0,
                        vals: vals.clone(),
                    });
                }
                Op::Delete(table, row_id) => {
                    let t = self
                        .tables
                        .get_mut(&table_key(table))
                        .ok_or("no such table")?;
                    t.end_row(*row_id, txn);
                }
                Op::Append(stream, value) => {
                    let s = self.streams.entry(stream.clone()).or_default();
                    let seq = s.len() as u64 + 1;
                    s.push(Event {
                        seq,
                        txn,
                        value: value.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn new_table(id: u32, name: String, cols: Vec<ColumnDef>) -> Table {
    let mut t = Table {
        id,
        name,
        cols,
        versions: Vec::new(),
        next_row_id: 1,
        indexes: BTreeMap::new(),
        by_row: HashMap::new(),
    };
    if let Some(pk) = t.pk() {
        let name = t.cols[pk].name.clone();
        t.build_index(&name);
    }
    t
}

fn encode_frame(txn: u64, ops: &[Op]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&txn.to_le_bytes());
    b.extend_from_slice(&(ops.len() as u32).to_le_bytes());
    for op in ops {
        match op {
            Op::CreateTable(name, cols) => {
                b.push(1);
                put_str(&mut b, name);
                b.extend_from_slice(&(cols.len() as u16).to_le_bytes());
                for c in cols {
                    put_str(&mut b, &c.name);
                    b.push(match c.ty {
                        ColType::Int => 0,
                        ColType::Float => 1,
                        ColType::Text => 2,
                        ColType::Bool => 3,
                    });
                    b.push(u8::from(c.primary_key) | (u8::from(c.not_null) << 1));
                }
            }
            Op::CreateIndex(t, c) => {
                b.push(2);
                put_str(&mut b, t);
                put_str(&mut b, c);
            }
            Op::Insert(t, vals) => {
                b.push(3);
                put_str(&mut b, t);
                b.extend_from_slice(&(vals.len() as u16).to_le_bytes());
                for v in vals {
                    put_value(&mut b, v);
                }
            }
            Op::Delete(t, row) => {
                b.push(4);
                put_str(&mut b, t);
                b.extend_from_slice(&row.to_le_bytes());
            }
            Op::Append(s, v) => {
                b.push(5);
                put_str(&mut b, s);
                put_value(&mut b, v);
            }
        }
    }
    b
}

fn decode_frame(b: &[u8]) -> Result<(u64, Vec<Op>), String> {
    let mut r = Reader::new(b);
    let txn = r.u64()?;
    let n = r.u32()?;
    let mut ops = Vec::new();
    for _ in 0..n {
        ops.push(match r.u8()? {
            1 => {
                let name = r.str()?;
                let nc = r.u16()?;
                let mut cols = Vec::new();
                for _ in 0..nc {
                    let cname = r.str()?;
                    let ty = match r.u8()? {
                        0 => ColType::Int,
                        1 => ColType::Float,
                        2 => ColType::Text,
                        3 => ColType::Bool,
                        t => return Err(format!("bad column type {t}")),
                    };
                    let flags = r.u8()?;
                    cols.push(ColumnDef {
                        name: cname,
                        ty,
                        primary_key: flags & 1 != 0,
                        not_null: flags & 2 != 0,
                    });
                }
                Op::CreateTable(name, cols)
            }
            2 => Op::CreateIndex(r.str()?, r.str()?),
            3 => {
                let t = r.str()?;
                let n = r.u16()?;
                let mut vals = Vec::new();
                for _ in 0..n {
                    vals.push(r.value()?);
                }
                Op::Insert(t, vals)
            }
            4 => Op::Delete(r.str()?, r.u64()?),
            5 => Op::Append(r.str()?, r.value()?),
            k => return Err(format!("bad op {k}")),
        });
    }
    Ok((txn, ops))
}

// ---------------------------------------------------------------- eval --

fn eval(e: &Expr, cols: &[ColumnDef], row: &[Value]) -> Result<Value, String> {
    Ok(match e {
        Expr::Lit(v) => v.clone(),
        Expr::Col(name) => {
            let i = cols
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("no such column: {name}"))?;
            row[i].clone()
        }
        Expr::Not(x) => match eval(x, cols, row)? {
            Value::Bool(b) => Value::Bool(!b),
            Value::Null => Value::Null,
            v => return Err(format!("NOT of non-boolean {v}")),
        },
        Expr::Bin(l, op, r) => {
            let a = eval(l, cols, row)?;
            match op {
                BinOp::And => {
                    if a == Value::Bool(false) {
                        return Ok(Value::Bool(false));
                    }
                    let b = eval(r, cols, row)?;
                    return Ok(match (a, b) {
                        (Value::Bool(x), Value::Bool(y)) => Value::Bool(x && y),
                        (Value::Bool(false), _) | (_, Value::Bool(false)) => Value::Bool(false),
                        _ => Value::Null,
                    });
                }
                BinOp::Or => {
                    if a == Value::Bool(true) {
                        return Ok(Value::Bool(true));
                    }
                    let b = eval(r, cols, row)?;
                    return Ok(match (a, b) {
                        (Value::Bool(x), Value::Bool(y)) => Value::Bool(x || y),
                        (Value::Bool(true), _) | (_, Value::Bool(true)) => Value::Bool(true),
                        _ => Value::Null,
                    });
                }
                _ => {}
            }
            let b = eval(r, cols, row)?;
            if a == Value::Null || b == Value::Null {
                return Ok(Value::Null);
            }
            match op {
                BinOp::Eq => Value::Bool(a == b),
                BinOp::Ne => Value::Bool(a != b),
                BinOp::Lt => Value::Bool(a < b),
                BinOp::Le => Value::Bool(a <= b),
                BinOp::Gt => Value::Bool(a > b),
                BinOp::Ge => Value::Bool(a >= b),
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => arith(*op, &a, &b)?,
                BinOp::And | BinOp::Or => unreachable!(),
            }
        }
    })
}

fn arith(op: BinOp, a: &Value, b: &Value) -> Result<Value, String> {
    if let (Value::Int(x), Value::Int(y)) = (a, b) {
        return Ok(Value::Int(match op {
            BinOp::Add => x.checked_add(*y).ok_or("integer overflow")?,
            BinOp::Sub => x.checked_sub(*y).ok_or("integer overflow")?,
            BinOp::Mul => x.checked_mul(*y).ok_or("integer overflow")?,
            BinOp::Div => x.checked_div(*y).ok_or("division by zero")?,
            _ => unreachable!(),
        }));
    }
    if let (Value::Text(x), Value::Text(y), BinOp::Add) = (a, b, op) {
        return Ok(Value::Text(format!("{x}{y}")));
    }
    let (x, y) = (
        a.as_f64()
            .ok_or_else(|| format!("cannot apply arithmetic to {a}"))?,
        b.as_f64()
            .ok_or_else(|| format!("cannot apply arithmetic to {b}"))?,
    );
    Ok(Value::Float(match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => x / y,
        _ => unreachable!(),
    }))
}

fn truthy(v: &Value) -> bool {
    matches!(v, Value::Bool(true))
}

/// `col = literal` conjunct usable for an index probe, if any.
fn index_probe<'a>(e: &'a Expr, t: &Table) -> Option<(&'a str, &'a Value)> {
    match e {
        Expr::Bin(l, BinOp::Eq, r) => match (&**l, &**r) {
            (Expr::Col(c), Expr::Lit(v)) | (Expr::Lit(v), Expr::Col(c))
                if t.indexes.keys().any(|k| k.eq_ignore_ascii_case(c)) =>
            {
                Some((c.as_str(), v))
            }
            _ => None,
        },
        Expr::Bin(l, BinOp::And, r) => index_probe(l, t).or_else(|| index_probe(r, t)),
        _ => None,
    }
}

fn coerce(v: Value, c: &ColumnDef) -> Result<Value, String> {
    Ok(match (v, &c.ty) {
        (Value::Null, _) => {
            if c.not_null {
                return Err(format!("column {} is NOT NULL", c.name));
            }
            Value::Null
        }
        (Value::Int(i), ColType::Float) => Value::Float(i as f64),
        (v @ Value::Int(_), ColType::Int)
        | (v @ Value::Float(_), ColType::Float)
        | (v @ Value::Text(_), ColType::Text)
        | (v @ Value::Bool(_), ColType::Bool) => v,
        (v, ty) => {
            return Err(format!(
                "type mismatch: {v} is not {ty:?} (column {})",
                c.name
            ));
        }
    })
}

// ------------------------------------------------------------- session --

impl Session {
    pub fn in_transaction(&self) -> bool {
        self.txn.is_some()
    }

    /// Runs one or more statements; returns the output of the last one.
    pub fn execute(&mut self, sql: &str) -> Result<Output, String> {
        let stmts = parse(sql)?;
        let mut last = Output::Ok("empty".into());
        for s in stmts {
            last = self.run(s)?;
        }
        Ok(last)
    }

    /// Runs a query and returns just the rows.
    pub fn query(&mut self, sql: &str) -> Result<Vec<Vec<Value>>, String> {
        Ok(self.execute(sql)?.rows())
    }

    fn run(&mut self, stmt: Stmt) -> Result<Output, String> {
        match stmt {
            Stmt::Begin => {
                if self.txn.is_some() {
                    return Err("already in a transaction".into());
                }
                self.txn = Some(Pending::default());
                Ok(Output::Ok("BEGIN".into()))
            }
            Stmt::Commit => {
                let p = self.txn.take().ok_or("no transaction in progress")?;
                let txn = self.commit(p)?;
                Ok(Output::Ok(format!("COMMIT txn {txn}")))
            }
            Stmt::Rollback => {
                self.txn.take().ok_or("no transaction in progress")?;
                Ok(Output::Ok("ROLLBACK".into()))
            }
            Stmt::Select { .. } | Stmt::Read { .. } | Stmt::Show(_) | Stmt::History { .. } => {
                let inner = self.db.inner.read().unwrap();
                self.read(&inner, &stmt)
            }
            write => {
                let auto = self.txn.is_none();
                if auto {
                    self.txn = Some(Pending::default());
                }
                let out = {
                    let inner = self.db.inner.read().unwrap();
                    let p = self.txn.as_mut().unwrap();
                    stage(&inner, p, write)
                };
                match out {
                    Ok(out) => {
                        if auto {
                            let p = self.txn.take().unwrap();
                            self.commit(p)?;
                        }
                        Ok(out)
                    }
                    Err(e) => {
                        if auto {
                            self.txn = None;
                        }
                        Err(e)
                    }
                }
            }
        }
    }

    fn commit(&mut self, p: Pending) -> Result<u64, String> {
        if p.ops.is_empty() {
            return Ok(self.db.last_txn());
        }
        let _serial = self.db.commit_lock.lock().unwrap();
        let mut inner = self.db.inner.write().unwrap();
        let txn = inner.last_txn + 1;
        // Validate against the newest state (optimistic concurrency).
        let mut seen_pk: HashMap<String, std::collections::BTreeSet<Value>> = HashMap::new();
        for op in &p.ops {
            match op {
                Op::CreateTable(name, _) => {
                    if inner.tables.contains_key(&table_key(name)) {
                        return Err(format!("table {name} already exists"));
                    }
                }
                Op::Insert(table, vals) => {
                    let t = match inner.tables.get(&table_key(table)) {
                        Some(t) => t,
                        None => continue, // created in this txn
                    };
                    if let Some(pk) = t.pk() {
                        let key = &vals[pk];
                        let dup = t.indexes[&t.cols[pk].name]
                            .get(key)
                            .is_some_and(|ix| ix.iter().any(|&i| t.versions[i].xmax == 0))
                            && !p.deleted.iter().any(|(tb, rid)| {
                                tb == table
                                    && t.by_row[rid].iter().any(|&i| {
                                        t.versions[i].xmax == 0 && &t.versions[i].vals[pk] == key
                                    })
                            });
                        if dup
                            || !seen_pk
                                .entry(table.clone())
                                .or_default()
                                .insert(key.clone())
                        {
                            return Err(format!("duplicate primary key {key} in {table}"));
                        }
                    }
                }
                Op::Delete(table, row_id) => {
                    let t = inner.tables.get(&table_key(table)).ok_or("no such table")?;
                    let live = t
                        .by_row
                        .get(row_id)
                        .is_some_and(|ix| ix.iter().any(|&i| t.versions[i].xmax == 0));
                    if !live {
                        return Err(format!(
                            "write conflict: row {row_id} of {table} was changed by a concurrent transaction"
                        ));
                    }
                }
                _ => {}
            }
        }
        let frame = encode_frame(txn, &p.ops);
        if let Some(f) = inner.files.as_mut() {
            f.append_wal(&frame)
                .map_err(|e| format!("wal write failed: {e}"))?;
        }
        inner.apply(txn, &p.ops)?;
        inner.last_txn = txn;
        if inner
            .files
            .as_ref()
            .is_some_and(|f| f.wal_len() > CHECKPOINT_WAL_BYTES)
        {
            inner.checkpoint()?;
        }
        Ok(txn)
    }

    fn read(&self, inner: &Inner, stmt: &Stmt) -> Result<Output, String> {
        match stmt {
            Stmt::Show(what) => Ok(match what.as_str() {
                "TABLES" => Output::Rows {
                    cols: vec!["table".into(), "columns".into(), "versions".into()],
                    rows: inner
                        .tables
                        .values()
                        .map(|t| {
                            vec![
                                Value::Text(t.name.clone()),
                                Value::Int(t.cols.len() as i64),
                                Value::Int(t.versions.len() as i64),
                            ]
                        })
                        .collect(),
                },
                "STREAMS" => Output::Rows {
                    cols: vec!["stream".into(), "events".into()],
                    rows: inner
                        .streams
                        .iter()
                        .map(|(n, e)| vec![Value::Text(n.clone()), Value::Int(e.len() as i64)])
                        .collect(),
                },
                "TXN" => Output::Rows {
                    cols: vec!["last_txn".into()],
                    rows: vec![vec![Value::Int(inner.last_txn as i64)]],
                },
                w => return Err(format!("SHOW {w}: expected TABLES, STREAMS or TXN")),
            }),
            Stmt::Read {
                stream,
                since,
                limit,
            } => {
                let events = inner.streams.get(stream).map(Vec::as_slice).unwrap_or(&[]);
                let rows = events
                    .iter()
                    .filter(|e| e.seq > *since)
                    .take(limit.unwrap_or(usize::MAX))
                    .map(|e| {
                        vec![
                            Value::Int(e.seq as i64),
                            Value::Int(e.txn as i64),
                            e.value.clone(),
                        ]
                    })
                    .collect();
                Ok(Output::Rows {
                    cols: vec!["seq".into(), "txn".into(), "value".into()],
                    rows,
                })
            }
            Stmt::History { table, filter } => {
                let t = inner
                    .tables
                    .get(&table_key(table))
                    .ok_or_else(|| format!("no such table: {table}"))?;
                let mut cols = vec!["_row".to_string(), "_xmin".into(), "_xmax".into()];
                cols.extend(t.cols.iter().map(|c| c.name.clone()));
                let mut rows = Vec::new();
                for v in &t.versions {
                    if let Some(f) = filter
                        && !truthy(&eval(f, &t.cols, &v.vals)?)
                    {
                        continue;
                    }
                    let mut row = vec![
                        Value::Int(v.row_id as i64),
                        Value::Int(v.xmin as i64),
                        Value::Int(v.xmax as i64),
                    ];
                    row.extend(v.vals.iter().cloned());
                    rows.push(row);
                }
                Ok(Output::Rows { cols, rows })
            }
            Stmt::Select {
                items,
                table,
                filter,
                order,
                limit,
                as_of,
            } => {
                let snapshot = as_of.unwrap_or(inner.last_txn);
                if snapshot > inner.last_txn {
                    return Err(format!(
                        "AS OF TXN {snapshot} is in the future (last txn is {})",
                        inner.last_txn
                    ));
                }
                let (cols, mut rows) = self.scan(inner, table, filter.as_ref(), snapshot)?;
                if let Some((col, desc)) = order {
                    let i = cols
                        .iter()
                        .position(|c| c.name.eq_ignore_ascii_case(col))
                        .ok_or_else(|| format!("no such column: {col}"))?;
                    rows.sort_by(|a, b| a[i].cmp(&b[i]));
                    if *desc {
                        rows.reverse();
                    }
                }
                if let Some(n) = limit {
                    rows.truncate(*n);
                }
                if items == &[SelectItem::CountStar] {
                    return Ok(Output::Rows {
                        cols: vec!["count".into()],
                        rows: vec![vec![Value::Int(rows.len() as i64)]],
                    });
                }
                let mut out_cols = Vec::new();
                let mut picks: Vec<usize> = Vec::new();
                for it in items {
                    match it {
                        SelectItem::Star => {
                            for (i, c) in cols.iter().enumerate() {
                                out_cols.push(c.name.clone());
                                picks.push(i);
                            }
                        }
                        SelectItem::Col(name) => {
                            let i = cols
                                .iter()
                                .position(|c| c.name.eq_ignore_ascii_case(name))
                                .ok_or_else(|| format!("no such column: {name}"))?;
                            out_cols.push(cols[i].name.clone());
                            picks.push(i);
                        }
                        SelectItem::CountStar => {
                            return Err("COUNT(*) cannot be mixed with columns".into());
                        }
                    }
                }
                let rows = rows
                    .into_iter()
                    .map(|r| picks.iter().map(|&i| r[i].clone()).collect())
                    .collect();
                Ok(Output::Rows {
                    cols: out_cols,
                    rows,
                })
            }
            _ => unreachable!(),
        }
    }

    /// Visible rows of `table` at `snapshot`, with this session's pending
    /// writes overlaid. Returns (columns, rows).
    fn scan(
        &self,
        inner: &Inner,
        table: &str,
        filter: Option<&Expr>,
        snapshot: u64,
    ) -> Result<(Vec<ColumnDef>, Vec<Vec<Value>>), String> {
        let pending = self.txn.as_ref();
        let key = table_key(table);
        let mut rows = Vec::new();
        let cols = if let Some(t) = inner.tables.get(&key) {
            let candidates: Vec<usize> = match filter.and_then(|f| index_probe(f, t)) {
                Some((col, val)) => {
                    let name = t
                        .indexes
                        .keys()
                        .find(|k| k.eq_ignore_ascii_case(col))
                        .unwrap();
                    t.indexes[name].get(val).cloned().unwrap_or_default()
                }
                None => (0..t.versions.len()).collect(),
            };
            for i in candidates {
                let v = &t.versions[i];
                if !v.visible(snapshot) {
                    continue;
                }
                if pending.is_some_and(|p| p.deleted.contains(&(key.clone(), v.row_id))) {
                    continue;
                }
                if let Some(f) = filter
                    && !truthy(&eval(f, &t.cols, &v.vals)?)
                {
                    continue;
                }
                rows.push(v.vals.clone());
            }
            t.cols.clone()
        } else if let Some(cols) = pending.and_then(|p| p.new_tables.get(&key)) {
            cols.clone()
        } else {
            return Err(format!("no such table: {table}"));
        };
        if let Some(p) = pending {
            for (tb, _, vals) in &p.inserted {
                if tb == &key {
                    if let Some(f) = filter
                        && !truthy(&eval(f, &cols, vals)?)
                    {
                        continue;
                    }
                    rows.push(vals.clone());
                }
            }
        }
        Ok((cols, rows))
    }
}

/// Stages a write statement into the session's pending transaction.
fn stage(inner: &Inner, p: &mut Pending, stmt: Stmt) -> Result<Output, String> {
    let cols_of = |p: &Pending, table: &str| -> Result<Vec<ColumnDef>, String> {
        let key = table_key(table);
        inner
            .tables
            .get(&key)
            .map(|t| t.cols.clone())
            .or_else(|| p.new_tables.get(&key).cloned())
            .ok_or_else(|| format!("no such table: {table}"))
    };
    match stmt {
        Stmt::CreateTable { name, cols } => {
            if inner.tables.contains_key(&table_key(&name))
                || p.new_tables.contains_key(&table_key(&name))
            {
                return Err(format!("table {name} already exists"));
            }
            if cols.is_empty() {
                return Err("a table needs at least one column".into());
            }
            p.new_tables.insert(table_key(&name), cols.clone());
            p.ops.push(Op::CreateTable(name, cols));
            Ok(Output::Ok("CREATE TABLE".into()))
        }
        Stmt::CreateIndex { table, col } => {
            let cols = cols_of(p, &table)?;
            let c = cols
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(&col))
                .ok_or_else(|| format!("no such column: {col}"))?;
            p.ops.push(Op::CreateIndex(table, c.name.clone()));
            Ok(Output::Ok("CREATE INDEX".into()))
        }
        Stmt::Insert { table, cols, rows } => {
            let defs = cols_of(p, &table)?;
            let positions: Vec<usize> = match &cols {
                Some(names) => names
                    .iter()
                    .map(|n| {
                        defs.iter()
                            .position(|c| c.name.eq_ignore_ascii_case(n))
                            .ok_or_else(|| format!("no such column: {n}"))
                    })
                    .collect::<Result<_, _>>()?,
                None => (0..defs.len()).collect(),
            };
            let mut n = 0;
            for row in rows {
                if row.len() != positions.len() {
                    return Err(format!(
                        "expected {} values, got {}",
                        positions.len(),
                        row.len()
                    ));
                }
                let mut vals = vec![Value::Null; defs.len()];
                for (pos, e) in positions.iter().zip(row) {
                    vals[*pos] = eval(&e, &defs, &[])?;
                }
                for (v, d) in vals.iter_mut().zip(&defs) {
                    let taken = std::mem::replace(v, Value::Null);
                    *v = coerce(taken, d)?;
                }
                // Same-transaction duplicate PK check; cross-txn happens at commit.
                if let Some(pk) = defs.iter().position(|c| c.primary_key)
                    && !p.staged_pk.insert((table_key(&table), vals[pk].clone()))
                {
                    return Err(format!("duplicate primary key {} in {table}", vals[pk]));
                }
                p.next_tmp -= 1;
                p.inserted
                    .push((table_key(&table), p.next_tmp, vals.clone()));
                p.ops.push(Op::Insert(table.clone(), vals));
                n += 1;
            }
            Ok(Output::Affected(n))
        }
        Stmt::Delete { table, filter } => {
            let n = stage_delete(inner, p, &table, filter.as_ref(), None)?;
            Ok(Output::Affected(n))
        }
        Stmt::Update {
            table,
            sets,
            filter,
        } => {
            let n = stage_delete(inner, p, &table, filter.as_ref(), Some(&sets))?;
            Ok(Output::Affected(n))
        }
        Stmt::Append { stream, value } => {
            let v = eval(&value, &[], &[])?;
            p.ops.push(Op::Append(stream, v));
            Ok(Output::Affected(1))
        }
        _ => unreachable!(),
    }
}

/// DELETE, or UPDATE when `sets` is given (delete old version + insert new).
fn stage_delete(
    inner: &Inner,
    p: &mut Pending,
    table: &str,
    filter: Option<&Expr>,
    sets: Option<&[(String, Expr)]>,
) -> Result<usize, String> {
    let key = table_key(table);
    let t = inner.tables.get(&key);
    let cols = t
        .map(|t| t.cols.clone())
        .or_else(|| p.new_tables.get(&key).cloned())
        .ok_or_else(|| format!("no such table: {table}"))?;
    let snapshot = inner.last_txn;
    let mut victims: Vec<(Option<u64>, Option<i64>, Vec<Value>)> = Vec::new();
    if let Some(t) = t {
        let candidates: Vec<usize> = match filter.and_then(|f| index_probe(f, t)) {
            Some((col, val)) => {
                let name = t
                    .indexes
                    .keys()
                    .find(|k| k.eq_ignore_ascii_case(col))
                    .unwrap();
                t.indexes[name].get(val).cloned().unwrap_or_default()
            }
            None => (0..t.versions.len()).collect(),
        };
        for i in candidates {
            let v = &t.versions[i];
            if !v.visible(snapshot) || p.deleted.contains(&(key.clone(), v.row_id)) {
                continue;
            }
            if let Some(f) = filter
                && !truthy(&eval(f, &cols, &v.vals)?)
            {
                continue;
            }
            victims.push((Some(v.row_id), None, v.vals.clone()));
        }
    }
    for (tb, tmp, vals) in &p.inserted {
        if tb != &key {
            continue;
        }
        if let Some(f) = filter
            && !truthy(&eval(f, &cols, vals)?)
        {
            continue;
        }
        victims.push((None, Some(*tmp), vals.clone()));
    }
    let n = victims.len();
    for (row_id, tmp, old) in victims {
        // Remove the old version.
        match (row_id, tmp) {
            (Some(rid), _) => {
                p.deleted.insert((key.clone(), rid));
                p.ops.push(Op::Delete(table.to_string(), rid));
            }
            (None, Some(tmp)) => {
                p.inserted.retain(|(_, t, _)| *t != tmp);
                if let Some(pk) = cols.iter().position(|c| c.primary_key) {
                    p.staged_pk.remove(&(key.clone(), old[pk].clone()));
                }
                // Drop the matching staged insert op (last one with equal values).
                if let Some(pos) = p.ops.iter().rposition(
                    |o| matches!(o, Op::Insert(tb, v) if table_key(tb) == key && *v == old),
                ) {
                    p.ops.remove(pos);
                }
            }
            _ => unreachable!(),
        }
        if let Some(sets) = sets {
            let mut new = old.clone();
            for (col, e) in sets {
                let i = cols
                    .iter()
                    .position(|c| c.name.eq_ignore_ascii_case(col))
                    .ok_or_else(|| format!("no such column: {col}"))?;
                new[i] = coerce(eval(e, &cols, &old)?, &cols[i])?;
            }
            if let Some(pk) = cols.iter().position(|c| c.primary_key)
                && !p.staged_pk.insert((key.clone(), new[pk].clone()))
            {
                return Err(format!("duplicate primary key {} in {table}", new[pk]));
            }
            p.next_tmp -= 1;
            p.inserted.push((key.clone(), p.next_tmp, new.clone()));
            p.ops.push(Op::Insert(table.to_string(), new));
        }
    }
    Ok(n)
}
