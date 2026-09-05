//! Recursive-descent parser for Tessera's SQL dialect. See docs/SQL.md.

use crate::{
    Value,
    lexer::{Token, lex},
};

#[derive(Debug, Clone, PartialEq)]
pub enum ColType {
    Int,
    Float,
    Text,
    Bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ColType,
    pub primary_key: bool,
    pub not_null: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Value),
    Col(String),
    Bin(Box<Expr>, BinOp, Box<Expr>),
    Not(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectItem {
    Star,
    Col(String),
    CountStar,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    CreateTable {
        name: String,
        cols: Vec<ColumnDef>,
    },
    CreateIndex {
        table: String,
        col: String,
    },
    Insert {
        table: String,
        cols: Option<Vec<String>>,
        rows: Vec<Vec<Expr>>,
    },
    Select {
        items: Vec<SelectItem>,
        table: String,
        filter: Option<Expr>,
        order: Option<(String, bool)>,
        limit: Option<usize>,
        /// `AS OF TXN n`: read the snapshot as it was after transaction n.
        as_of: Option<u64>,
    },
    Update {
        table: String,
        sets: Vec<(String, Expr)>,
        filter: Option<Expr>,
    },
    Delete {
        table: String,
        filter: Option<Expr>,
    },
    Begin,
    Commit,
    Rollback,
    /// `APPEND TO stream <expr>`: append one event to an append-only stream.
    Append {
        stream: String,
        value: Expr,
    },
    /// `READ stream [SINCE seq] [LIMIT n]`.
    Read {
        stream: String,
        since: u64,
        limit: Option<usize>,
    },
    /// `SHOW TABLES`, `SHOW STREAMS`, `SHOW TXN`.
    Show(String),
    /// `HISTORY table WHERE ...`: every version of the matching rows.
    History {
        table: String,
        filter: Option<Expr>,
    },
}

struct P {
    toks: Vec<Token>,
    pos: usize,
}

impl P {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Token::Ident(s)) if s.eq_ignore_ascii_case(kw))
    }
    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect_kw(&mut self, kw: &str) -> Result<(), String> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(format!("expected {kw}, found {:?}", self.peek()))
        }
    }
    fn eat_sym(&mut self, s: &str) -> bool {
        if matches!(self.peek(), Some(Token::Sym(x)) if *x == s) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect_sym(&mut self, s: &str) -> Result<(), String> {
        if self.eat_sym(s) {
            Ok(())
        } else {
            Err(format!("expected '{s}', found {:?}", self.peek()))
        }
    }
    fn ident(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Token::Ident(s)) => Ok(s),
            other => Err(format!("expected identifier, found {other:?}")),
        }
    }
    fn int(&mut self) -> Result<u64, String> {
        match self.next() {
            Some(Token::Lit(Value::Int(n))) if n >= 0 => Ok(n as u64),
            other => Err(format!("expected non-negative integer, found {other:?}")),
        }
    }

    fn stmt(&mut self) -> Result<Stmt, String> {
        let kw = match self.peek() {
            Some(Token::Ident(s)) => s.to_ascii_uppercase(),
            other => return Err(format!("expected statement, found {other:?}")),
        };
        self.pos += 1;
        match kw.as_str() {
            "CREATE" => {
                if self.eat_kw("TABLE") {
                    let name = self.ident()?;
                    self.expect_sym("(")?;
                    let mut cols = Vec::new();
                    loop {
                        let name = self.ident()?;
                        let ty = match self.ident()?.to_ascii_uppercase().as_str() {
                            "INT" | "INTEGER" | "BIGINT" => ColType::Int,
                            "FLOAT" | "REAL" | "DOUBLE" => ColType::Float,
                            "TEXT" | "STRING" | "VARCHAR" => ColType::Text,
                            "BOOL" | "BOOLEAN" => ColType::Bool,
                            t => return Err(format!("unknown type {t}")),
                        };
                        let mut def = ColumnDef {
                            name,
                            ty,
                            primary_key: false,
                            not_null: false,
                        };
                        loop {
                            if self.eat_kw("PRIMARY") {
                                self.expect_kw("KEY")?;
                                def.primary_key = true;
                                def.not_null = true;
                            } else if self.eat_kw("NOT") {
                                self.expect_kw("NULL")?;
                                def.not_null = true;
                            } else {
                                break;
                            }
                        }
                        cols.push(def);
                        if !self.eat_sym(",") {
                            break;
                        }
                    }
                    self.expect_sym(")")?;
                    if cols.iter().filter(|c| c.primary_key).count() > 1 {
                        return Err("only one PRIMARY KEY column is supported".into());
                    }
                    Ok(Stmt::CreateTable { name, cols })
                } else if self.eat_kw("INDEX") {
                    self.expect_kw("ON")?;
                    let table = self.ident()?;
                    self.expect_sym("(")?;
                    let col = self.ident()?;
                    self.expect_sym(")")?;
                    Ok(Stmt::CreateIndex { table, col })
                } else {
                    Err("expected TABLE or INDEX after CREATE".into())
                }
            }
            "INSERT" => {
                self.expect_kw("INTO")?;
                let table = self.ident()?;
                let cols = if self.eat_sym("(") {
                    let mut v = vec![self.ident()?];
                    while self.eat_sym(",") {
                        v.push(self.ident()?);
                    }
                    self.expect_sym(")")?;
                    Some(v)
                } else {
                    None
                };
                self.expect_kw("VALUES")?;
                let mut rows = Vec::new();
                loop {
                    self.expect_sym("(")?;
                    let mut row = vec![self.expr()?];
                    while self.eat_sym(",") {
                        row.push(self.expr()?);
                    }
                    self.expect_sym(")")?;
                    rows.push(row);
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                Ok(Stmt::Insert { table, cols, rows })
            }
            "SELECT" => {
                let mut items = Vec::new();
                loop {
                    if self.eat_sym("*") {
                        items.push(SelectItem::Star);
                    } else if self.is_kw("COUNT") {
                        self.pos += 1;
                        self.expect_sym("(")?;
                        self.expect_sym("*")?;
                        self.expect_sym(")")?;
                        items.push(SelectItem::CountStar);
                    } else {
                        items.push(SelectItem::Col(self.ident()?));
                    }
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_kw("FROM")?;
                let table = self.ident()?;
                let mut as_of = None;
                if self.eat_kw("AS") {
                    self.expect_kw("OF")?;
                    self.expect_kw("TXN")?;
                    as_of = Some(self.int()?);
                }
                let filter = if self.eat_kw("WHERE") {
                    Some(self.expr()?)
                } else {
                    None
                };
                let order = if self.eat_kw("ORDER") {
                    self.expect_kw("BY")?;
                    let col = self.ident()?;
                    let desc = self.eat_kw("DESC");
                    if !desc {
                        self.eat_kw("ASC");
                    }
                    Some((col, desc))
                } else {
                    None
                };
                let limit = if self.eat_kw("LIMIT") {
                    Some(self.int()? as usize)
                } else {
                    None
                };
                Ok(Stmt::Select {
                    items,
                    table,
                    filter,
                    order,
                    limit,
                    as_of,
                })
            }
            "UPDATE" => {
                let table = self.ident()?;
                self.expect_kw("SET")?;
                let mut sets = Vec::new();
                loop {
                    let col = self.ident()?;
                    self.expect_sym("=")?;
                    sets.push((col, self.expr()?));
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                let filter = if self.eat_kw("WHERE") {
                    Some(self.expr()?)
                } else {
                    None
                };
                Ok(Stmt::Update {
                    table,
                    sets,
                    filter,
                })
            }
            "DELETE" => {
                self.expect_kw("FROM")?;
                let table = self.ident()?;
                let filter = if self.eat_kw("WHERE") {
                    Some(self.expr()?)
                } else {
                    None
                };
                Ok(Stmt::Delete { table, filter })
            }
            "HISTORY" => {
                let table = self.ident()?;
                let filter = if self.eat_kw("WHERE") {
                    Some(self.expr()?)
                } else {
                    None
                };
                Ok(Stmt::History { table, filter })
            }
            "BEGIN" | "START" => {
                self.eat_kw("TRANSACTION");
                Ok(Stmt::Begin)
            }
            "COMMIT" | "END" => Ok(Stmt::Commit),
            "ROLLBACK" | "ABORT" => Ok(Stmt::Rollback),
            "APPEND" => {
                self.expect_kw("TO")?;
                let stream = self.ident()?;
                let value = self.expr()?;
                Ok(Stmt::Append { stream, value })
            }
            "READ" => {
                let stream = self.ident()?;
                let since = if self.eat_kw("SINCE") { self.int()? } else { 0 };
                let limit = if self.eat_kw("LIMIT") {
                    Some(self.int()? as usize)
                } else {
                    None
                };
                Ok(Stmt::Read {
                    stream,
                    since,
                    limit,
                })
            }
            "SHOW" => Ok(Stmt::Show(self.ident()?.to_ascii_uppercase())),
            other => Err(format!("unknown statement {other}")),
        }
    }

    fn expr(&mut self) -> Result<Expr, String> {
        self.or()
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut l = self.and()?;
        while self.eat_kw("OR") {
            l = Expr::Bin(Box::new(l), BinOp::Or, Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut l = self.not()?;
        while self.eat_kw("AND") {
            l = Expr::Bin(Box::new(l), BinOp::And, Box::new(self.not()?));
        }
        Ok(l)
    }
    fn not(&mut self) -> Result<Expr, String> {
        if self.eat_kw("NOT") {
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let l = self.add()?;
        let op = match self.peek() {
            Some(Token::Sym("=")) => BinOp::Eq,
            Some(Token::Sym("!=")) => BinOp::Ne,
            Some(Token::Sym("<")) => BinOp::Lt,
            Some(Token::Sym("<=")) => BinOp::Le,
            Some(Token::Sym(">")) => BinOp::Gt,
            Some(Token::Sym(">=")) => BinOp::Ge,
            _ => return Ok(l),
        };
        self.pos += 1;
        Ok(Expr::Bin(Box::new(l), op, Box::new(self.add()?)))
    }
    fn add(&mut self) -> Result<Expr, String> {
        let mut l = self.mul()?;
        loop {
            let op = match self.peek() {
                Some(Token::Sym("+")) => BinOp::Add,
                Some(Token::Sym("-")) => BinOp::Sub,
                _ => return Ok(l),
            };
            self.pos += 1;
            l = Expr::Bin(Box::new(l), op, Box::new(self.mul()?));
        }
    }
    fn mul(&mut self) -> Result<Expr, String> {
        let mut l = self.atom()?;
        loop {
            let op = match self.peek() {
                Some(Token::Sym("*")) => BinOp::Mul,
                Some(Token::Sym("/")) => BinOp::Div,
                _ => return Ok(l),
            };
            self.pos += 1;
            l = Expr::Bin(Box::new(l), op, Box::new(self.atom()?));
        }
    }
    fn atom(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Token::Lit(v)) => Ok(Expr::Lit(v)),
            Some(Token::Sym("-")) => Ok(Expr::Bin(
                Box::new(Expr::Lit(Value::Int(0))),
                BinOp::Sub,
                Box::new(self.atom()?),
            )),
            Some(Token::Sym("(")) => {
                let e = self.expr()?;
                self.expect_sym(")")?;
                Ok(e)
            }
            Some(Token::Ident(s)) => Ok(match s.to_ascii_uppercase().as_str() {
                "TRUE" => Expr::Lit(Value::Bool(true)),
                "FALSE" => Expr::Lit(Value::Bool(false)),
                "NULL" => Expr::Lit(Value::Null),
                _ => Expr::Col(s),
            }),
            other => Err(format!("unexpected {other:?} in expression")),
        }
    }
}

/// Parses one or more `;`-separated statements.
pub fn parse(src: &str) -> Result<Vec<Stmt>, String> {
    let mut p = P {
        toks: lex(src)?,
        pos: 0,
    };
    let mut out = Vec::new();
    while p.peek().is_some() {
        if p.eat_sym(";") {
            continue;
        }
        out.push(p.stmt()?);
        if p.peek().is_some() && !p.eat_sym(";") {
            return Err(format!("unexpected {:?} after statement", p.peek()));
        }
    }
    Ok(out)
}
