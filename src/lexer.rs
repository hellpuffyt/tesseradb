//! SQL tokenizer. Keywords are plain identifiers compared case-insensitively
//! by the parser, so the lexer stays tiny.

use crate::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Ident(String),
    Lit(Value),
    /// One of `( ) , ; * = < > + - / !` or the two-char forms `<= >= != <>`.
    Sym(&'static str),
}

pub fn lex(src: &str) -> Result<Vec<Token>, String> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
                i += 1;
            }
            out.push(Token::Ident(src[s..i].to_string()));
        } else if c.is_ascii_digit() {
            let s = i;
            let mut float = false;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                float |= b[i] == b'.';
                i += 1;
            }
            let text = &src[s..i];
            out.push(Token::Lit(if float {
                Value::Float(text.parse().map_err(|_| format!("bad number {text}"))?)
            } else {
                Value::Int(text.parse().map_err(|_| format!("bad number {text}"))?)
            }));
        } else if c == b'\'' {
            let mut s = String::new();
            i += 1;
            loop {
                match b.get(i) {
                    None => return Err("unterminated string".into()),
                    Some(b'\'') if b.get(i + 1) == Some(&b'\'') => {
                        s.push('\'');
                        i += 2;
                    }
                    Some(b'\'') => {
                        i += 1;
                        break;
                    }
                    Some(_) => {
                        let ch = src[i..].chars().next().unwrap();
                        s.push(ch);
                        i += ch.len_utf8();
                    }
                }
            }
            out.push(Token::Lit(Value::Text(s)));
        } else {
            let two = src.get(i..i + 2).unwrap_or("");
            let sym = match two {
                "<=" => Some("<="),
                ">=" => Some(">="),
                "!=" => Some("!="),
                "<>" => Some("!="),
                _ => None,
            };
            if let Some(s) = sym {
                out.push(Token::Sym(s));
                i += 2;
                continue;
            }
            let s = match c {
                b'(' => "(",
                b')' => ")",
                b',' => ",",
                b';' => ";",
                b'*' => "*",
                b'=' => "=",
                b'<' => "<",
                b'>' => ">",
                b'+' => "+",
                b'-' => "-",
                b'/' => "/",
                _ => return Err(format!("unexpected character {:?}", c as char)),
            };
            out.push(Token::Sym(s));
            i += 1;
        }
    }
    Ok(out)
}
