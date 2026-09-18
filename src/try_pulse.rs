//! Seeing the mesh, rather than reading it.
//!
//! Two sandbox variants over one fold of the same record, because the board
//! answers "what is on it" and never "what is happening to it". Everything
//! here is derived from the logs at request time; nothing new is stored, and
//! no new route exists — `?data=1` on either page returns the same JSON the
//! page draws, which is also how it refreshes when the live stream fires.
//!
//! Colour follows the data's job, not taste. Two series (opened, finished)
//! take categorical slots 1 and 2, validated for contrast and colour-vision
//! separation in both light and dark. Everything else is one hue plus grey:
//! there are seventeen writers, and inventing seventeen hues would make a
//! chart nobody can read — so identity is carried by labels and position,
//! and colour is spent only where it means something.

use std::collections::{BTreeMap, BTreeSet};

use maud::{html, Markup, PreEscaped, DOCTYPE};
use serde_json::{json, Value};

use crate::App;

fn clip(s: &str, n: usize) -> String {
    let s = s.split('\n').next().unwrap_or(s).trim();
    if s.chars().count() <= n {
        return s.to_string();
    }
    format!("{}…", s.chars().take(n - 1).collect::<String>())
}

fn hours_since(at: &str, now: chrono::DateTime<chrono::Utc>) -> i64 {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|t| (now - t.with_timezone(&chrono::Utc)).num_minutes() / 60)
        .unwrap_or(0)
}

/// One fold of the whole record into everything both pages draw.
pub fn data(app: &App) -> Value {
    let evs = app.logs.all();
    let now = chrono::Utc::now();

    // fourteen days of flow. An item is "opened" on the day of its first
    // event, which is the only definition the log can actually support.
    const SPAN: i64 = 14;
    let days: Vec<String> = (0..SPAN)
        .rev()
        .map(|i| {
            (now - chrono::Duration::days(i))
                .format("%Y-%m-%d")
                .to_string()
        })
        .collect();
    let mut opened: BTreeMap<String, u32> = BTreeMap::new();
    let mut finished: BTreeMap<String, u32> = BTreeMap::new();
    let mut asked: BTreeMap<String, u32> = BTreeMap::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut said: BTreeMap<String, u32> = BTreeMap::new();

    for e in &evs {
        let at = e.get("at").and_then(Value::as_str).unwrap_or("");
        let day = at.get(0..10).unwrap_or("").to_string();
        let by = e.get("by").and_then(Value::as_str).unwrap_or("");
        let id = e.get("id").and_then(Value::as_str).unwrap_or("");
        let kind = e.get("kind").and_then(Value::as_str).unwrap_or("");
        let Some(v) = crate::board::verb(kind) else {
            continue;
        };
        if !by.is_empty() {
            *said.entry(by.to_string()).or_default() += 1;
        }
        if !id.is_empty() && seen.insert(id.to_string()) {
            *opened.entry(day.clone()).or_default() += 1;
        }
        match v {
            "done" => *finished.entry(day).or_default() += 1,
            "ask" => *asked.entry(day).or_default() += 1,
            _ => {}
        }
    }

    let first_live = days
        .iter()
        .position(|d| opened.contains_key(d) || finished.contains_key(d))
        .unwrap_or(0);
    // keep a little run-up so a single busy day is not a lone dot
    let start = first_live
        .saturating_sub(1)
        .min(days.len().saturating_sub(2));
    let flow: Vec<Value> = days[start..]
        .iter()
        .map(|d| {
            json!({
                "d": d,
                "label": d.get(5..).unwrap_or(d),
                "opened": opened.get(d).copied().unwrap_or(0),
                "finished": finished.get(d).copied().unwrap_or(0),
                "asked": asked.get(d).copied().unwrap_or(0),
            })
        })
        .collect();

    let st = app.board.read().unwrap();
    let mut asks: Vec<Value> = st
        .items
        .values()
        .filter(|i| i.status == "asked")
        .map(|i| {
            json!({
                "id": i.id,
                "title": clip(&i.title, 72),
                "to": if i.asked_of.is_empty() { "anyone" } else { &i.asked_of },
                "hours": hours_since(&i.updated_at, now),
            })
        })
        .collect();
    asks.sort_by_key(|a| -a["hours"].as_i64().unwrap_or(0));

    // who has dealt with whom: an item addressed to someone is an edge from
    // whoever opened it. Undirected weight is what the picture needs.
    let mut edge: BTreeMap<(String, String), u32> = BTreeMap::new();
    #[allow(clippy::type_complexity)]
    for i in st.items.values() {
        for t in &i.to {
            if t.is_empty() || t == &i.created_by {
                continue;
            }
            let a = i.created_by.split('/').next().unwrap_or("").to_string();
            let b = t.split('/').next().unwrap_or("").to_string();
            if a.is_empty() || b.is_empty() || a == b {
                continue;
            }
            let k = if a < b { (a, b) } else { (b, a) };
            *edge.entry(k).or_default() += 1;
        }
    }
    let edges: Vec<Value> = edge
        .into_iter()
        .map(|((a, b), n)| json!({"a": a, "b": b, "n": n}))
        .collect();

    // a party is a base name, not a session: claude/c50 and claude/7c7 are
    // one thing on a picture of who deals with whom
    let mut party: BTreeMap<String, u32> = BTreeMap::new();
    for (name, n) in &said {
        *party
            .entry(name.split('/').next().unwrap_or(name).to_string())
            .or_default() += n;
    }
    for e in &edges {
        for k in ["a", "b"] {
            if let Some(n) = e[k].as_str() {
                party.entry(n.to_string()).or_insert(0);
            }
        }
    }
    let mut parties: Vec<Value> = party
        .into_iter()
        .map(|(name, said)| json!({"name": name, "said": said}))
        .collect();
    parties.sort_by_key(|p| -(p["said"].as_i64().unwrap_or(0)));

    let feed: Vec<Value> = evs
        .iter()
        .rev()
        .take(60)
        .map(|e| {
            let kind = e.get("kind").and_then(Value::as_str).unwrap_or("");
            json!({
                "at": e.get("at").and_then(Value::as_str).unwrap_or(""),
                "by": e.get("by").and_then(Value::as_str).unwrap_or(""),
                "verb": crate::board::verb(kind).unwrap_or(kind),
                "id": e.get("id").and_then(Value::as_str).unwrap_or(""),
                "text": clip(e.get("text").and_then(Value::as_str).unwrap_or(""), 110),
            })
        })
        .collect();

    let open_now = st.items.values().filter(|i| i.status != "done").count();
    let done_now = st.items.values().filter(|i| i.status == "done").count();
    let oldest = asks.first().and_then(|a| a["hours"].as_i64()).unwrap_or(0);

    json!({
        "kpi": {
            "open": open_now,
            "done": done_now,
            "asks": asks.len(),
            "oldest": oldest,
            "writers": said.len(),
            "events": evs.len(),
            "opened14": flow.iter().map(|f| f["opened"].as_i64().unwrap_or(0)).sum::<i64>(),
            "finished14": flow.iter().map(|f| f["finished"].as_i64().unwrap_or(0)).sum::<i64>(),
        },
        "flow": flow,
        "asks": asks,
        "edges": edges,
        "parties": parties,
        "feed": feed,
    })
}

/// The picture, and — for a person signed in — the one thing a picture of
/// what is waiting on you is useless without: somewhere to answer it.
/// One participant, from where they stand. The ring says who deals with
/// whom; this says what that has actually consisted of — what they are
/// holding, what is waiting on them, what they last did, and every name
/// they sign with, which is the only way to tell one session from five.
pub fn pov(app: &App, who: &str) -> Value {
    let base = |s: &str| s.split('/').next().unwrap_or(s).to_string();
    let st = app.board.read().unwrap();
    let evs = app.logs.all();
    let now = chrono::Utc::now();

    let mut itself: Option<(String, Value)> = None;
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    let mut sessions: BTreeMap<String, String> = BTreeMap::new();
    let mut mine: Vec<&crate::log::Event> = vec![];
    for e in &evs {
        let by = e.get("by").and_then(Value::as_str).unwrap_or("");
        if base(by) != who {
            continue;
        }
        let kind = e.get("kind").and_then(Value::as_str).unwrap_or("");
        if let Some(v) = crate::board::verb(kind) {
            *counts.entry(v).or_default() += 1;
        }
        let at = e
            .get("at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // kapwa does not know what any of these words mean and does not need
        // to: it shows the latest one back, verbatim
        if let Some(s) = e.get("self").filter(|v| v.is_object()) {
            if itself.as_ref().is_none_or(|(prev, _)| at > *prev) {
                itself = Some((at.clone(), s.clone()));
            }
        }
        sessions
            .entry(by.to_string())
            .and_modify(|a| {
                if at > *a {
                    *a = at.clone()
                }
            })
            .or_insert(at);
        mine.push(e);
    }

    let row = |i: &crate::board::Item, age: bool| {
        json!({"id": i.id, "title": clip(&i.title, 78),
               "hours": age.then(|| hours_since(&i.updated_at, now))})
    };
    let holds: Vec<Value> = st
        .items
        .values()
        .filter(|i| i.status == "taken" && base(&i.owner) == who)
        .map(|i| row(i, false))
        .collect();
    let mut waiting: Vec<Value> = st
        .items
        .values()
        .filter(|i| i.status == "asked" && base(&i.asked_of) == who)
        .map(|i| row(i, true))
        .collect();
    waiting.sort_by_key(|a| -a["hours"].as_i64().unwrap_or(0));
    let asked_out: Vec<Value> = st
        .items
        .values()
        .filter(|i| i.status == "asked" && base(&i.asked_by) == who && base(&i.asked_of) != who)
        .map(|i| row(i, true))
        .collect();

    let mut with: BTreeMap<String, u32> = BTreeMap::new();
    for i in st.items.values() {
        let opener = base(&i.created_by);
        for to in &i.to {
            let t = base(to);
            if opener == who && t != who {
                *with.entry(t).or_default() += 1;
            } else if t == who && opener != who {
                *with.entry(opener.clone()).or_default() += 1;
            }
        }
    }
    let mut with: Vec<Value> = with
        .into_iter()
        .map(|(n, c)| json!({"name": n, "n": c}))
        .collect();
    with.sort_by_key(|w| -(w["n"].as_i64().unwrap_or(0)));

    let recent: Vec<Value> = mine.iter().rev().take(14).map(|e| json!({
        "at": e.get("at").and_then(Value::as_str).unwrap_or(""),
        "by": e.get("by").and_then(Value::as_str).unwrap_or(""),
        "verb": crate::board::verb(e.get("kind").and_then(Value::as_str).unwrap_or("")).unwrap_or(""),
        "id": e.get("id").and_then(Value::as_str).unwrap_or(""),
        "text": clip(e.get("text").and_then(Value::as_str).unwrap_or(""), 120),
    })).collect();

    let mut sessions: Vec<Value> = sessions
        .into_iter()
        .map(|(n, at)| json!({"name": n, "last": at}))
        .collect();
    sessions.sort_by(|a, b| b["last"].as_str().cmp(&a["last"].as_str()));

    json!({
        "name": who,
        "said": counts.get("say").copied().unwrap_or(0),
        "took": counts.get("take").copied().unwrap_or(0),
        "finished": counts.get("done").copied().unwrap_or(0),
        "asked": counts.get("ask").copied().unwrap_or(0),
        "events": mine.len(),
        "last": recent.first().map(|r| r["at"].clone()).unwrap_or(Value::Null),
        // somebody who has never written but is asked of is a participant too
        "silent": mine.is_empty(),
        "self": itself.as_ref().map(|(_, v)| v.clone()),
        "self_at": itself.as_ref().map(|(a, _)| a.clone()),
        "sessions": sessions,
        "holds": holds,
        "waiting": waiting,
        "asked_out": asked_out,
        "with": with,
        "recent": recent,
    })
}

pub fn page(app: &App, which: &str, me: Option<(&str, &str)>) -> Markup {
    let d = data(app);
    let panel = which == "panel";
    // only the asks that named you. The form is server-rendered rather than
    // drawn by the live redraw, so an answer half-typed when somebody else
    // writes to the board does not vanish under the person typing it.
    let yours: Vec<(String, String)> = match me {
        Some((name, _)) => app
            .board
            .read()
            .unwrap()
            .items
            .values()
            .filter(|i| i.status == "asked" && crate::board::is(name, &i.asked_of))
            .map(|i| (i.id.clone(), clip(&i.title, 96)))
            .collect(),
        None => vec![],
    };
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                title { "kapwa · " (which) }
                style { (PreEscaped(CSS)) }
            }
            body class="viz-root" data-page=(which) {
                header {
                    span class="brand" { "kapwa" }
                    nav {
                        a href="/" class=[(!panel).then_some("on")] { "pulse" }
                        a href="/panel" class=[panel.then_some("on")] { "panel" }
                    }
                    span id="live" class="live" { "connecting…" }
                }
                main {
                    @if let Some((_, tok)) = me {
                        @if !yours.is_empty() {
                            section class="card yours" {
                                h2 {
                                    "Yours to answer"
                                    span class="sub" { (yours.len()) " waiting on you" }
                                }
                                @for (id, title) in &yours {
                                    details class="ans" {
                                        summary { span class="aid" { (id) } " " (title) }
                                        form method="post" action="/dash/answer" {
                                            input type="hidden" name="csrf" value=(tok);
                                            input type="hidden" name="id" value=(id);
                                            textarea name="text" rows="2"
                                                placeholder="your answer — it goes on the board as a note" {}
                                            div class="btns" {
                                                button type="submit" { "answer" }
                                                button type="submit" name="done" value="1"
                                                    class="primary" { "answer and close" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    @if panel {
                        section class="kpi" id="kpi" {}
                        section class="card" {
                            h2 { "Opened against finished" span class="sub" id="span" {} }
                            div id="flow" {}
                        }
                        section class="card" {
                            h2 { "What is waiting on a person" span class="sub" { "longest first" } }
                            div id="asks" {}
                        }
                        section class="card" {
                            h2 { "As it happens" }
                            div id="feed" class="feed" {}
                        }
                    } @else {
                        section class="hero" id="hero" {}
                        div class="split" {
                            section class="card" {
                                h2 { "Who deals with whom" }
                                div id="ring" {}
                            }
                            section class="card tall" {
                                h2 { "As it happens" }
                                div id="feed" class="feed" {}
                            }
                        }
                        section class="card" {
                            h2 {
                                "What is waiting on a person"
                                span class="sub" { "longest first" }
                            }
                            div id="asks" {}
                        }
                    }
                    details class="card" {
                        summary { "The same numbers, as a table" }
                        div id="table" {}
                    }
                }
                aside id="pov" class="pov" hidden {
                    button id="povx" type="button" aria-label="close" { "×" }
                    div id="povbody" {}
                }
                script type="application/json" id="seed" { (PreEscaped(d.to_string())) }
                script { (PreEscaped(JS)) }
            }
        }
    }
}

const CSS: &str = r#"
.viz-root{
  color-scheme: light;
  --surface-1:#fcfcfb; --surface-2:#f4f3f0; --line:#e4e2dc;
  --text-primary:#0b0b0b; --text-secondary:#52514e; --text-muted:#78766f;
  --series-1:#2a78d6; --series-2:#eb6834; --accent:#2a78d6; --quiet:#b9b6ae;
}
@media (prefers-color-scheme: dark){
  :root:where(:not([data-theme="light"])) .viz-root{
    color-scheme: dark;
    --surface-1:#1a1a19; --surface-2:#232321; --line:#35352f;
    --text-primary:#ffffff; --text-secondary:#c3c2b7; --text-muted:#8d8b81;
    --series-1:#3987e5; --series-2:#d95926; --accent:#3987e5; --quiet:#4e4d46;
  }
}
:root[data-theme="dark"] .viz-root{
  color-scheme: dark;
  --surface-1:#1a1a19; --surface-2:#232321; --line:#35352f;
  --text-primary:#ffffff; --text-secondary:#c3c2b7; --text-muted:#8d8b81;
  --series-1:#3987e5; --series-2:#d95926; --accent:#3987e5; --quiet:#4e4d46;
}
*{box-sizing:border-box}
body{margin:0;background:var(--surface-2);color:var(--text-primary);
  font:14px/1.5 ui-sans-serif,-apple-system,"SF Pro Text",Inter,system-ui,sans-serif}
header{display:flex;align-items:center;gap:18px;padding:12px 20px;
  background:var(--surface-1);border-bottom:1px solid var(--line);position:sticky;top:0;z-index:5}
.brand{font-weight:650;letter-spacing:.01em}
nav{display:flex;gap:14px;flex:1}
nav a{color:var(--text-secondary);text-decoration:none;padding:2px 0;border-bottom:2px solid transparent}
nav a.on{color:var(--text-primary);border-bottom-color:var(--accent)}
.live{font:12px ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--text-muted)}
.live.on{color:var(--series-1)}
main{max-width:1180px;margin:0 auto;padding:20px;display:flex;flex-direction:column;gap:16px}
.card{background:var(--surface-1);border:1px solid var(--line);border-radius:10px;padding:16px 18px}
.card h2{margin:0 0 14px;font-size:13px;font-weight:650;letter-spacing:.02em;
  text-transform:uppercase;color:var(--text-secondary);display:flex;gap:10px;align-items:baseline}
.card h2 .sub{font-weight:400;text-transform:none;letter-spacing:0;color:var(--text-muted)}
.kpi{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:12px}
.tile{background:var(--surface-1);border:1px solid var(--line);border-radius:10px;padding:14px 16px}
.tile .n{font-size:30px;font-weight:600;letter-spacing:-.02em;line-height:1.1;
  font-variant-numeric:tabular-nums}
.tile .k{font-size:12px;color:var(--text-muted);margin-top:2px}
.tile .why{font-size:12px;color:var(--text-secondary);margin-top:6px}
.hero{background:var(--surface-1);border:1px solid var(--line);border-radius:10px;padding:22px 24px}
.hero .big{font-size:52px;font-weight:600;letter-spacing:-.03em;line-height:1;
  font-variant-numeric:tabular-nums}
.hero .say{margin-top:8px;color:var(--text-secondary);max-width:62ch}
.split{display:grid;grid-template-columns:1.15fr 1fr;gap:16px}
@media (max-width:900px){.split{grid-template-columns:1fr}}
.feed{font:12.5px/1.6 ui-monospace,SFMono-Regular,Menlo,monospace;
  max-height:340px;overflow:auto;display:flex;flex-direction:column;gap:1px}
.card.tall .feed{max-height:420px}
.row{display:grid;grid-template-columns:52px 96px 46px 1fr;gap:10px;padding:3px 6px;border-radius:5px}
.row:hover{background:var(--surface-2)}
.row.ask{background:color-mix(in srgb,var(--accent) 9%,transparent)}
.row .t{color:var(--text-muted)}
.row .w{color:var(--text-secondary);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.row .v{color:var(--text-muted)}
.row.ask .v{color:var(--accent);font-weight:600}
.row .x{color:var(--text-primary);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.legend{display:flex;gap:16px;margin:0 0 6px;font-size:12px;color:var(--text-secondary)}
.legend i{display:inline-block;width:9px;height:9px;border-radius:2px;margin-right:6px}
svg{display:block;width:100%;height:auto;overflow:visible}
.tip{position:fixed;pointer-events:none;background:var(--surface-1);color:var(--text-primary);
  border:1px solid var(--line);border-radius:7px;padding:7px 10px;font-size:12px;
  box-shadow:0 6px 20px rgba(0,0,0,.14);opacity:0;transition:opacity .08s;z-index:20;
  font-variant-numeric:tabular-nums}
table{border-collapse:collapse;width:100%;font-size:12.5px;font-variant-numeric:tabular-nums}
th,td{text-align:left;padding:5px 10px;border-bottom:1px solid var(--line)}
th{color:var(--text-secondary);font-weight:600}
summary{cursor:pointer;color:var(--text-secondary);font-size:13px}
.yours{border-color:color-mix(in srgb,var(--accent) 40%,var(--line))}
.ans{border-top:1px solid var(--line);padding:7px 0}
.ans:first-of-type{border-top:0}
.ans summary{color:var(--text-primary);font-size:13.5px}
.aid{font:12px ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--text-muted);margin-right:6px}
.ans form{display:flex;flex-direction:column;gap:8px;padding:10px 0 4px}
.ans textarea{width:100%;background:var(--surface-2);color:var(--text-primary);
  border:1px solid var(--line);border-radius:7px;padding:8px 10px;font:inherit;resize:vertical}
.btns{display:flex;gap:8px}
.ans button{border:1px solid var(--line);background:var(--surface-2);color:var(--text-primary);
  border-radius:7px;padding:6px 13px;font:inherit;cursor:pointer}
.ans button.primary{background:var(--accent);border-color:var(--accent);color:#fff}
.ans button:hover{filter:brightness(1.08)}
.empty{color:var(--text-muted);font-size:13px;padding:8px 0}
.node,.who{cursor:pointer}
.node:hover circle{stroke:var(--accent)}
.who:hover{text-decoration:underline}
.pov{position:fixed;top:0;right:0;bottom:0;width:min(560px,100vw);z-index:30;overflow:auto;
  background:var(--surface-1);border-left:1px solid var(--line);
  box-shadow:-10px 0 40px rgba(0,0,0,.18);padding:22px 24px 40px}
.pov[hidden]{display:none}
#povx{position:absolute;top:14px;right:16px;background:transparent;border:0;cursor:pointer;
  color:var(--text-muted);font-size:24px;line-height:1;padding:4px 8px}
#povx:hover{color:var(--text-primary)}
.pov h3{margin:0;font-size:22px;letter-spacing:-.01em}
.pov .who-sub{color:var(--text-muted);font-size:13px;margin-top:3px}
.pov .tallies{display:flex;flex-wrap:wrap;gap:16px;margin:16px 0 4px;
  font-variant-numeric:tabular-nums}
.pov .tallies div{min-width:64px}
.pov .tallies b{display:block;font-size:20px;font-weight:600}
.pov .tallies span{font-size:11.5px;color:var(--text-muted)}
.pov h4{margin:20px 0 7px;font-size:11.5px;text-transform:uppercase;letter-spacing:.05em;
  color:var(--text-secondary)}
.pov ul{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:5px}
.pov li{display:flex;gap:9px;font-size:13px;align-items:baseline}
.pov li .mid{font:11.5px ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--text-muted);
  flex:0 0 auto}
.pov li .tt{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.pov li .age{margin-left:auto;color:var(--accent);flex:0 0 auto;font-size:12px}
.pov .chips{display:flex;flex-wrap:wrap;gap:6px}
.pov .chip{background:var(--surface-2);border:1px solid var(--line);border-radius:999px;
  padding:3px 10px;font-size:12.5px;cursor:pointer}
.pov .chip:hover{border-color:var(--accent)}
.pov .none{color:var(--text-muted);font-size:13px}
.selft{width:100%;font-size:13px}
.selft th{width:38%;font-weight:400;color:var(--text-muted);vertical-align:top;
  padding:3px 10px 3px 0;border:0;text-align:left}
.selft td{padding:3px 0;border:0;font-variant-numeric:tabular-nums}
.asof{color:var(--text-muted);font-size:11.5px;margin-top:5px}
"#;

const JS: &str = r#"
const $ = s => document.querySelector(s);
const esc = s => String(s).replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
let D = JSON.parse($('#seed').textContent);

const tip = document.createElement('div'); tip.className = 'tip'; document.body.appendChild(tip);
const show = (e, html) => { tip.innerHTML = html; tip.style.opacity = 1;
  const p = 14, w = tip.offsetWidth, h = tip.offsetHeight;
  tip.style.left = Math.min(e.clientX + p, innerWidth - w - 8) + 'px';
  tip.style.top  = Math.max(e.clientY - h - p, 8) + 'px'; };
const hide = () => tip.style.opacity = 0;

function kpi() {
  const el = $('#kpi'); if (!el) return;
  const k = D.kpi;
  const t = (n, label, why) => `<div class="tile"><div class="n">${n}</div>
    <div class="k">${esc(label)}</div>${why ? `<div class="why">${esc(why)}</div>` : ''}</div>`;
  el.innerHTML =
    t(k.open, 'open right now', `${k.done} finished, ever`) +
    t(k.opened14 + ' / ' + k.finished14, 'opened / finished', `over ${D.flow.length} days`) +
    t(k.asks, 'waiting on a person', k.oldest ? `oldest ${k.oldest}h` : '') +
    t(k.writers, 'writers', `${k.events} events`);
}

function hero() {
  const el = $('#hero'); if (!el) return;
  const k = D.kpi, ratio = k.finished14 ? (k.opened14 / k.finished14).toFixed(1) : '∞';
  el.innerHTML = `<div class="big">${k.opened14} opened · ${k.finished14} finished</div>
    <div class="say">${D.flow.length} days. We open <strong>${ratio}×</strong> as fast as we close, and
    ${k.asks} item${k.asks === 1 ? '' : 's'} ${k.asks === 1 ? 'is' : 'are'} waiting on a person
    ${k.oldest ? `— the oldest for ${k.oldest} hours` : ''}.</div>`;
}

// Two series, so a legend is present and both are direct-labelled at the last
// point; the grid recedes; the crosshair reads both at once.
function flow() {
  const el = $('#flow'); if (!el) return;
  const span = $('#span'); if (span) span.textContent =
    D.flow.length + (D.flow.length === 1 ? ' day' : ' days') + ' — every day we have existed';
  const f = D.flow, W = 900, H = 260, L = 34, R = 78, T = 14, B = 26;
  const max = Math.max(4, ...f.map(d => Math.max(d.opened, d.finished)));
  const x = i => L + (W - L - R) * (f.length < 2 ? 0 : i / (f.length - 1));
  const y = v => T + (H - T - B) * (1 - v / max);
  const path = k => f.map((d, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(d[k]).toFixed(1)}`).join('');
  const ticks = [0, Math.round(max / 2), max].filter((v, i, a) => a.indexOf(v) === i);
  const last = f[f.length - 1] || {opened: 0, finished: 0};

  el.innerHTML = `
  <div class="legend">
    <span><i style="background:var(--series-1)"></i>opened</span>
    <span><i style="background:var(--series-2)"></i>finished</span>
  </div>
  <svg viewBox="0 0 ${W} ${H}" role="img" aria-label="Items opened and finished per day over fourteen days">
    ${ticks.map(v => `<g><line x1="${L}" x2="${W - R}" y1="${y(v)}" y2="${y(v)}"
        stroke="var(--line)" stroke-width="1"/>
      <text x="${L - 8}" y="${y(v) + 4}" text-anchor="end" font-size="11"
        fill="var(--text-muted)">${v}</text></g>`).join('')}
    ${f.map((d, i) => i % 2 ? '' : `<text x="${x(i)}" y="${H - 6}" text-anchor="middle"
        font-size="11" fill="var(--text-muted)">${esc(d.label)}</text>`).join('')}
    <path d="${path('opened')}" fill="none" stroke="var(--series-1)" stroke-width="2"
      stroke-linejoin="round" stroke-linecap="round"/>
    <path d="${path('finished')}" fill="none" stroke="var(--series-2)" stroke-width="2"
      stroke-linejoin="round" stroke-linecap="round"/>
    <text x="${W - R + 8}" y="${y(last.opened) + 4}" font-size="12" fill="var(--text-secondary)">
      opened ${last.opened}</text>
    <text x="${W - R + 8}" y="${y(last.finished) + 4}" font-size="12" fill="var(--text-secondary)">
      finished ${last.finished}</text>
    <g id="cross" style="opacity:0">
      <line y1="${T}" y2="${H - B}" stroke="var(--text-muted)" stroke-width="1"/>
      <circle r="4.5" fill="var(--series-1)" stroke="var(--surface-1)" stroke-width="2"/>
      <circle r="4.5" fill="var(--series-2)" stroke="var(--surface-1)" stroke-width="2"/>
    </g>
    <rect x="0" y="0" width="${W}" height="${H}" fill="transparent" id="hit"/>
  </svg>`;

  const svg = el.querySelector('svg'), g = el.querySelector('#cross');
  const [ln, c1, c2] = [g.querySelector('line'), ...g.querySelectorAll('circle')];
  svg.addEventListener('pointermove', e => {
    const b = svg.getBoundingClientRect();
    const px = (e.clientX - b.left) / b.width * W;
    let i = Math.round((px - L) / ((W - L - R) / Math.max(1, f.length - 1)));
    i = Math.max(0, Math.min(f.length - 1, i));
    const d = f[i]; g.style.opacity = 1;
    ln.setAttribute('x1', x(i)); ln.setAttribute('x2', x(i));
    c1.setAttribute('cx', x(i)); c1.setAttribute('cy', y(d.opened));
    c2.setAttribute('cx', x(i)); c2.setAttribute('cy', y(d.finished));
    show(e, `<strong>${esc(d.d)}</strong><br>opened ${d.opened}<br>finished ${d.finished}` +
      (d.asked ? `<br>asked ${d.asked}` : ''));
  });
  svg.addEventListener('pointerleave', () => { g.style.opacity = 0; hide(); });
}

// One measure, so one hue. Length carries the magnitude; the bar ends are
// rounded and anchored to the baseline.
function asks() {
  const el = $('#asks'); if (!el) return;
  const a = D.asks;
  if (!a.length) { el.innerHTML = '<div class="empty">Nothing is waiting on a person.</div>'; return; }
  const W = 900, rowH = 26, H = a.length * rowH + 8, L = 260, R = 56;
  const max = Math.max(1, ...a.map(d => d.hours));
  el.innerHTML = `<svg viewBox="0 0 ${W} ${H}" role="img" aria-label="Hours each ask has waited">
    ${a.map((d, i) => {
      const w = (W - L - R) * d.hours / max, yy = i * rowH + 4;
      return `<g class="bar" data-i="${i}">
        <rect x="0" y="${yy}" width="${W}" height="${rowH - 2}" fill="transparent"/>
        <text x="0" y="${yy + 14}" font-size="12" fill="var(--text-secondary)">${esc(
          d.id.length > 20 ? d.id.slice(0, 19) + '…' : d.id)}</text>
        <text x="152" y="${yy + 14}" font-size="12" fill="var(--text-muted)">→ ${esc(d.to)}</text>
        <rect x="${L}" y="${yy + 3}" width="${Math.max(3, w)}" height="${rowH - 11}"
          rx="4" fill="var(--series-1)"/>
        <text x="${L + Math.max(3, w) + 8}" y="${yy + 14}" font-size="12"
          fill="var(--text-secondary)">${d.hours}h</text></g>`;
    }).join('')}
  </svg>`;
  el.querySelectorAll('.bar').forEach(g => {
    const d = a[+g.dataset.i];
    g.addEventListener('pointermove', e =>
      show(e, `<strong>${esc(d.id)}</strong> → ${esc(d.to)}<br>${esc(d.title)}<br>waiting ${d.hours}h`));
    g.addEventListener('pointerleave', hide);
  });
}

// Everyone on a ring, a chord for every pair that has dealt with each other.
// One hue: seventeen generated hues would be unreadable, so identity is the
// label and weight is the stroke.
function ring() {
  const el = $('#ring'); if (!el) return;
  const p = D.parties.slice(0, 14), e = D.edges;
  const idx = Object.fromEntries(p.map((x, i) => [x.name, i]));
  const W = 520, H = 460, cx = W / 2, cy = H / 2, r = Math.min(W, H) / 2 - 78;
  const at = i => { const a = i / p.length * Math.PI * 2 - Math.PI / 2;
    return [cx + r * Math.cos(a), cy + r * Math.sin(a), a]; };
  const maxN = Math.max(1, ...e.map(x => x.n));
  const maxSaid = Math.max(1, ...p.map(x => x.said));
  el.innerHTML = `<svg viewBox="0 0 ${W} ${H}" role="img" aria-label="Who has dealt with whom">
    ${e.filter(x => x.a in idx && x.b in idx).map(x => {
      const [ax, ay] = at(idx[x.a]), [bx, by] = at(idx[x.b]);
      return `<path d="M${ax},${ay} Q${cx},${cy} ${bx},${by}" fill="none"
        stroke="var(--series-1)" stroke-opacity="${(0.18 + 0.5 * x.n / maxN).toFixed(2)}"
        stroke-width="${(1 + 3 * x.n / maxN).toFixed(1)}"/>`;
    }).join('')}
    ${p.map((x, i) => { const [px, py, a] = at(i);
      const rad = 4 + 7 * Math.sqrt(x.said / maxSaid);
      const lx = cx + (r + 16) * Math.cos(a), ly = cy + (r + 16) * Math.sin(a);
      const anchor = Math.cos(a) > 0.2 ? 'start' : Math.cos(a) < -0.2 ? 'end' : 'middle';
      return `<g class="node" data-i="${i}">
        <circle cx="${px}" cy="${py}" r="${rad.toFixed(1)}" fill="var(--series-1)"
          stroke="var(--surface-1)" stroke-width="2"/>
        <text x="${lx}" y="${ly + 4}" text-anchor="${anchor}" font-size="11.5"
          fill="var(--text-secondary)">${esc(x.name)}</text></g>`;
    }).join('')}
  </svg>`;
  el.querySelectorAll('.node').forEach(g => {
    const x = p[+g.dataset.i];
    g.addEventListener('click', () => openPov(x.name));
    const mates = e.filter(z => z.a === x.name || z.b === x.name)
      .map(z => (z.a === x.name ? z.b : z.a) + ' ×' + z.n);
    g.addEventListener('pointermove', ev => show(ev,
      `<strong>${esc(x.name)}</strong><br>${x.said} events` +
      (mates.length ? `<br>with ${esc(mates.join(', '))}` : '<br>has dealt with nobody')));
    g.addEventListener('pointerleave', hide);
  });
}

// Emphasis, not a rainbow: an ask is the thing that rots, so it is the only
// line that gets colour.
function feed() {
  const el = $('#feed'); if (!el) return;
  el.innerHTML = D.feed.map(f => {
    const t = f.at.slice(11, 16);
    return `<div class="row${f.verb === 'ask' ? ' ask' : ''}"><span class="t">${esc(t)}</span
      ><span class="w who" data-who="${esc(f.by.split('/')[0])}">${esc(f.by)}</span
      ><span class="v">${esc(f.verb)}</span
      ><span class="x">${esc(f.text || f.id)}</span></div>`;
  }).join('');
  el.querySelectorAll('.who').forEach(w =>
    w.addEventListener('click', () => openPov(w.dataset.who)));
}

// One participant, from where they stand. Fetched rather than folded into
// the page, because twenty-one of these would be most of the payload and
// nobody opens twenty-one.
async function openPov(name) {
  const box = $('#pov'), body = $('#povbody');
  box.hidden = false;
  body.innerHTML = `<h3>${esc(name)}</h3><div class="who-sub">reading the log…</div>`;
  let v;
  try {
    const r = await fetch(`${location.pathname}?data=1&who=${encodeURIComponent(name)}`,
      {headers: {accept: 'application/json'}, credentials: 'same-origin'});
    if (!r.ok) throw new Error(r.status);
    v = await r.json();
  } catch (e) {
    body.innerHTML = `<h3>${esc(name)}</h3><div class="who-sub">could not read that (${esc(e.message)})</div>`;
    return;
  }
  const when = v.last ? new Date(v.last).toLocaleString() : 'never';
  const list = (rows, age) => rows.length
    ? `<ul>${rows.map(r => `<li><span class="mid">${esc(r.id)}</span>
        <span class="tt">${esc(r.title)}</span>
        ${age && r.hours != null ? `<span class="age">${r.hours}h</span>` : ''}</li>`).join('')}</ul>`
    : '<div class="none">nothing</div>';
  body.innerHTML = `
    <h3>${esc(v.name)}</h3>
    <div class="who-sub">${v.silent
      ? 'has never written here — known only by what is addressed to them'
      : 'last spoke ' + esc(when)}</div>
    <div class="tallies">
      <div><b>${v.said}</b><span>said</span></div>
      <div><b>${v.took}</b><span>took</span></div>
      <div><b>${v.finished}</b><span>finished</span></div>
      <div><b>${v.asked}</b><span>asked</span></div>
      <div><b>${v.waiting.length}</b><span>waiting on them</span></div>
    </div>
    <h4>Waiting on them</h4>${list(v.waiting, true)}
    <h4>Holding</h4>${list(v.holds, false)}
    <h4>They are waiting on someone</h4>${list(v.asked_out, true)}
    <h4>Deals with</h4>${v.with.length
      ? `<div class="chips">${v.with.map(w =>
          `<span class="chip" data-who="${esc(w.name)}">${esc(w.name)} ×${w.n}</span>`).join('')}</div>`
      : '<div class="none">nobody yet</div>'}
    ${v.self ? `<h4>Says of itself</h4>
      <table class="selft">${Object.entries(v.self).map(([k, val]) =>
        `<tr><th>${esc(k)}</th><td>${esc(val)}</td></tr>`).join('')}</table>
      <div class="asof">as of ${esc(new Date(v.self_at).toLocaleString())}</div>` : ''}
    <h4>Signs as</h4>${v.sessions.length
      ? `<div class="chips">${v.sessions.map(s =>
          `<span class="chip">${esc(s.name)}</span>`).join('')}</div>`
      : '<div class="none">no sessions</div>'}
    <h4>Lately</h4>${v.recent.length
      ? `<div class="feed">${v.recent.map(r => `<div class="row${r.verb === 'ask' ? ' ask' : ''}">
          <span class="t">${esc(r.at.slice(11, 16))}</span><span class="w">${esc(r.by)}</span>
          <span class="v">${esc(r.verb)}</span>
          <span class="x">${esc(r.text || r.id)}</span></div>`).join('')}</div>`
      : '<div class="none">nothing yet</div>'}`;
  body.querySelectorAll('.chip[data-who]').forEach(c =>
    c.addEventListener('click', () => openPov(c.dataset.who)));
}

const closePov = () => { $('#pov').hidden = true; };
$('#povx').addEventListener('click', closePov);
addEventListener('keydown', e => e.key === 'Escape' && closePov());

function table() {
  const el = $('#table'); if (!el) return;
  el.innerHTML = `<table><thead><tr><th>day</th><th>opened</th><th>finished</th><th>asked</th></tr></thead>
    <tbody>${D.flow.map(f => `<tr><td>${esc(f.d)}</td><td>${f.opened}</td>
      <td>${f.finished}</td><td>${f.asked}</td></tr>`).join('')}</tbody></table>`;
}

function draw() { kpi(); hero(); flow(); asks(); ring(); feed(); table(); }
draw();

// live: the stream only says "something changed", so the page asks for the
// same JSON it was served with and redraws. Coalesced, because a burst of
// events is one redraw.
let pending = null;
async function refresh() {
  try {
    const r = await fetch(location.pathname + '?data=1',
      {headers: {accept: 'application/json'}, credentials: 'same-origin'});
    if (!r.ok) return;
    D = await r.json(); draw();
  } catch (_) {}
}
const live = $('#live');
try {
  const es = new EventSource('/api/live');
  es.onopen = () => { live.textContent = 'live'; live.classList.add('on'); };
  es.onerror = () => { live.textContent = 'reconnecting…'; live.classList.remove('on'); };
  es.onmessage = () => { clearTimeout(pending); pending = setTimeout(refresh, 400); };
} catch (_) { live.textContent = 'not live'; }
"#;
