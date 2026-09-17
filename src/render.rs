//! Views. Plain text for agents; one read-only HTML page for people.

use maud::{html, Markup, DOCTYPE};

use crate::auth::Who;
use crate::board::{Item, State};
use crate::puller::PeerStatus;
use crate::App;

struct Sections {
    title: String,
    logs: Vec<String>,
    peers: Vec<(String, PeerStatus)>,
    groups: Vec<(&'static str, Vec<Item>)>,
    contested: Vec<(String, String, String, String)>,
}

fn sections(app: &App) -> Sections {
    let st: State = app.board.read().unwrap().clone();
    let items: Vec<Item> = st.items.values().cloned().collect();
    let open: Vec<&Item> = items
        .iter()
        .filter(|i| !matches!(i.status.as_str(), "done" | "parked"))
        .collect();
    let by = |status: &str| -> Vec<Item> {
        let mut v: Vec<Item> = open
            .iter()
            .filter(|i| i.status == status)
            .map(|i| (*i).clone())
            .collect();
        v.sort_by(|a, b| (blank(&a.priority, "P9"), &a.id).cmp(&(blank(&b.priority, "P9"), &b.id)));
        v
    };
    let mut peers: Vec<(String, PeerStatus)> = app
        .peers
        .lock()
        .unwrap()
        .iter()
        .map(|(u, s)| (u.clone(), s.clone()))
        .collect();
    peers.sort_by(|a, b| a.0.cmp(&b.0));
    Sections {
        title: format!(
            "kapwa · {} · {} items · {} open",
            app.cfg.writer,
            items.len(),
            open.len()
        ),
        logs: st.writers.iter().map(|(w, s)| format!("{w}@{s}")).collect(),
        peers,
        groups: vec![
            ("asked", by("asked")),
            ("blockers", by("blocked")),
            ("in motion", by("claimed")),
            ("open", by("open")),
        ],
        contested: items
            .iter()
            .flat_map(|i| {
                i.history.iter().filter_map(move |h| {
                    h.fold
                        .as_ref()
                        .map(|f| (i.id.clone(), h.by.clone(), f.clone(), h.at.clone()))
                })
            })
            .collect(),
    }
}

fn peer_line(u: &str, s: &PeerStatus) -> String {
    match &s.last_error {
        Some(e) => format!("{u} down since {e}"),
        None => format!("{u} ok {}", s.last_ok.clone().unwrap_or_default()),
    }
}

pub fn board(app: &App) -> String {
    let s = sections(app);
    let mut out = vec![
        s.title.clone(),
        format!("logs {}", s.logs.join(" · ")),
        format!(
            "peers {}",
            if s.peers.is_empty() {
                "none".to_string()
            } else {
                s.peers
                    .iter()
                    .map(|(u, st)| peer_line(u, st))
                    .collect::<Vec<_>>()
                    .join(" · ")
            }
        ),
    ];
    for (label, rows) in &s.groups {
        if rows.is_empty() {
            continue;
        }
        out.push(format!("\n{label} ({})", rows.len()));
        for i in rows {
            out.push(format!("  {}", line(i)));
        }
    }
    if !s.contested.is_empty() {
        out.push(format!("\ncontested ({})", s.contested.len()));
        for (id, by, why, at) in &s.contested {
            out.push(format!("  {:<22} {by}: {why} ({at})", id));
        }
    }
    out.join("\n") + "\n"
}

fn line(i: &Item) -> String {
    format!(
        "{:<3} {:<22} {:<9} {:<11} {:<12} {}",
        i.priority,
        i.id,
        i.product,
        i.status,
        blank(&i.owner, "—"),
        i.title
    )
}

/// Read-only by construction: no forms, no scripts, refreshes itself.
pub fn page(app: &App, who: &Who) -> Markup {
    let s = sections(app);
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta http-equiv="refresh" content="15";
                title { "kapwa · " (app.cfg.writer) }
                style { (CSS) }
            }
            body {
                h1 { (s.title) }
                div class="meta" { "logs " (s.logs.join(" · ")) }
                div class="meta" {
                    "peers "
                    @if s.peers.is_empty() { "none" }
                    @for (i, (u, st)) in s.peers.iter().enumerate() {
                        @if i > 0 { " · " }
                        @match &st.last_error {
                            Some(e) => span class="down" { (u) " down since " (e) },
                            None => span class="ok" { (u) " ok " (st.last_ok.clone().unwrap_or_default()) },
                        }
                    }
                }
                @for (label, rows) in &s.groups {
                    @if !rows.is_empty() {
                        h2 { (label) " " small { (rows.len()) } }
                        table {
                            @for i in rows {
                                tr {
                                    td class="pri" { (i.priority) }
                                    td class="id" { (i.id) }
                                    td { (i.product) }
                                    td class={ "st-" (i.status) } { (i.status) }
                                    td class="who" { (blank(&i.owner, "—")) }
                                    td { (i.title) }
                                }
                            }
                        }
                    }
                }
                @if !s.contested.is_empty() {
                    h2 { "contested " small { (s.contested.len()) } }
                    table {
                        @for (id, by, why, at) in &s.contested {
                            tr { td class="id" { (id) } td { (by) } td { (why) } td class="dim" { (at) } }
                        }
                    }
                }
                footer {
                    "you: " (who.name) " (" (who.kind) ") · read-only · refreshes every 15s · "
                    a href="/auth/logout" { "sign out" }
                }
            }
        }
    }
}

fn blank<'a>(s: &'a str, d: &'a str) -> &'a str {
    if s.is_empty() {
        d
    } else {
        s
    }
}

const CSS: &str = r#"
body{font:14px/1.45 ui-monospace,SFMono-Regular,Menlo,monospace;margin:2rem auto;max-width:72rem;padding:0 1rem;color:#222;background:#fafafa}
h1{font-size:1rem;margin:0 0 .25rem} h2{font-size:.9rem;margin:1.5rem 0 .25rem;text-transform:uppercase;letter-spacing:.05em}
small{font-weight:normal;color:#888} .meta,.dim{color:#888} .ok{color:#2a7} .down{color:#c33}
table{border-collapse:collapse;width:100%} td{padding:.2rem .5rem .2rem 0;vertical-align:top;border-top:1px solid #eee}
.id{white-space:nowrap} .pri{width:2rem} .who{white-space:nowrap}
.st-blocked,.st-asked{color:#c33} .st-claimed{color:#27c} .st-open{color:#666}
footer{margin-top:2rem;color:#888;font-size:.8rem}
"#;
