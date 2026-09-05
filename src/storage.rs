//! On-disk format: a page file plus a write-ahead log.
//!
//! * `<db>`      — page 0 is the header; every other page is a slotted page
//!   of records (schemas, row versions, stream events, index defs). Records
//!   larger than a page spill into a chain of overflow pages.
//! * `<db>.wal`  — CRC32-framed logical records, one frame per committed
//!   transaction. Appended and fsynced on every commit.
//!
//! A checkpoint rewrites the whole page file to `<db>.tmp`, fsyncs, renames
//! it over `<db>`, then truncates the WAL. Recovery loads the page file and
//! replays every WAL frame whose txn id is newer than the header's
//! `last_txn`, stopping at the first torn/corrupt frame.
//!
//! ponytail: checkpoint rewrites every page; dirty-page tracking is the
//! upgrade path once page files outgrow a few hundred MiB.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use crate::Reader;

pub const PAGE_SIZE: usize = 4096;
const MAGIC: &[u8; 4] = b"TSRA";
const FORMAT: u32 = 1;
/// Per-page bookkeeping: u16 slot count + u16 free-space end.
const PAGE_HDR: usize = 4;
const SLOT: usize = 4;
const OVERFLOW_TAG: u8 = 0xFF;
const OVERFLOW_PAYLOAD: usize = PAGE_SIZE - 8;

/// CRC-32 (IEEE), table-driven; std has no checksum, and this is 15 lines.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *e = c;
        }
        t
    });
    !data.iter().fold(!0u32, |c, &b| {
        table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8)
    })
}

/// A slotted page: `[n_slots u16][free_end u16][slot: off u16, len u16]…`
/// with record bytes growing down from the end of the page.
pub struct Page {
    pub data: Vec<u8>,
}

impl Page {
    pub fn new() -> Self {
        let mut data = vec![0u8; PAGE_SIZE];
        data[2..4].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
        Page { data }
    }
    fn n_slots(&self) -> usize {
        u16::from_le_bytes([self.data[0], self.data[1]]) as usize
    }
    fn free_end(&self) -> usize {
        u16::from_le_bytes([self.data[2], self.data[3]]) as usize
    }
    pub fn capacity() -> usize {
        PAGE_SIZE - PAGE_HDR - SLOT
    }
    pub fn fits(&self, len: usize) -> bool {
        let used_front = PAGE_HDR + self.n_slots() * SLOT + SLOT;
        self.free_end() >= used_front && self.free_end() - used_front >= len
    }
    /// Appends a record; caller checks [`Page::fits`].
    pub fn push(&mut self, rec: &[u8]) {
        let n = self.n_slots();
        let end = self.free_end();
        let off = end - rec.len();
        self.data[off..end].copy_from_slice(rec);
        let slot = PAGE_HDR + n * SLOT;
        self.data[slot..slot + 2].copy_from_slice(&(off as u16).to_le_bytes());
        self.data[slot + 2..slot + 4].copy_from_slice(&(rec.len() as u16).to_le_bytes());
        self.data[0..2].copy_from_slice(&((n + 1) as u16).to_le_bytes());
        self.data[2..4].copy_from_slice(&(off as u16).to_le_bytes());
    }
    pub fn records(&self) -> Result<Vec<&[u8]>, String> {
        let mut out = Vec::new();
        for i in 0..self.n_slots() {
            let s = PAGE_HDR + i * SLOT;
            let off = u16::from_le_bytes([self.data[s], self.data[s + 1]]) as usize;
            let len = u16::from_le_bytes([self.data[s + 2], self.data[s + 3]]) as usize;
            out.push(self.data.get(off..off + len).ok_or("corrupt slot")?);
        }
        Ok(out)
    }
}

impl Default for Page {
    fn default() -> Self {
        Self::new()
    }
}

/// Serialises records into pages (the checkpoint writer).
pub struct PageWriter {
    pub pages: Vec<Page>,
    /// Index of the slotted page currently being filled (overflow pages
    /// are raw and never receive slots).
    cur: usize,
}

impl PageWriter {
    pub fn new() -> Self {
        PageWriter {
            pages: vec![Page::new()],
            cur: 0,
        }
    }
    pub fn push(&mut self, rec: &[u8]) {
        if rec.len() > Page::capacity() {
            // Overflow chain: a stub record [0xFF][first_page u64][total_len
            // u32] in a slotted page, followed by full raw pages. The stub
            // comes first so a sequential reader knows which pages to skip.
            self.ensure_room(13);
            let first = self.pages.len() as u64;
            let mut stub = vec![OVERFLOW_TAG];
            stub.extend_from_slice(&first.to_le_bytes());
            stub.extend_from_slice(&(rec.len() as u32).to_le_bytes());
            self.pages[self.cur].push(&stub);
            for chunk in rec.chunks(OVERFLOW_PAYLOAD) {
                let mut p = Page::new();
                p.data[..chunk.len()].copy_from_slice(chunk);
                self.pages.push(p);
            }
        } else {
            self.ensure_room(rec.len());
            self.pages[self.cur].push(rec);
        }
    }
    fn ensure_room(&mut self, len: usize) {
        if !self.pages[self.cur].fits(len) {
            self.pages.push(Page::new());
            self.cur = self.pages.len() - 1;
        }
    }
}

impl Default for PageWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Header page contents.
#[derive(Debug, Clone, Copy, Default)]
pub struct Header {
    pub n_pages: u64,
    pub last_txn: u64,
}

fn encode_header(h: Header) -> Vec<u8> {
    let mut b = vec![0u8; PAGE_SIZE];
    b[..4].copy_from_slice(MAGIC);
    b[4..8].copy_from_slice(&FORMAT.to_le_bytes());
    b[8..12].copy_from_slice(&(PAGE_SIZE as u32).to_le_bytes());
    b[12..20].copy_from_slice(&h.n_pages.to_le_bytes());
    b[20..28].copy_from_slice(&h.last_txn.to_le_bytes());
    let crc = crc32(&b[..28]);
    b[28..32].copy_from_slice(&crc.to_le_bytes());
    b
}

pub struct Files {
    pub db_path: PathBuf,
    pub wal: File,
}

impl Files {
    pub fn wal_path(db: &Path) -> PathBuf {
        let mut s = db.as_os_str().to_owned();
        s.push(".wal");
        PathBuf::from(s)
    }

    pub fn open(db: &Path) -> std::io::Result<Self> {
        // read+write (not append): Windows refuses `set_len` on an
        // append-only handle, so the checkpoint truncation would fail.
        let wal = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(Self::wal_path(db))?;
        Ok(Files {
            db_path: db.to_path_buf(),
            wal,
        })
    }

    /// Loads header + every logical record from the page file. A missing
    /// file is an empty database.
    pub fn load_pages(&self) -> Result<(Header, Vec<Vec<u8>>), String> {
        let mut f = match File::open(&self.db_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Header::default(), Vec::new()));
            }
            Err(e) => return Err(e.to_string()),
        };
        let mut all = Vec::new();
        f.read_to_end(&mut all).map_err(|e| e.to_string())?;
        if all.len() < PAGE_SIZE {
            return Err("page file too short".into());
        }
        if &all[..4] != MAGIC {
            return Err("not a Tessera database (bad magic)".into());
        }
        if u32::from_le_bytes(all[28..32].try_into().unwrap()) != crc32(&all[..28]) {
            return Err("header checksum mismatch".into());
        }
        let header = Header {
            n_pages: u64::from_le_bytes(all[12..20].try_into().unwrap()),
            last_txn: u64::from_le_bytes(all[20..28].try_into().unwrap()),
        };
        let (chunks, _) = all[PAGE_SIZE..].as_chunks::<PAGE_SIZE>();
        let pages: Vec<&[u8]> = chunks.iter().map(|c| c.as_slice()).collect();
        if pages.len() as u64 != header.n_pages {
            return Err(format!(
                "page count mismatch: header says {}, file has {}",
                header.n_pages,
                pages.len()
            ));
        }
        let mut records = Vec::new();
        let mut skip_until = 0usize;
        for (i, raw) in pages.iter().enumerate() {
            if i < skip_until {
                continue; // overflow data page, consumed via its stub
            }
            let page = Page { data: raw.to_vec() };
            for rec in page.records()? {
                if rec.first() == Some(&OVERFLOW_TAG) && rec.len() == 13 {
                    let first = u64::from_le_bytes(rec[1..9].try_into().unwrap()) as usize;
                    let total = u32::from_le_bytes(rec[9..13].try_into().unwrap()) as usize;
                    let n = total.div_ceil(OVERFLOW_PAYLOAD);
                    let mut buf = Vec::with_capacity(total);
                    for p in pages
                        .get(first..first + n)
                        .ok_or("overflow chain out of range")?
                    {
                        buf.extend_from_slice(&p[..OVERFLOW_PAYLOAD]);
                    }
                    buf.truncate(total);
                    records.push(buf);
                    skip_until = skip_until.max(first + n);
                } else {
                    records.push(rec.to_vec());
                }
            }
        }
        Ok((header, records))
    }

    /// Atomically replaces the page file with `records`, then truncates the
    /// WAL. Safe against a crash at any point: the rename is atomic and the
    /// txn filter makes WAL replay idempotent.
    pub fn checkpoint(&mut self, records: &[Vec<u8>], last_txn: u64) -> std::io::Result<()> {
        let mut w = PageWriter::new();
        for r in records {
            w.push(r);
        }
        let tmp = {
            let mut s = self.db_path.as_os_str().to_owned();
            s.push(".tmp");
            PathBuf::from(s)
        };
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&encode_header(Header {
                n_pages: w.pages.len() as u64,
                last_txn,
            }))?;
            for p in &w.pages {
                f.write_all(&p.data)?;
            }
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.db_path)?;
        if let Some(dir) = self.db_path.parent()
            && !dir.as_os_str().is_empty()
            && let Ok(d) = File::open(dir)
        {
            let _ = d.sync_all();
        }
        self.wal.set_len(0)?;
        self.wal.sync_all()?;
        Ok(())
    }

    /// Appends one frame and fsyncs. Frame = `[len u32][crc u32][payload]`.
    pub fn append_wal(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 8);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&crc32(payload).to_le_bytes());
        frame.extend_from_slice(payload);
        self.wal.seek(SeekFrom::End(0))?;
        self.wal.write_all(&frame)?;
        self.wal.sync_data()
    }

    /// Every intact frame payload in order; stops at the first torn or
    /// corrupt frame (a crash mid-append), which is discarded.
    pub fn read_wal(&mut self) -> std::io::Result<Vec<Vec<u8>>> {
        self.wal.seek(SeekFrom::Start(0))?;
        let mut all = Vec::new();
        self.wal.read_to_end(&mut all)?;
        let mut r = Reader::new(&all);
        let mut out = Vec::new();
        while !r.done() {
            let Ok(len) = r.u32() else { break };
            let Ok(crc) = r.u32() else { break };
            let Ok(payload) = r.bytes(len as usize) else {
                break;
            };
            if crc32(payload) != crc {
                break;
            }
            out.push(payload.to_vec());
        }
        Ok(out)
    }

    pub fn wal_len(&self) -> u64 {
        self.wal.metadata().map(|m| m.len()).unwrap_or(0)
    }
}
