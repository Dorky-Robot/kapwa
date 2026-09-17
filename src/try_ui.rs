//! Sandbox: two ways of looking at who is talking to whom, side by side, so
//! one can be chosen by seeing it rather than by imagining it. Both draw the
//! same live data from the same fold; neither writes anything.
//!
//! Nothing here is load-bearing. When one is picked it moves to `/`, and
//! this file goes away.

use std::collections::BTreeMap;

use maud::{html, Markup, PreEscaped, DOCTYPE};

use crate::board::{base, Item};
use crate::render::{ago, feed, LIVE_JS};
use crate::App;

/// Someone who has done something here. Derived: nobody announces itself.
pub struct Party {
    pub name: String,
    pub last: String,
    pub said: usize,
    pub holds: usize,
    pub asked_of: usize,
    pub topics: Vec<String>,
}

/// A directed act: who did something that was about somebody else's work,
/// or addressed to them. This is the whole "who is talking to whom".
pub struct Edge {
    pub from: String,
    pub to: String,
    pub n: usize,
    pub last: String,
    pub kind: &'static str,
}

fn note(
    edges: &mut BTreeMap<(String, String, &'static str), (usize, String)>,
    from: &str,
    to: &str,
    kind: &'static str,
    at: &str,
) {
    if from.is_empty() || to.is_empty() || from == to {
        return;
    }
    let e = edges
        .entry((from.to_string(), to.to_string(), kind))
        .or_insert((0, String::new()));
    e.0 += 1;
    if at > e.1.as_str() {
        e.1 = at.to_string();
    }
}

/// Who spoke to whom, read out of the log. Four ways one participant's act
/// lands on another: addressing them, asking them, answering them, and
/// picking up what they wrote.
pub fn edges(app: &App) -> Vec<Edge> {
    let st = app.board.read().unwrap();
    let mut acc: BTreeMap<(String, String, &'static str), (usize, String)> = BTreeMap::new();
    for i in st.items.values() {
        let author = i.created_by.clone();
        let mut asker = String::new();
        let mut asked = String::new();
        for h in &i.history {
            match h.verb.as_str() {
                "ask" => {
                    asker = h.by.clone();
                    asked = h.to.clone().unwrap_or_default();
                    note(&mut acc, &h.by, &asked, "asks", &h.at);
                }
                "say" => {
                    if let Some(to) = &h.to {
                        note(&mut acc, &h.by, to, "tells", &h.at);
                    }
                    if h.fold.as_deref() == Some("answers the ask") {
                        note(&mut acc, &h.by, &asker, "answers", &h.at);
                        asked.clear();
                    } else if h.by != author {
                        note(&mut acc, &h.by, &author, "adds to", &h.at);
                    }
                }
                "take" | "done" => note(
                    &mut acc,
                    &h.by,
                    &author,
                    if h.verb == "take" {
                        "takes from"
                    } else {
                        "finishes for"
                    },
                    &h.at,
                ),
                _ => {}
            }
        }
    }
    let mut v: Vec<Edge> = acc
        .into_iter()
        .map(|((from, to, kind), (n, last))| Edge {
            from,
            to,
            n,
            last,
            kind,
        })
        .collect();
    v.sort_by(|a, b| b.last.cmp(&a.last));
    v
}

pub fn parties(app: &App) -> Vec<Party> {
    let st = app.board.read().unwrap();
    let mut acc: BTreeMap<String, Party> = BTreeMap::new();
    let seen = |acc: &mut BTreeMap<String, Party>, name: &str, at: &str| {
        if name.is_empty() {
            return;
        }
        let p = acc.entry(name.to_string()).or_insert_with(|| Party {
            name: name.to_string(),
            last: String::new(),
            said: 0,
            holds: 0,
            asked_of: 0,
            topics: vec![],
        });
        if at > p.last.as_str() {
            p.last = at.to_string();
        }
    };
    for i in st.items.values() {
        for h in &i.history {
            seen(&mut acc, &h.by, &h.at);
            if let Some(to) = &h.to {
                seen(&mut acc, to, &h.at);
            }
            if let Some(p) = acc.get_mut(&h.by) {
                p.said += 1;
                for t in &i.topics {
                    if !p.topics.contains(t) {
                        p.topics.push(t.clone());
                    }
                }
            }
        }
        if i.status != "done" {
            if let Some(p) = acc.get_mut(&i.owner) {
                p.holds += 1;
            }
            if i.status == "asked" {
                if let Some(p) = acc.get_mut(&i.asked_of) {
                    p.asked_of += 1;
                }
            }
        }
    }
    let mut v: Vec<Party> = acc.into_values().collect();
    v.sort_by(|a, b| b.last.cmp(&a.last));
    v
}

/// A stable colour per participant: same name, same hue, every reload and
/// every machine. Sessions of one key land near each other on the wheel.
fn hue(name: &str) -> u32 {
    let b = base(name);
    let mut h: u32 = 0;
    for c in b.bytes() {
        h = h.wrapping_mul(31).wrapping_add(c as u32);
    }
    let mut off: u32 = 0;
    if let Some(tag) = name.strip_prefix(b).and_then(|t| t.strip_prefix('/')) {
        off = tag.bytes().fold(0u32, |a, c| a.wrapping_add(c as u32)) % 24;
    }
    (h % 360 + off) % 360
}

fn dot(name: &str) -> Markup {
    html! { span class="dot" style={ "background:hsl(" (hue(name)) " 70% 55%)" } {} }
}

fn shell(title: &str, which: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                title { "kapwa · " (title) }
                style { (PreEscaped(CSS)) }
            }
            body {
                header {
                    span class="brand" { "kapwa" }
                    nav {
                        a href="/try/" class=[(which == "index").then_some("on")] { "sandbox" }
                        a href="/try/constellation" class=[(which == "c").then_some("on")] { "constellation" }
                        a href="/try/rail" class=[(which == "r").then_some("on")] { "rail" }
                        a href="/" { "the board" }
                    }
                    span id="live" class="live" { "connecting…" }
                }
                main { (body) }
                script { (PreEscaped(LIVE_JS)) }
            }
        }
    }
}

pub fn index(app: &App) -> Markup {
    let p = parties(app);
    let e = edges(app);
    shell(
        "sandbox",
        "index",
        html! {
            div class="wrap prose" {
                h1 { "Two ways of seeing who is talking to whom" }
                p { "Both are live and both draw the same fold — " (p.len()) " participants, " (e.len()) " directed pairs. Pick one and it moves to the board; the other goes away." }
                div class="cards" {
                    a class="pick" href="/try/constellation" {
                        h2 { "Constellation" }
                        p { "Everyone on a ring, an arc for every pair who have dealt with each other, thicker the more often and brighter the more recently. Answers " em { "who works with whom" } " at a glance, and shows a participant nobody talks to." }
                        p class="dim" { "Weakest at: what any one thing is about." }
                    }
                    a class="pick" href="/try/rail" {
                        h2 { "Rail" }
                        p { "One lane per participant and a single stream of acts down the middle, each drawn from its author's lane to whoever it landed on. Answers " em { "what is happening right now, and between whom" } "." }
                        p class="dim" { "Weakest at: the shape of the whole, once there are many participants." }
                    }
                }
                p class="dim" { "Neither writes anything. Chronology and the numbers belong to " code { "kapwa day" } " and " code { "kapwa stats" } ", which another session is building." }
            }
        },
    )
}

/// Everyone on a ring; an arc for every pair that has dealt with each other.
pub fn constellation(app: &App) -> Markup {
    let parties = parties(app);
    let edges = edges(app);
    let n = parties.len().max(1);
    let (w, h) = (760.0_f64, 520.0_f64);
    let (cx, cy, r) = (w / 2.0, h / 2.0 - 10.0, (h / 2.0 - 90.0).max(120.0));
    let at = |i: usize| {
        let a = (i as f64 / n as f64) * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
        (cx + r * a.cos(), cy + r * a.sin())
    };
    let idx: BTreeMap<&str, usize> = parties
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.as_str(), i))
        .collect();
    let newest = edges.first().map(|e| e.last.clone()).unwrap_or_default();

    shell(
        "constellation",
        "c",
        html! {
            div class="split" {
                section class="panel grow" {
                    div class="phead" { "who deals with whom" span class="dim" { (edges.len()) " pairs" } }
                    svg viewBox={ "0 0 " (w) " " (h) } class="cons" {
                        @for e in &edges {
                            @if let (Some(&a), Some(&b)) = (idx.get(e.from.as_str()), idx.get(e.to.as_str())) {
                                @let (x1, y1) = at(a);
                                @let (x2, y2) = at(b);
                                @let mx = (x1 + x2) / 2.0;
                                @let my = (y1 + y2) / 2.0;
                                @let bend = 0.35;
                                @let qx = cx + (mx - cx) * bend;
                                @let qy = cy + (my - cy) * bend;
                                path
                                    d={ "M" (x1) "," (y1) " Q" (qx) "," (qy) " " (x2) "," (y2) }
                                    class=[(e.last == newest).then_some("hot")]
                                    style={ "stroke:hsl(" (hue(&e.from)) " 70% 55%);stroke-width:" (1.0 + (e.n as f64).min(6.0)) ";opacity:" (0.25 + 0.6 / (1.0 + fade(&e.last))) }
                                    { title { (e.from) " " (e.kind) " " (e.to) " · " (e.n) "× · " (ago(&e.last)) " ago" } }
                            }
                        }
                        @for (i, p) in parties.iter().enumerate() {
                            @let (x, y) = at(i);
                            g class="node" {
                                circle cx=(x) cy=(y) r=(8.0 + (p.said as f64).min(10.0)) style={ "fill:hsl(" (hue(&p.name)) " 70% 55%)" } {
                                    title { (p.name) " · " (p.said) " acts · last " (ago(&p.last)) " ago" }
                                }
                                @if p.holds > 0 { circle cx=(x) cy=(y) r=(14.0 + (p.said as f64).min(10.0)) class="ring" {} }
                                text x=(x) y=(y + 30.0) text-anchor="middle" class="lbl" { (p.name) }
                            }
                        }
                    }
                    div class="legend dim" {
                        "thickness = how often · brightness = how recently · a ring = holds something now"
                    }
                }
                section class="panel side" {
                    div class="phead" { "lately" }
                    ul class="edges" {
                        @for e in edges.iter().take(14) {
                            li {
                                (dot(&e.from)) span class="who" { (e.from) }
                                span class="verb" { (e.kind) }
                                (dot(&e.to)) span class="who" { (e.to) }
                                span class="when dim" { (ago(&e.last)) }
                            }
                        }
                        @if edges.is_empty() { li class="dim" { "nobody has spoken to anybody yet." } }
                    }
                }
            }
        },
    )
}

fn fade(at: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|t| {
            ((chrono::Utc::now() - t.with_timezone(&chrono::Utc))
                .num_minutes()
                .max(0) as f64)
                / 60.0
        })
        .unwrap_or(9.0)
}

/// One lane per participant, and the acts running down the middle.
pub fn rail(app: &App) -> Markup {
    let parties = parties(app);
    let rows = feed(app, 26);
    let st = app.board.read().unwrap();
    let target: BTreeMap<String, (String, String)> = st
        .items
        .values()
        .map(|i: &Item| (i.id.clone(), (i.created_by.clone(), i.asked_of.clone())))
        .collect();

    shell(
        "rail",
        "r",
        html! {
            div class="split" {
                section class="panel lanes" {
                    div class="phead" { "who is about" span class="dim" { (parties.len()) } }
                    @for p in &parties {
                        div class="lane" style={ "border-left-color:hsl(" (hue(&p.name)) " 70% 55%)" } {
                            div class="lname" { (dot(&p.name)) (p.name) }
                            div class="lmeta dim" {
                                (ago(&p.last)) " ago · " (p.said) " acts"
                                @if p.holds > 0 { " · holds " (p.holds) }
                                @if p.asked_of > 0 { span class="wait" { " · waiting on them " (p.asked_of) } }
                            }
                            @if !p.topics.is_empty() {
                                div class="ltopics dim" { @for t in p.topics.iter().take(4) { span class="t" { "#" (t) } } }
                            }
                        }
                    }
                    @if parties.is_empty() { div class="dim" { "nobody yet." } }
                }
                section class="panel grow" {
                    div class="phead" { "the rail" span class="dim" { "newest first" } }
                    ul class="rail" {
                        @for (at, by, verb, id, text) in &rows {
                            @let to = match verb.as_str() {
                                "ask" => target.get(id).map(|t| t.1.clone()).unwrap_or_default(),
                                _ => target.get(id).map(|t| t.0.clone()).unwrap_or_default(),
                            };
                            li {
                                span class="when dim" { (ago(at)) }
                                (dot(by)) span class="who" { (by) }
                                span class={ "verb v-" (verb) } { (verb) }
                                @if !to.is_empty() && &to != by {
                                    span class="arrow dim" { "→" }
                                    (dot(&to)) span class="who" { (to) }
                                }
                                span class="id dim" { (id) }
                                span class="what" { (text) }
                            }
                        }
                        @if rows.is_empty() { li class="dim" { "nothing has happened yet." } }
                    }
                }
            }
        },
    )
}

const CSS: &str = r#"
:root{--bg:#0d0f11;--panel:#14171a;--line:#22262b;--ink:#e6e8ea;--dim:#7d858d;--ok:#3ddc97}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--ink);font:13.5px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace}
header{display:flex;gap:1.25rem;align-items:center;padding:.7rem 1rem;border-bottom:1px solid var(--line);background:#101316;position:sticky;top:0;z-index:2}
.brand{font-weight:600;letter-spacing:.04em}
nav{display:flex;gap:1rem;flex:1}nav a{color:var(--dim);text-decoration:none}nav a:hover{color:var(--ink)}nav a.on{color:var(--ink);border-bottom:1px solid var(--ok)}
.live{color:var(--dim);font-size:.8rem}#live.ok::before{content:"● ";color:var(--ok)}
main{padding:1rem}
.split{display:grid;grid-template-columns:minmax(0,1fr) 22rem;gap:1rem;align-items:start}
.panel{background:var(--panel);border:1px solid var(--line);border-radius:10px;overflow:hidden}
.phead{display:flex;justify-content:space-between;padding:.6rem .9rem;border-bottom:1px solid var(--line);text-transform:uppercase;letter-spacing:.06em;font-size:.72rem;color:var(--dim)}
.dim{color:var(--dim)}
.cons{width:100%;height:auto;display:block;padding:.5rem}
.cons path{fill:none;stroke-linecap:round}
.cons path.hot{animation:pulse 1.6s ease-out}
@keyframes pulse{from{opacity:1;stroke-width:6}to{opacity:.5}}
.cons .ring{fill:none;stroke:#fff;stroke-opacity:.25}
.cons .lbl{fill:var(--dim);font:11px ui-monospace,Menlo,monospace}
.legend{padding:.5rem .9rem;border-top:1px solid var(--line);font-size:.75rem}
ul{list-style:none;margin:0;padding:0}
.edges li,.rail li{display:flex;gap:.45rem;align-items:baseline;padding:.4rem .9rem;border-bottom:1px solid var(--line);white-space:nowrap;overflow:hidden}
.rail li:hover,.edges li:hover{background:#181c20}
.dot{display:inline-block;width:.55rem;height:.55rem;border-radius:50%;flex:none}
.who{color:var(--ink)}.verb{color:var(--dim)}.when{margin-left:auto;font-size:.75rem}
.rail .when{margin-left:0;width:2.6rem;text-align:right;flex:none}
.rail .what{color:var(--dim);overflow:hidden;text-overflow:ellipsis}
.v-take{color:#6cb6ff}.v-ask{color:#ff7b72}.v-done{color:var(--ok)}
.lanes{padding-bottom:.4rem}
.lane{padding:.5rem .9rem;border-left:3px solid var(--line);margin:.4rem .5rem}
.lname{display:flex;gap:.45rem;align-items:center}
.lmeta,.ltopics{font-size:.75rem}.wait{color:#ff7b72}.t{margin-right:.4rem}
.wrap{max-width:52rem;margin:0 auto}
.prose h1{font-size:1.2rem}.prose p{color:#c9ced3}
.cards{display:grid;grid-template-columns:1fr 1fr;gap:1rem;margin:1.5rem 0}
.pick{display:block;padding:1rem;background:var(--panel);border:1px solid var(--line);border-radius:10px;text-decoration:none;color:inherit}
.pick:hover{border-color:var(--ok)}
.pick h2{margin:0 0 .5rem;font-size:1rem}.pick p{margin:.4rem 0;font-size:.85rem}
@media (max-width:880px){.split,.cards{grid-template-columns:1fr}}
"#;
