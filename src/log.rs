//! Per-writer append-only logs on disk.
//!
//!     <dir>/log/<writer>.jsonl       this node's own log
//!     <dir>/mirror/<writer>.jsonl    pulled copies of other writers' logs
//!
//! Nobody writes to anyone else's log: you append to your own and pull
//! copies of the rest. That is the whole reason there is nothing to
//! conflict on. Every line is one event with `writer`, `seq` (1-based,
//! contiguous per writer) and `at`; `seq` is what makes a pull resumable
//! and idempotent.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};

pub type Event = Map<String, Value>;

pub struct Logs {
    pub dir: PathBuf,
    pub writer: String,
    /// seq of my log: one lock, so exactly one thing hands out numbers
    own: Mutex<u64>,
    /// pulled events land through one lock, so two pullers that both
    /// fetched `grok-cto` from different peers cannot interleave writes
    mirrors: Mutex<()>,
}

pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-'))
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn seq_of(e: &Event) -> u64 {
    e.get("seq").and_then(Value::as_u64).unwrap_or(0)
}

impl Logs {
    pub fn open(dir: &Path, writer: &str) -> Logs {
        let logs = Logs {
            dir: dir.to_path_buf(),
            writer: writer.to_string(),
            own: Mutex::new(0),
            mirrors: Mutex::new(()),
        };
        let last = logs.last_seq(writer);
        *logs.own.lock().unwrap() = last;
        logs
    }

    pub fn path(&self, name: &str) -> PathBuf {
        let sub = if name == self.writer { "log" } else { "mirror" };
        self.dir.join(sub).join(format!("{name}.jsonl"))
    }

    /// Every writer we hold a log for, own first.
    pub fn writers(&self) -> Vec<String> {
        let mut out = vec![];
        for sub in ["log", "mirror"] {
            let Ok(rd) = std::fs::read_dir(self.dir.join(sub)) else {
                continue;
            };
            let mut names: Vec<String> = rd
                .flatten()
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".jsonl"))
                        .map(String::from)
                })
                .collect();
            names.sort();
            for n in names {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        out
    }

    /// Events from `name` with seq > since, in order.
    pub fn read(&self, name: &str, since: u64) -> Vec<Event> {
        let Ok(body) = std::fs::read_to_string(self.path(name)) else {
            return vec![];
        };
        body.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Event>(l).ok())
            .filter(|e| seq_of(e) > since)
            .collect()
    }

    pub fn last_seq(&self, name: &str) -> u64 {
        self.read(name, 0).last().map(seq_of).unwrap_or(0)
    }

    fn append_line(&self, name: &str, event: &Event) -> std::io::Result<()> {
        let p = self.path(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(p)?;
        writeln!(f, "{}", serde_json::to_string(event)?)
    }

    /// Append to my log. Assigns `writer`, `seq`, `at`; `by` defaults to
    /// me. `by` is who acted; `writer` is whose log it landed in — they
    /// differ once a gateway writes on behalf of an agent key.
    pub fn append_own(&self, mut attrs: Event) -> std::io::Result<Event> {
        let mut seq = self.own.lock().unwrap();
        let next = *seq + 1;
        attrs
            .entry("by")
            .or_insert_with(|| Value::String(self.writer.clone()));
        attrs.insert("writer".into(), Value::String(self.writer.clone()));
        attrs.insert("seq".into(), Value::from(next));
        attrs.insert("at".into(), Value::String(now()));
        self.append_line(&self.writer, &attrs)?;
        *seq = next;
        Ok(attrs)
    }

    /// Ingest pulled events into a mirror. Only contiguous seqs are
    /// accepted; a gap means we asked the wrong `since` or a peer is
    /// missing the middle, and the right answer is to stop and pull again,
    /// not to write a hole. Returns how many were written.
    pub fn ingest(&self, writer: &str, events: &[Event]) -> Result<usize, &'static str> {
        if writer == self.writer {
            return Err("own log");
        }
        if !valid_name(writer) {
            return Err("bad name");
        }
        let _g = self.mirrors.lock().unwrap();
        let mut last = self.last_seq(writer);
        let mut n = 0;
        for ev in events {
            if ev.get("writer").and_then(Value::as_str) != Some(writer) {
                tracing::warn!("mirror {writer}: event not from that writer, dropping");
                continue;
            }
            let seq = seq_of(ev);
            if seq <= last {
                continue; // already have it; a peer re-sent — harmless
            }
            if seq != last + 1 {
                tracing::warn!(
                    "mirror {writer}: gap at seq {seq} (have {last}); dropping rest of batch"
                );
                break;
            }
            self.append_line(writer, ev).map_err(|_| "write failed")?;
            last = seq;
            n += 1;
        }
        Ok(n)
    }

    /// Every event from every log, in fold order: `(at, writer, seq)`.
    /// Sorting by `at` first is what makes "earliest claim wins" mean the
    /// same thing on every node; `writer` then `seq` break ties.
    pub fn all(&self) -> Vec<Event> {
        let mut all: Vec<Event> = self
            .writers()
            .iter()
            .flat_map(|w| self.read(w, 0))
            .collect();
        all.sort_by(|a, b| {
            let k = |e: &Event| {
                (
                    e.get("at")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    e.get("writer")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    seq_of(e),
                )
            };
            k(a).cmp(&k(b))
        });
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(writer: &str, seq: u64) -> Event {
        json!({"kind":"note","id":"x","writer":writer,"seq":seq,"at":format!("2026-09-16T10:00:0{seq}.000Z"),"by":writer})
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn ingest_accepts_contiguous_skips_held_halts_at_gap() {
        let tmp = tempfile::tempdir().unwrap();
        let logs = Logs::open(tmp.path(), "me");
        assert_eq!(logs.ingest("peer", &[ev("peer", 1), ev("peer", 2)]), Ok(2));
        assert_eq!(
            logs.ingest("peer", &[ev("peer", 1), ev("peer", 2), ev("peer", 3)]),
            Ok(1)
        );
        assert_eq!(logs.ingest("peer", &[ev("peer", 5)]), Ok(0));
        assert_eq!(logs.last_seq("peer"), 3);
    }

    #[test]
    fn ingest_refuses_own_log_and_foreign_events() {
        let tmp = tempfile::tempdir().unwrap();
        let logs = Logs::open(tmp.path(), "me");
        assert_eq!(logs.ingest("me", &[ev("me", 1)]), Err("own log"));
        assert_eq!(logs.ingest("peer", &[ev("someone-else", 1)]), Ok(0));
    }

    #[test]
    fn own_seq_continues_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let logs = Logs::open(tmp.path(), "me");
            logs.append_own(json!({"kind":"note","id":"x"}).as_object().unwrap().clone())
                .unwrap();
            logs.append_own(json!({"kind":"note","id":"x"}).as_object().unwrap().clone())
                .unwrap();
        }
        let logs = Logs::open(tmp.path(), "me");
        let e = logs
            .append_own(json!({"kind":"note","id":"x"}).as_object().unwrap().clone())
            .unwrap();
        assert_eq!(e["seq"], 3);
        assert_eq!(e["by"], "me");
    }

    #[test]
    fn names() {
        assert!(valid_name("claude@mac2024"));
        assert!(valid_name("doug-mini"));
        assert!(!valid_name("../etc"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(""));
    }
}
