//! The fold. Items are derived state; the logs are the truth.
//!
//! Rebuilt from disk whenever any log advances. That is O(all events) and
//! fine for now — this is a coordination board, not a firehose. When it
//! isn't fine the fix is incremental folding, not a database.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::log::Event;

pub const STATUSES: [&str; 6] = ["open", "claimed", "blocked", "asked", "done", "parked"];
pub const PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];

#[derive(Clone, Debug, Serialize)]
pub struct Hist {
    pub at: String,
    pub by: String,
    pub kind: String,
    pub writer: String,
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// what the fold decided about this event, when it differs from what
    /// the event asked for ("claim lost to X")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fold: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub product: String,
    pub title: String,
    pub status: String,
    pub owner: String,
    pub priority: String,
    pub pointers: Value,
    pub watchers: Value,
    pub created_by: String,
    pub updated_at: String,
    pub history: Vec<Hist>,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct State {
    pub items: BTreeMap<String, Item>,
    pub writers: BTreeMap<String, u64>,
    pub events: usize,
    pub built_at: String,
}

fn s(e: &Event, k: &str) -> Option<String> {
    e.get(k).and_then(Value::as_str).map(String::from)
}
fn pick(v: Option<String>, allowed: &[&str], default: &str) -> String {
    match v {
        Some(v) if allowed.contains(&v.as_str()) => v,
        _ => default.to_string(),
    }
}
fn hist(e: &Event) -> Hist {
    Hist {
        at: s(e, "at").unwrap_or_default(),
        by: s(e, "by").unwrap_or_default(),
        kind: s(e, "kind").unwrap_or_default(),
        writer: s(e, "writer").unwrap_or_default(),
        seq: e.get("seq").and_then(Value::as_u64).unwrap_or(0),
        note: s(e, "note"),
        value: s(e, "value"),
        to: s(e, "to"),
        fold: None,
    }
}

/// Fold a list of events (already in `(at, writer, seq)` order) into items.
pub fn fold(events: &[Event]) -> BTreeMap<String, Item> {
    let mut items: BTreeMap<String, Item> = BTreeMap::new();
    for e in events {
        let (Some(kind), Some(id)) = (s(e, "kind"), s(e, "id")) else {
            continue;
        };
        let by = s(e, "by").unwrap_or_default();
        let at = s(e, "at").unwrap_or_default();

        if kind == "create" {
            if items.contains_key(&id) {
                // first create wins; a second create for the same id is just a note
                touch(
                    &mut items,
                    &id,
                    e,
                    Some("duplicate create ignored".into()),
                    |_| {},
                );
            } else {
                items.insert(
                    id.clone(),
                    Item {
                        id: id.clone(),
                        kind: s(e, "type").unwrap_or_else(|| "fyi".into()),
                        product: s(e, "product").unwrap_or_default(),
                        title: s(e, "title").unwrap_or_else(|| id.clone()),
                        status: pick(s(e, "status"), &STATUSES, "open"),
                        owner: s(e, "owner").unwrap_or_default(),
                        priority: pick(s(e, "priority"), &PRIORITIES, ""),
                        pointers: e
                            .get("pointers")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Map::new())),
                        watchers: e
                            .get("watchers")
                            .cloned()
                            .unwrap_or_else(|| Value::Array(vec![])),
                        created_by: by,
                        updated_at: at,
                        history: vec![hist(e)],
                    },
                );
            }
            continue;
        }

        match kind.as_str() {
            "claim" => {
                let status = pick(s(e, "status"), &STATUSES, "claimed");
                let lost = items
                    .get(&id)
                    .filter(|i| !(i.owner.is_empty() || i.owner == by))
                    .map(|i| format!("claim lost to {}", i.owner));
                touch(&mut items, &id, e, lost.clone(), |i| {
                    // earlier claim already folded in → this one lost; item
                    // unchanged, but the attempt stays in history so the
                    // loser can see why
                    if lost.is_none() {
                        i.owner = by.clone();
                        i.status = status.clone();
                    }
                });
            }
            "release" => {
                let status = pick(s(e, "status"), &STATUSES, "open");
                touch(&mut items, &id, e, None, |i| {
                    if i.owner == by {
                        i.owner.clear();
                        i.status = status.clone();
                    }
                });
            }
            "status" => {
                let v = s(e, "value");
                touch(&mut items, &id, e, None, |i| {
                    i.status = pick(v.clone(), &STATUSES, &i.status.clone())
                });
            }
            "priority" => {
                let v = s(e, "value");
                touch(&mut items, &id, e, None, |i| {
                    i.priority = pick(v.clone(), &PRIORITIES, "")
                });
            }
            "assign" => {
                let to = s(e, "to");
                let status = pick(s(e, "status"), &STATUSES, "claimed");
                touch(&mut items, &id, e, None, |i| {
                    if let Some(to) = &to {
                        i.owner = to.clone();
                    }
                    i.status = status.clone();
                });
            }
            "resolve" => {
                let status = pick(s(e, "status"), &STATUSES, "done");
                touch(&mut items, &id, e, None, |i| i.status = status.clone());
            }
            "note" => touch(&mut items, &id, e, None, |_| {}),
            // unknown kind: the log keeps it, the fold ignores it
            _ => {}
        }
    }
    items
}

/// Apply `f` to an existing item and record the event in its history. An
/// event for an item we haven't seen created is dropped: its create may be
/// in a log we haven't pulled yet, and the next refresh may have it.
fn touch(
    items: &mut BTreeMap<String, Item>,
    id: &str,
    e: &Event,
    fold_note: Option<String>,
    f: impl FnOnce(&mut Item),
) {
    if let Some(item) = items.get_mut(id) {
        f(item);
        let mut h = hist(e);
        h.fold = fold_note;
        item.updated_at = h.at.clone();
        item.history.push(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(kind: &str, id: &str, at: &str, by: &str, extra: Value) -> Event {
        let mut m = json!({"kind":kind,"id":id,"at":at,"by":by,"writer":by,"seq":1})
            .as_object()
            .unwrap()
            .clone();
        if let Some(o) = extra.as_object() {
            m.extend(o.clone());
        }
        m
    }

    #[test]
    fn earliest_claim_wins_and_loser_is_recorded() {
        let items = fold(&[
            ev(
                "create",
                "x",
                "2026-09-16T10:00:00.000Z",
                "cto",
                json!({"title":"X"}),
            ),
            ev("claim", "x", "2026-09-16T10:00:01.000Z", "grok", json!({})),
            ev(
                "claim",
                "x",
                "2026-09-16T10:00:04.000Z",
                "claude",
                json!({}),
            ),
        ]);
        let x = &items["x"];
        assert_eq!(x.owner, "grok");
        assert_eq!(x.status, "claimed");
        assert_eq!(x.history[2].fold.as_deref(), Some("claim lost to grok"));
    }

    #[test]
    fn only_owner_releases() {
        let items = fold(&[
            ev("create", "x", "2026-09-16T10:00:00.000Z", "cto", json!({})),
            ev("claim", "x", "2026-09-16T10:00:01.000Z", "grok", json!({})),
            ev(
                "release",
                "x",
                "2026-09-16T10:00:02.000Z",
                "claude",
                json!({}),
            ),
            ev(
                "release",
                "x",
                "2026-09-16T10:00:03.000Z",
                "grok",
                json!({}),
            ),
        ]);
        assert_eq!(items["x"].owner, "");
        assert_eq!(items["x"].status, "open");
        assert_eq!(items["x"].history.len(), 4);
    }

    #[test]
    fn unknown_item_is_dropped_not_crashed_on() {
        assert!(fold(&[ev(
            "claim",
            "ghost",
            "2026-09-16T10:00:00.000Z",
            "grok",
            json!({})
        )])
        .is_empty());
    }
}
