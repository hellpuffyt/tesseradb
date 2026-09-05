//! `tessera` — the TesseraDB shell.
//!
//! ```text
//! tessera app.db                  interactive shell
//! tessera app.db -e "SELECT …"    run one statement
//! tessera app.db -f script.sql    run a file
//! tessera --memory                scratch database
//! ```

use std::{
    io::{BufRead, Write},
    path::Path,
    sync::Arc,
};

use tesseradb::{Db, Output, Value};

fn render(out: &Output) -> String {
    match out {
        Output::Ok(s) => s.clone(),
        Output::Affected(n) => format!("{n} row(s) affected"),
        Output::Rows { cols, rows } => {
            let mut widths: Vec<usize> = cols.iter().map(String::len).collect();
            let cells: Vec<Vec<String>> = rows
                .iter()
                .map(|r| r.iter().map(Value::to_string).collect())
                .collect();
            for r in &cells {
                for (i, c) in r.iter().enumerate() {
                    widths[i] = widths[i].max(c.chars().count());
                }
            }
            let line = |cells: &[String]| {
                cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
                    .collect::<Vec<_>>()
                    .join(" │ ")
            };
            let mut s = line(cols);
            s.push('\n');
            s.push_str(
                &widths
                    .iter()
                    .map(|w| "─".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("─┼─"),
            );
            for r in &cells {
                s.push('\n');
                s.push_str(&line(r));
            }
            s.push_str(&format!(
                "\n({} row{})",
                rows.len(),
                if rows.len() == 1 { "" } else { "s" }
            ));
            s
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: tessera <file.db> [-e SQL | -f script.sql]\n       tessera --memory [-e SQL | -f script.sql]"
    );
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        usage();
    }
    let db: Arc<Db> = if args[0] == "--memory" {
        Db::open_in_memory()
    } else {
        match Db::open(Path::new(&args[0])) {
            Ok(db) => db,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    };
    let mut session = db.session();
    let mut exit = 0;

    let script: Option<String> = match args.get(1).map(String::as_str) {
        Some("-e") => Some(args.get(2).cloned().unwrap_or_else(|| usage())),
        Some("-f") => Some(
            std::fs::read_to_string(args.get(2).unwrap_or_else(|| usage())).unwrap_or_else(|e| {
                eprintln!("error: {e}");
                std::process::exit(1)
            }),
        ),
        Some(_) => usage(),
        None => None,
    };

    if let Some(sql) = script {
        match session.execute(&sql) {
            Ok(out) => println!("{}", render(&out)),
            Err(e) => {
                eprintln!("error: {e}");
                exit = 1;
            }
        }
    } else {
        println!(
            "TesseraDB {} — type .help for help, .quit to exit",
            env!("CARGO_PKG_VERSION")
        );
        let stdin = std::io::stdin();
        let mut buf = String::new();
        loop {
            print!(
                "{}",
                if session.in_transaction() {
                    "tessera*> "
                } else {
                    "tessera> "
                }
            );
            std::io::stdout().flush().ok();
            buf.clear();
            if stdin.lock().read_line(&mut buf).unwrap_or(0) == 0 {
                break;
            }
            let line = buf.trim();
            match line {
                "" => continue,
                ".quit" | ".exit" | "\\q" => break,
                ".help" => {
                    println!("{}", HELP);
                    continue;
                }
                ".checkpoint" => {
                    match db.checkpoint() {
                        Ok(()) => println!("checkpointed at txn {}", db.last_txn()),
                        Err(e) => eprintln!("error: {e}"),
                    }
                    continue;
                }
                _ => {}
            }
            match session.execute(line) {
                Ok(out) => println!("{}", render(&out)),
                Err(e) => eprintln!("error: {e}"),
            }
        }
    }
    if let Err(e) = db.checkpoint() {
        eprintln!("checkpoint failed: {e}");
        exit = 1;
    }
    std::process::exit(exit);
}

const HELP: &str = "\
statements
  CREATE TABLE t (id INT PRIMARY KEY, name TEXT NOT NULL, score FLOAT, done BOOL)
  CREATE INDEX ON t (name)
  INSERT INTO t VALUES (1, 'a', 1.5, false), (2, 'b', NULL, true)
  SELECT * FROM t WHERE score > 1 AND NOT done ORDER BY id DESC LIMIT 10
  SELECT COUNT(*) FROM t AS OF TXN 3          -- time travel
  HISTORY t WHERE id = 1                      -- every version of a row
  UPDATE t SET score = score + 1 WHERE id = 1
  DELETE FROM t WHERE done
  BEGIN / COMMIT / ROLLBACK
  APPEND TO events 'task 7 finished'          -- append-only streams
  READ events SINCE 0 LIMIT 100
  SHOW TABLES / SHOW STREAMS / SHOW TXN
shell
  .checkpoint   .help   .quit";
