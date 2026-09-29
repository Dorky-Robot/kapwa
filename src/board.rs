//! The fold. Items are derived state; the logs are the truth.
//!
//! Five verbs, and an item is whatever they add up to:
//!
//!   say    a new item, or a note on one that exists
//!   take   it's mine
//!   drop   not mine anymore
//!   done   finished
//!   ask    a person (or anyone) must answer before this moves
//!
//! Rebuilt from disk whenever any log advances. That is O(all events) and
//! fine for now — this is a ledger of commitments, not a firehose. When it
//! isn't fine the fix is incremental folding, not a database.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::log::Event;

pub const PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];

#[derive(Clone, Debug, Serialize)]
pub struct Hist {
    pub at: String,
    pub by: String,
    pub verb: String,
    pub writer: String,
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// what the fold decided about this event, when it differs from what
    /// the event asked for ("take lost to X", "answers the ask")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fold: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub id: String,
    pub title: String,
    /// open · taken · asked · done
    pub status: String,
    pub owner: String,
    /// who must answer, when status is `asked` ("" = anyone)
    pub asked_of: String,
    pub asked_by: String,
    pub priority: String,
    /// every topic any event about this item carried; an item can have many
    pub topics: Vec<String>,
    /// everyone this item was ever addressed to
    pub to: Vec<String>,
    pub created_by: String,
    pub created_at: String,
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

/// The wire verb for a `kind`. The older names are accepted so a log
/// written before the verbs were settled still folds.
pub fn verb(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "say" | "create" | "note" => "say",
        "take" | "claim" => "take",
        "drop" | "release" => "drop",
        "done" | "resolve" => "done",
        "ask" => "ask",
        _ => return None,
    })
}

/// `claude/ab12` is a session of `claude`.
pub fn base(name: &str) -> &str {
    name.split('/').next().unwrap_or(name)
}

/// Is `by` the participant `target` names? A key name covers all of its
/// sessions; a session name covers only itself.
pub fn is(by: &str, target: &str) -> bool {
    by == target || base(by) == target
}

/// Same key, whatever the session. A tag is the key's own claim about which
/// of its sessions wrote, so it can sign but it cannot fence: any session of
/// a key could have written under any other tag.
fn same_key(a: &str, b: &str) -> bool {
    !b.is_empty() && base(a) == base(b)
}

/// May `by` close this item? `Err` says who can, so a refusal teaches.
///
/// An ask that names someone is theirs to close: the asker handed the
/// decision over, and whoever holds the item is waiting on it too. Anything
/// else is yours to close if you hold it, opened it, or asked it of anyone
/// — including "decided against, because". A person signed in is who the
/// keys answer to, and may close anything; who counts as one is decided by
/// the identity provider, not here.
///
/// Everyone else says instead: that leaves the note and not the verdict.
/// Checked where a key writes, not in the fold, so history written before
/// the rule still reads as it did.
pub fn may_close(item: &Item, by: &str, person: bool) -> Result<(), String> {
    if person {
        return Ok(());
    }
    if item.status == "asked" && !item.asked_of.is_empty() {
        return if same_key(by, &item.asked_of) {
            Ok(())
        } else {
            Err(format!(
                "this is asked of {}, and only they can close it; `say` to add to it",
                item.asked_of
            ))
        };
    }
    let asked_it = item.status == "asked" && same_key(by, &item.asked_by);
    if asked_it || same_key(by, &item.owner) || same_key(by, &item.created_by) {
        return Ok(());
    }
    let whose = if item.owner.is_empty() {
        format!("{} opened it", item.created_by)
    } else {
        format!("{} holds it", item.owner)
    };
    Err(format!(
        "not yours to close: {whose}; `say` to add to it, or `take` it if nobody holds it"
    ))
}

fn s(e: &Event, k: &str) -> Option<String> {
    e.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

fn text_of(e: &Event) -> Option<String> {
    s(e, "text")
        .or_else(|| s(e, "title"))
        .or_else(|| s(e, "note"))
}

pub fn clean_topic(t: &str) -> Option<String> {
    let t = t.trim().trim_start_matches('#').to_lowercase();
    (!t.is_empty()).then_some(t)
}

/// What one participant may see of the board, and under what scope.
///
/// `topics` narrows: empty means everything, as it always has. `fence`
/// subtracts, and is the part that is not self-serve — a topic matching one
/// of its patterns is on no default board, and a key reads it only because
/// its own line in the agents file names the topic. So a scope asked for in
/// a query narrows what you see and can never widen it, which is the one
/// way a fence like this leaks.
///
/// It is a lens, not a redaction: fenced events still replicate to every
/// node, and anyone holding a key that names the topic still reads them.
/// The point is that clinical work does not land, unasked, in the context
/// window of every agent that starts a session.
#[derive(Clone, Debug, Default)]
pub struct Lens {
    pub topics: Vec<String>,
    pub fence: Vec<String>,
}

impl Lens {
    /// Everything, fenced by nothing: the view a node with no fence set has
    /// always had, and what the tests want.
    #[cfg(test)]
    pub fn all() -> Lens {
        Lens::default()
    }

    /// Narrowed to these topics, fenced by nothing.
    #[cfg(test)]
    pub fn under(topics: &[&str]) -> Lens {
        Lens {
            topics: topics.iter().map(|t| t.to_string()).collect(),
            fence: vec![],
        }
    }

    pub fn wanted(&self, i: &Item) -> bool {
        (self.topics.is_empty() || i.topics.iter().any(|t| self.topics.contains(t)))
            && !i.topics.iter().any(|t| fenced(t, &self.fence))
    }

    pub fn scope(&self) -> String {
        if self.topics.is_empty() {
            String::new()
        } else {
            format!("#{}", self.topics.join(" #"))
        }
    }
}

/// Is this topic behind the fence? A pattern matches as a substring, so one
/// word covers a family of topics without anybody having to list each one
/// as it is invented — which is the failure mode a fence has to survive.
pub fn fenced(topic: &str, fence: &[String]) -> bool {
    fence.iter().any(|p| topic.contains(p.as_str()))
}

pub fn topics_of(e: &Event) -> Vec<String> {
    match e.get("t") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .filter_map(clean_topic)
            .collect(),
        Some(Value::String(v)) => v.split(',').filter_map(clean_topic).collect(),
        _ => vec![],
    }
}

fn hist(e: &Event, verb: &str) -> Hist {
    Hist {
        at: s(e, "at").unwrap_or_default(),
        by: s(e, "by").unwrap_or_default(),
        verb: verb.to_string(),
        writer: s(e, "writer").unwrap_or_default(),
        seq: e.get("seq").and_then(Value::as_u64).unwrap_or(0),
        text: text_of(e),
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
        // unknown kind: the log keeps it, the fold ignores it
        let Some(v) = verb(&kind) else { continue };
        let by = s(e, "by").unwrap_or_default();
        let at = s(e, "at").unwrap_or_default();
        let mut h = hist(e, v);

        let Some(item) = items.get_mut(&id) else {
            // only `say` brings an item into being. Anything else about an
            // item we haven't seen is dropped: its first `say` may be in a
            // log we haven't synced yet, and the next fold may have it.
            if v == "say" {
                let mut topics = topics_of(e);
                topics.sort();
                topics.dedup();
                items.insert(
                    id.clone(),
                    Item {
                        id: id.clone(),
                        title: text_of(e).unwrap_or_else(|| id.clone()),
                        status: "open".into(),
                        owner: String::new(),
                        asked_of: String::new(),
                        asked_by: String::new(),
                        priority: s(e, "p")
                            .filter(|p| PRIORITIES.contains(&p.as_str()))
                            .unwrap_or_default(),
                        topics,
                        to: s(e, "to").into_iter().collect(),
                        created_by: by,
                        created_at: at.clone(),
                        updated_at: at,
                        history: vec![h],
                    },
                );
            }
            continue;
        };

        match v {
            "say" => {
                // a word from whoever was asked (or, if nobody in
                // particular, from anyone but the asker) answers the ask
                let answers = item.status == "asked"
                    && !is(&by, &item.asked_by)
                    && (item.asked_of.is_empty() || is(&by, &item.asked_of));
                if answers {
                    item.status = if item.owner.is_empty() {
                        "open"
                    } else {
                        "taken"
                    }
                    .into();
                    item.asked_of.clear();
                    h.fold = Some("answers the ask".into());
                }
            }
            "take" => {
                if item.owner.is_empty() || item.owner == by {
                    item.owner = by.clone();
                    if item.status != "asked" {
                        item.status = "taken".into();
                    }
                } else {
                    // an earlier take already folded in; this one lost. The
                    // item is unchanged, but the attempt stays in history
                    // so the loser can see why.
                    h.fold = Some(format!("take lost to {}", item.owner));
                }
            }
            "drop" => {
                if item.owner == by {
                    item.owner.clear();
                    if item.status == "taken" {
                        item.status = "open".into();
                    }
                }
            }
            "done" => item.status = "done".into(),
            "ask" => {
                item.status = "asked".into();
                item.asked_by = by.clone();
                item.asked_of = s(e, "to").unwrap_or_default();
            }
            _ => {}
        }

        for t in topics_of(e) {
            if !item.topics.contains(&t) {
                item.topics.push(t);
            }
        }
        item.topics.sort();
        if let Some(to) = s(e, "to") {
            if !item.to.contains(&to) {
                item.to.push(to);
            }
        }
        if let Some(p) = s(e, "p").filter(|p| PRIORITIES.contains(&p.as_str())) {
            item.priority = p;
        }
        item.updated_at = at;
        item.history.push(h);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(kind: &str, id: &str, sec: u32, by: &str, extra: Value) -> Event {
        let mut m = json!({"kind":kind,"id":id,"at":format!("2026-09-17T10:00:{sec:02}.000Z"),"by":by,"writer":base(by),"seq":sec})
            .as_object()
            .unwrap()
            .clone();
        if let Some(o) = extra.as_object() {
            m.extend(o.clone());
        }
        m
    }

    #[test]
    fn say_makes_an_item_and_then_notes_on_it() {
        let items = fold(&[
            ev(
                "say",
                "x",
                0,
                "ana",
                json!({"text":"Roof leaks","t":["Roof","#urgent"]}),
            ),
            ev(
                "say",
                "x",
                1,
                "ben",
                json!({"text":"over the back door","t":"roof,water"}),
            ),
        ]);
        let x = &items["x"];
        assert_eq!(
            (x.title.as_str(), x.status.as_str(), x.history.len()),
            ("Roof leaks", "open", 2)
        );
        assert_eq!(x.topics, ["roof", "urgent", "water"]);
    }

    #[test]
    fn earliest_take_wins_and_the_loser_is_recorded() {
        let items = fold(&[
            ev("say", "x", 0, "ana", json!({"text":"X"})),
            ev("take", "x", 1, "grok", json!({})),
            ev("take", "x", 4, "claude", json!({})),
        ]);
        assert_eq!(
            (items["x"].owner.as_str(), items["x"].status.as_str()),
            ("grok", "taken")
        );
        assert_eq!(
            items["x"].history[2].fold.as_deref(),
            Some("take lost to grok")
        );
    }

    #[test]
    fn two_sessions_of_one_key_are_two_participants() {
        let items = fold(&[
            ev("say", "x", 0, "ana", json!({"text":"X"})),
            ev("take", "x", 1, "claude/a1", json!({})),
            ev("take", "x", 2, "claude/b2", json!({})),
        ]);
        assert_eq!(items["x"].owner, "claude/a1");
        assert_eq!(
            items["x"].history[2].fold.as_deref(),
            Some("take lost to claude/a1")
        );
    }

    #[test]
    fn only_the_owner_drops() {
        let items = fold(&[
            ev("say", "x", 0, "ana", json!({})),
            ev("take", "x", 1, "grok", json!({})),
            ev("drop", "x", 2, "claude", json!({})),
            ev("drop", "x", 3, "grok", json!({})),
        ]);
        assert_eq!(
            (items["x"].owner.as_str(), items["x"].status.as_str()),
            ("", "open")
        );
    }

    #[test]
    fn an_ask_waits_for_the_one_asked() {
        let asked = [
            ev("say", "x", 0, "ana", json!({"text":"Which plan?"})),
            ev("take", "x", 1, "claude/a1", json!({})),
            ev(
                "ask",
                "x",
                2,
                "claude/a1",
                json!({"to":"felix","text":"A or B?"}),
            ),
        ];
        let items = fold(&asked);
        assert_eq!(
            (items["x"].status.as_str(), items["x"].asked_of.as_str()),
            ("asked", "felix")
        );

        // somebody else chiming in does not answer it; nor does the asker
        let mut more = asked.to_vec();
        more.push(ev("say", "x", 3, "ben", json!({"text":"I'd pick A"})));
        more.push(ev(
            "say",
            "x",
            4,
            "claude/a1",
            json!({"text":"still waiting"}),
        ));
        assert_eq!(fold(&more)["x"].status, "asked");

        // the one asked does, and the item goes back to whoever holds it
        more.push(ev("say", "x", 5, "felix", json!({"text":"B"})));
        let x = &fold(&more)["x"];
        assert_eq!(
            (x.status.as_str(), x.owner.as_str()),
            ("taken", "claude/a1")
        );
        assert_eq!(
            x.history.last().unwrap().fold.as_deref(),
            Some("answers the ask")
        );
    }

    #[test]
    fn who_may_close_what() {
        let base = [
            ev("say", "x", 0, "ana/s1", json!({"text":"X"})),
            ev("take", "x", 1, "grok", json!({})),
        ];
        let x = &fold(&base)["x"];
        // the holder, the opener (from any session of its key), a person
        assert!(may_close(x, "grok/t9", false).is_ok());
        assert!(may_close(x, "ana/other", false).is_ok());
        assert!(may_close(x, "felix", true).is_ok());
        // and nobody else
        let no = may_close(x, "claude", false).unwrap_err();
        assert!(no.contains("grok holds it") && no.contains("say"), "{no}");

        // an ask that names someone is theirs alone, holder and asker included
        let mut asked = base.to_vec();
        asked.push(ev(
            "ask",
            "x",
            2,
            "grok",
            json!({"to":"felix","text":"A or B?"}),
        ));
        let x = &fold(&asked)["x"];
        for by in ["grok", "ana", "claude"] {
            let no = may_close(x, by, false).unwrap_err();
            assert!(no.contains("asked of felix"), "{by}: {no}");
        }
        assert!(may_close(x, "felix", false).is_ok(), "felix's own key");
        assert!(may_close(x, "felix", true).is_ok());

        // answered, it goes back to its holder to close
        asked.push(ev("say", "x", 3, "felix", json!({"text":"B"})));
        assert!(may_close(&fold(&asked)["x"], "grok", false).is_ok());

        // an ask of anyone may be closed by whoever asked it
        let open = [
            ev("say", "y", 0, "ana", json!({"text":"Y"})),
            ev("ask", "y", 1, "bot", json!({"text":"anyone?"})),
        ];
        let y = &fold(&open)["y"];
        assert!(may_close(y, "bot", false).is_ok());
        assert!(may_close(y, "claude", false).is_err());
    }

    #[test]
    fn a_verb_about_an_unknown_item_is_dropped_not_crashed_on() {
        assert!(fold(&[ev("take", "ghost", 0, "grok", json!({}))]).is_empty());
    }

    #[test]
    fn a_log_written_with_the_older_kinds_still_folds() {
        let items = fold(&[
            ev("create", "x", 0, "ana", json!({"title":"Old style"})),
            ev("claim", "x", 1, "grok", json!({})),
            ev("note", "x", 2, "grok", json!({"note":"on it"})),
            ev("resolve", "x", 3, "grok", json!({})),
        ]);
        assert_eq!(
            (items["x"].title.as_str(), items["x"].status.as_str()),
            ("Old style", "done")
        );
    }
}
