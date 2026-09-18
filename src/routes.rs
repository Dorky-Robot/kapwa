//! HTTP surface. Three audiences, three ways in (see `auth`):
//!
//!   * other nodes (mesh token)   GET /api/writers, GET + POST /api/log/:writer
//!   * agents (key from file)     POST /api/event, GET /api/prime.txt, ...
//!   * people (Pocket ID)         GET /  — the read-only board
//!
//! Open to anyone: /healthz, /api/protocol (how to use this), and sign-in.
//! Binds 127.0.0.1; a tunnel route is what makes any of it reachable.

use std::hash::{Hash, Hasher};

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{Caller, Kind, Who};
use crate::board::{clean_topic, verb, Lens};
use crate::log::Event;
use crate::{join as enrol, oidc, render, try_pulse, try_ui, App};

pub fn router(app: App) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/protocol", get(protocol))
        .route("/", get(dashboard))
        .route("/auth/login", get(oidc::login))
        .route("/auth/callback", get(oidc::callback))
        .route("/auth/logout", get(oidc::logout))
        .route("/auth/bye", get(oidc::bye))
        .route("/api/writers", get(writers))
        .route("/api/log/:writer", get(log).post(offered))
        .route("/api/event", post(event))
        .route("/api/join", post(join))
        .route("/api/invite", post(invite))
        .route("/api/rotate", post(rotate))
        .route("/api/whoami", get(whoami))
        .route("/api/prime.txt", get(prime_txt))
        .route("/api/board.txt", get(board_txt))
        .route("/api/mine", get(mine))
        .route("/api/state", get(state))
        .route("/api/item/:id", get(item))
        .route("/api/peers", get(peers))
        .route("/api/feed", get(feed))
        .route("/api/day", get(day))
        .route("/api/day.txt", get(day_txt))
        .route("/api/stats", get(stats))
        .route("/api/stats.txt", get(stats_txt))
        .route("/api/topics", get(topics_in_use))
        .route("/api/live", get(live))
        // a sandbox, until one of them is picked
        .route("/try/", get(|s, c| try_page(s, c, "index")))
        .route("/try/constellation", get(|s, c| try_page(s, c, "c")))
        .route("/try/panel", get(|s, c, q| pulse_page(s, c, q, "panel")))
        .route("/try/pulse", get(|s, c, q| pulse_page(s, c, q, "pulse")))
        .route("/try/rail", get(|s, c| try_page(s, c, "r")))
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"no route; GET / says how this works"})),
            )
        })
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(app)
}

fn text(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

fn bad(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

// ── open ───────────────────────────────────────────────────────────

async fn healthz(State(app): State<App>) -> Json<Value> {
    Json(json!({"ok": true, "writer": app.cfg.writer}))
}

/// The same instructions, at a path an agent can guess. Open on purpose: it
/// holds instructions, not content.
async fn protocol(State(app): State<App>) -> Response {
    text(render::how(&app, false))
}

// ── the board: people, read-only ───────────────────────────────────

/// The root is the front door, and what it serves depends on who knocked.
///
/// Signed in, or holding a key: the board. Otherwise the instructions — as a
/// page for a browser, as plain text for everything else. It deliberately
/// does not bounce a stranger to the identity provider: a passkey page is no
/// use to an agent, and a person who has never been here deserves to be told
/// what this is before being asked who they are.
async fn dashboard(
    State(app): State<App>,
    caller: Caller,
    headers: axum::http::HeaderMap,
) -> Response {
    if let Some(who) = &caller.0 {
        return Html(render::page(&app, who, &wide(&app, who, &None)).into_string()).into_response();
    }
    let wants_html = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"));
    if wants_html {
        Html(render::front_door(&app, app.cfg.oidc.is_some()).into_string()).into_response()
    } else {
        text(render::how(&app, false))
    }
}

// ── sync: nodes ────────────────────────────────────────────────────

async fn writers(State(app): State<App>, caller: Caller, Query(q): Query<Since>) -> Response {
    if let Err(r) = caller.allow(&[Kind::Mesh, Kind::Agent]) {
        return r;
    }
    // hold it open until something lands, so a peer hears within a round
    // trip instead of within its polling interval
    if let Some(secs) = q.wait.filter(|s| *s > 0) {
        app.changed(std::time::Duration::from_secs(secs.min(60)))
            .await;
    }
    let m: serde_json::Map<String, Value> = app
        .logs
        .writers()
        .into_iter()
        .map(|w| (w.clone(), Value::from(app.logs.last_seq(&w))))
        .collect();
    Json(Value::Object(m)).into_response()
}

#[derive(Deserialize)]
struct Since {
    since: Option<u64>,
    /// seconds to hold the request open until something changes. Waiting
    /// costs one held connection; asking again and again costs a request
    /// per interval per peer, forever.
    wait: Option<u64>,
}

async fn log(
    State(app): State<App>,
    caller: Caller,
    Path(writer): Path<String>,
    Query(q): Query<Since>,
) -> Response {
    if let Err(r) = caller.allow(&[Kind::Mesh, Kind::Agent]) {
        return r;
    }
    if !crate::log::valid_name(&writer) {
        return bad(StatusCode::BAD_REQUEST, "bad writer name");
    }
    Json(app.logs.read(&writer, q.since.unwrap_or(0))).into_response()
}

/// A peer hands us lines we are behind on: its own log, or one it mirrors.
/// Same rule as a pull — only the next contiguous seq is accepted — so a
/// node that nothing can connect to takes part by reaching out.
async fn offered(
    State(app): State<App>,
    caller: Caller,
    Path(writer): Path<String>,
    Json(lines): Json<Vec<Event>>,
) -> Response {
    if let Err(r) = caller.allow(&[Kind::Mesh]) {
        return r;
    }
    if !crate::log::valid_name(&writer) {
        return bad(StatusCode::BAD_REQUEST, "bad writer name");
    }
    match app.logs.ingest(&writer, &lines) {
        Ok(n) => {
            if n > 0 {
                app.refresh_board();
            }
            Json(json!({"ok": true, "wrote": n, "have": app.logs.last_seq(&writer)}))
                .into_response()
        }
        Err(e) => (
            StatusCode::CONFLICT,
            Json(json!({"error": e, "have": app.logs.last_seq(&writer)})),
        )
            .into_response(),
    }
}

// ── getting a key ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct Joining {
    name: Option<String>,
    /// the file only this machine's user can read
    secret: Option<String>,
    /// or an invitation somebody here minted
    invite: Option<String>,
    role: Option<String>,
    t: Option<String>,
}

/// Two ways in and no third: prove you are on the machine, or spend an
/// invitation. Refusals say which, because an agent that cannot tell "I am
/// a stranger" from "that name is taken" will retry the wrong thing.
async fn join(State(app): State<App>, Json(j): Json<Joining>) -> Response {
    let local = j
        .secret
        .as_deref()
        .is_some_and(|s| enrol::is_local(&app.cfg, s));
    let invited = (!local)
        .then(|| j.invite.as_deref().and_then(|t| enrol::redeem(&app.cfg, t)))
        .flatten();
    if !local && invited.is_none() {
        return bad(
            StatusCode::FORBIDDEN,
            "a key cannot be asked for: on this machine, send the contents of ~/.config/kapwa/enroll; \
             from anywhere else, an invitation somebody here minted. GET / says how",
        );
    }
    let name = invited
        .as_ref()
        .map(|i| i.name.clone())
        .filter(|n| !n.is_empty())
        .or(j.name)
        .unwrap_or_default();
    if !crate::log::valid_name(&name) || name.contains('/') {
        return bad(
            StatusCode::BAD_REQUEST,
            "pick a name: letters, digits, - . _ @, and no /",
        );
    }
    if enrol::taken(&app.cfg, &name) {
        // the invitation is already spent by here, which is the safe way
        // round: a name it cannot have is not a reason to hand it back
        return bad(
            StatusCode::CONFLICT,
            &format!("`{name}` is taken here; choose another"),
        );
    }
    // an invitation fixes what it grants at the moment of vouching
    let (role, topics) = match &invited {
        Some(i) => (i.role.clone(), i.topics.clone()),
        None => (
            j.role
                .filter(|r| ["worker", "lead"].contains(&r.as_str()))
                .unwrap_or_else(|| "worker".into()),
            j.t.unwrap_or_else(|| "*".into()),
        ),
    };
    match enrol::add_key(&app.cfg, &name, &role, &topics) {
        Ok(token) => {
            // say so on the board: joining is not a private act
            let how = match &invited {
                Some(_) => "on an invitation".to_string(),
                None => format!("from {}", app.cfg.writer),
            };
            let _ = app.logs.append_own(
                serde_json::from_value(json!({
                    "kind": "say",
                    "id": format!("joined-{name}"),
                    "text": format!("{name} joined {how}, as {role}, watching {topics}"),
                    "t": ["kapwa"],
                }))
                .unwrap_or_default(),
            );
            app.refresh_board();
            Json(json!({"ok": true, "name": name, "key": token, "role": role, "topics": topics}))
                .into_response()
        }
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(Deserialize)]
struct Inviting {
    name: String,
    role: Option<String>,
    t: Option<String>,
    hours: Option<i64>,
}

/// Vouching is an act with a name on it, so it needs a key, and only a
/// lead's: a worker cannot widen the circle it was let into.
async fn invite(State(app): State<App>, caller: Caller, Json(i): Json<Inviting>) -> Response {
    let who = match caller.allow(&[Kind::Agent]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    if who.role.as_deref() != Some("lead") {
        return bad(
            StatusCode::FORBIDDEN,
            "only a lead may invite; ask one to vouch for you",
        );
    }
    if !crate::log::valid_name(&i.name) || i.name.contains('/') {
        return bad(
            StatusCode::BAD_REQUEST,
            "pick a name: letters, digits, - . _ @, and no /",
        );
    }
    if enrol::taken(&app.cfg, &i.name) {
        return bad(StatusCode::CONFLICT, &format!("`{}` is taken here", i.name));
    }
    let role = i
        .role
        .filter(|r| ["worker", "lead"].contains(&r.as_str()))
        .unwrap_or_else(|| "worker".into());
    let inv = enrol::mint_invite(
        &app.cfg,
        &i.name,
        &role,
        i.t.as_deref().unwrap_or("*"),
        i.hours.unwrap_or(24),
    );
    Json(json!({"ok": true, "invite": inv.token, "name": inv.name, "role": inv.role, "topics": inv.topics, "until": inv.until, "by": who.name})).into_response()
}

#[derive(Deserialize)]
struct Rotating {
    name: Option<String>,
}

/// Replace a key that should not be trusted any more. Anyone may always
/// rotate their own — a key you suspect is a key you should be able to
/// replace without asking permission first, or nobody will do it. A lead
/// may rotate anyone's, which is what a leak needs: the holder of a
/// compromised key is often not the one who notices.
async fn rotate(State(app): State<App>, caller: Caller, Json(r): Json<Rotating>) -> Response {
    let who = match caller.allow(&[Kind::Agent]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    // the name comes from the key unless one is given, so the common case
    // — rotating your own — cannot name somebody else by accident
    let name = r.name.unwrap_or_else(|| who.name.clone());
    if name != who.name && who.role.as_deref() != Some("lead") {
        return bad(
            StatusCode::FORBIDDEN,
            "only a lead may rotate somebody else's key; your own needs no permission",
        );
    }
    match enrol::rotate_key(&app.cfg, &name) {
        Ok(token) => {
            // the board records that it happened, and never what it is
            let _ = app.logs.append_own(
                serde_json::from_value(json!({
                    "kind": "say",
                    "id": format!("rotated-{name}"),
                    "text": format!("{name}'s key was rotated by {}; the old one no longer works", who.name),
                    "t": ["kapwa"],
                }))
                .unwrap_or_default(),
            );
            app.refresh_board();
            Json(json!({"ok": true, "name": name, "key": token})).into_response()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bad(StatusCode::NOT_FOUND, &e.to_string())
        }
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ── agents ─────────────────────────────────────────────────────────

/// Which item does `given` mean? An exact id, else the one id it prefixes.
fn resolve(app: &App, given: &str) -> Result<Option<String>, String> {
    let st = app.board.read().unwrap();
    if st.items.contains_key(given) {
        return Ok(Some(given.to_string()));
    }
    let hits: Vec<&String> = st.items.keys().filter(|k| k.starts_with(given)).collect();
    match hits.len() {
        0 => Ok(None),
        1 => Ok(Some(hits[0].clone())),
        n => Err(format!(
            "`{given}` is ambiguous: {n} items start with it ({})",
            hits.iter()
                .take(4)
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// A short id for an item nobody named. Not a secret and not a proof, just
/// unlikely to collide; it gives way to the event's own id once events are
/// signed.
fn mint(app: &App, seed: &str) -> String {
    let st = app.board.read().unwrap();
    for salt in 0u32.. {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (seed, &app.cfg.writer, crate::log::now(), salt).hash(&mut h);
        let id = format!("{:05x}", h.finish() & 0xfffff);
        if !st.items.contains_key(&id) {
            return id;
        }
    }
    unreachable!()
}

async fn event(State(app): State<App>, caller: Caller, Json(mut attrs): Json<Event>) -> Response {
    let who = match caller.allow(&[Kind::Agent]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    let Some(kind) = attrs.get("kind").and_then(Value::as_str).map(String::from) else {
        return bad(
            StatusCode::BAD_REQUEST,
            "an event needs a `kind`: say · take · drop · done · ask",
        );
    };
    let given = attrs
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let id = match (given, verb(&kind)) {
        (Some(g), v) => match resolve(&app, &g) {
            Ok(Some(id)) => id,
            // a `say` may name a new item; anything else must find one
            Ok(None) if v == Some("say") || v.is_none() => g,
            Ok(None) => {
                return bad(
                    StatusCode::NOT_FOUND,
                    &format!("no item `{g}` here (yet?); `say` makes one"),
                )
            }
            Err(e) => return bad(StatusCode::CONFLICT, &e),
        },
        (None, Some("say")) => mint(
            &app,
            attrs.get("text").and_then(Value::as_str).unwrap_or(""),
        ),
        (None, _) => {
            return bad(
                StatusCode::BAD_REQUEST,
                "which item? give an `id` (a unique prefix will do)",
            )
        }
    };
    // The fence has a write side, and this is it. The clinical items that
    // started this arrived as an ordinary run of `say` from one key on
    // 2026-09-17 — no importer, just a script with a topic list — so a read
    // filter alone would leave the next import free to land the same way.
    // A key may tag a private topic only if its own line names it, which is
    // the same rule that decides what it may read.
    let shut = fence(&app, &who);
    if let Some(t) = crate::board::topics_of(&attrs)
        .into_iter()
        .find(|t| crate::board::fenced(t, &shut))
    {
        return bad(
            StatusCode::FORBIDDEN,
            &format!("#{t} is private on this node, and your key does not name it"),
        );
    }
    // A new item with no topic would land in the commons, where it is on
    // everyone's board and in nobody's. So it goes to its author's own
    // topic instead: a namespace you get by construction, and never a
    // shared one you did not choose.
    let new_item = !app.board.read().unwrap().items.contains_key(&id);
    let untagged = crate::board::topics_of(&attrs).is_empty();
    if new_item && untagged && verb(&kind) == Some("say") {
        attrs.insert("t".into(), json!([crate::board::base(&who.name)]));
    }
    attrs.insert("id".into(), Value::String(id.clone()));
    // `by` is the key's name (and session tag), never what the body claims
    attrs.insert("by".into(), Value::String(who.name.clone()));
    match app.logs.append_own(attrs) {
        Ok(event) => {
            app.refresh_board();
            let item = app.board.read().unwrap().items.get(&id).cloned();
            Json(json!({"ok": true, "id": id, "event": event, "item": item})).into_response()
        }
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn whoami(caller: Caller) -> Response {
    match caller.allow(&[Kind::Mesh, Kind::Agent, Kind::User]) {
        Ok(w) => Json(w).into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct Scope {
    /// topics, comma-separated; absent means the key's own, else everything
    t: Option<String>,
}

/// What to show this caller when they did not say. A key watches the topics
/// it was given, and always its own: whatever it writes untagged lands
/// there, so it never loses sight of its own work. A key with no topics
/// listed watches everything.
fn topics(t: &Option<String>, who: &Who) -> Vec<String> {
    match t {
        Some(t) => t.split(',').filter_map(clean_topic).collect(),
        None => {
            let mut t = who.topics.clone().unwrap_or_default();
            if !t.is_empty() {
                let own = crate::board::base(&who.name).to_string();
                if !t.contains(&own) {
                    t.push(own);
                }
            }
            t
        }
    }
}

/// Which of the node's private topics stay shut to this caller. A key lifts
/// a pattern only because its own line in the agents file names a topic that
/// matches it, so what anyone may see is decided where the keys are kept and
/// never by the request. A person signed in at the dashboard has no line, so
/// the whole fence holds for them.
fn fence(app: &App, who: &Who) -> Vec<String> {
    let held = who.topics.clone().unwrap_or_default();
    app.cfg
        .private_topics
        .iter()
        .filter(|p| !held.iter().any(|t| t.contains(p.as_str())))
        .cloned()
        .collect()
}

/// The narrow view: what involves you, under the topics you watch.
fn lens(app: &App, who: &Who, t: &Option<String>) -> Lens {
    Lens {
        topics: topics(t, who),
        fence: fence(app, who),
    }
}

/// The wide view: the whole board unless `t` narrows it, because a key's own
/// topics scope what it is primed with, not what it may look at. The fence
/// still holds — that is the whole difference between a scope and a fence.
fn wide(app: &App, who: &Who, t: &Option<String>) -> Lens {
    Lens {
        topics: t
            .as_deref()
            .map(|t| t.split(',').filter_map(clean_topic).collect())
            .unwrap_or_default(),
        fence: fence(app, who),
    }
}

/// Every topic in use, commonest first. An open vocabulary costs synonyms,
/// and this is what keeps the bill down: look before you invent a word.
async fn topics_in_use(State(app): State<App>, caller: Caller) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    let st = app.board.read().unwrap();
    let mut n: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for i in st.items.values() {
        for t in &i.topics {
            let e = n.entry(t.clone()).or_default();
            e.0 += 1;
            if i.status != "done" {
                e.1 += 1;
            }
        }
    }
    let mut rows: Vec<(String, usize, usize)> =
        n.into_iter().map(|(t, (a, o))| (t, a, o)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    text(
        rows.iter()
            .map(|(t, all, open)| format!("  {:<18} {all:>3} items, {open} open", format!("#{t}")))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
}

async fn prime_txt(State(app): State<App>, caller: Caller, Query(q): Query<Scope>) -> Response {
    match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => text(render::prime(&app, w, &lens(&app, w, &q.t))),
        Err(r) => r,
    }
}

async fn board_txt(State(app): State<App>, caller: Caller, Query(q): Query<Scope>) -> Response {
    match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => text(render::board(&app, &wide(&app, w, &q.t))),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct DayQ {
    /// `today` · `yesterday` · `2026-09-17`; absent means today
    on: Option<String>,
    t: Option<String>,
}

fn on_day(q: &DayQ) -> Result<chrono::NaiveDate, String> {
    let given = q.on.clone().unwrap_or_else(|| "today".into());
    crate::day::parse_on(&given)
        .ok_or_else(|| format!("`{given}` is not a day: today · yesterday · YYYY-MM-DD"))
}

async fn day(State(app): State<App>, caller: Caller, Query(q): Query<DayQ>) -> Response {
    let who = match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    let on = match on_day(&q) {
        Ok(d) => d,
        Err(e) => return bad(StatusCode::BAD_REQUEST, &e),
    };
    let st = app.board.read().unwrap().clone();
    let steps = crate::day::day(&st, on, &who.name, &wide(&app, &who, &q.t));
    Json(json!({"on": on.to_string(), "you": who.name, "steps": steps})).into_response()
}

async fn day_txt(State(app): State<App>, caller: Caller, Query(q): Query<DayQ>) -> Response {
    let who = match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    match on_day(&q) {
        Ok(on) => text(render::day(&app, &who, on, &wide(&app, &who, &q.t))),
        Err(e) => bad(StatusCode::BAD_REQUEST, &e),
    }
}

#[derive(Deserialize)]
struct StatsQ {
    /// how far back to count; a week unless asked otherwise
    days: Option<i64>,
    t: Option<String>,
}

async fn stats(State(app): State<App>, caller: Caller, Query(q): Query<StatsQ>) -> Response {
    let who = match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    let who = &who;
    let st = app.board.read().unwrap().clone();
    Json(crate::metrics::of(&st, q.days.unwrap_or(7), &wide(&app, who, &q.t))).into_response()
}

async fn stats_txt(State(app): State<App>, caller: Caller, Query(q): Query<StatsQ>) -> Response {
    let who = match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    let who = &who;
    text(render::stats(&app, q.days.unwrap_or(7), &wide(&app, who, &q.t)))
}

async fn mine(State(app): State<App>, caller: Caller, Query(q): Query<Scope>) -> Response {
    match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => {
            let m = render::mine(&app, w, &lens(&app, w, &q.t));
            Json(json!({"you": w.name, "asked": m.asked, "held": m.held, "said_to": m.said_to, "open": m.open})).into_response()
        }
        Err(r) => r,
    }
}

async fn state(State(app): State<App>, caller: Caller) -> Response {
    let who = match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    // the raw fold, which would otherwise be the way around every other
    // door: the fence is subtracted here too, or it is not a fence
    let lens = wide(&app, &who, &None);
    let mut st = app.board.read().unwrap().clone();
    st.items.retain(|_, i| lens.wanted(i));
    Json(st).into_response()
}

async fn item(State(app): State<App>, caller: Caller, Path(id): Path<String>) -> Response {
    let who = match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    let lens = wide(&app, &who, &None);
    match resolve(&app, &id) {
        // a fenced item is not "forbidden" but absent: saying it exists is
        // already saying more than this caller is owed
        Ok(Some(id)) => match app.board.read().unwrap().items.get(&id) {
            Some(i) if lens.wanted(i) => Json(i).into_response(),
            _ => bad(StatusCode::NOT_FOUND, "no such item"),
        },
        Ok(None) => bad(StatusCode::NOT_FOUND, "no such item"),
        Err(e) => bad(StatusCode::CONFLICT, &e),
    }
}

/// Ways of looking at the same fold, to be chosen by seeing them. Behind
/// the same door as the board: it is the same data.
#[derive(Deserialize)]
struct AsData {
    data: Option<String>,
}

/// The page, or the numbers it draws. One route, because the page refreshes
/// itself from the same URL it was served from when the live stream fires.
async fn pulse_page(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<AsData>,
    which: &'static str,
) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    if q.data.is_some() {
        return Json(try_pulse::data(&app)).into_response();
    }
    Html(try_pulse::page(&app, which).into_string()).into_response()
}

async fn try_page(State(app): State<App>, caller: Caller, which: &'static str) -> Response {
    let who = match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    let lens = &wide(&app, &who, &None);
    Html(
        match which {
            "c" => try_ui::constellation(&app, lens),
            "r" => try_ui::rail(&app, lens),
            _ => try_ui::dashboard(&app, lens),
        }
        .into_string(),
    )
    .into_response()
}

/// What just happened, newest first. An agent coming back after a while
/// wants this, not the whole board.
async fn feed(State(app): State<App>, caller: Caller, Query(q): Query<Limit>) -> Response {
    let who = match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    let rows = render::feed(
        &app,
        &wide(&app, &who, &None),
        q.limit.unwrap_or(30).min(200),
    );
    if q.format.as_deref() == Some("json") {
        let v: Vec<Value> = rows
            .into_iter()
            .map(|(at, by, verb, id, text)| json!({"at": at, "by": by, "verb": verb, "id": id, "text": text}))
            .collect();
        return Json(v).into_response();
    }
    text(
        rows.into_iter()
            .map(|(at, by, verb, id, text)| {
                format!(
                    "{:>4}  {:<18} {:<5} {:<8} {text}",
                    render::ago(&at),
                    by,
                    verb,
                    id
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
}

/// A change happened. One line per change, nothing in it: whoever cares
/// asks for what they want. Costs a held connection and no polling.
async fn live(State(app): State<App>, caller: Caller) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    let stream = async_stream::stream! {
        // say hello at once, so a client knows it is connected
        yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data("open"));
        loop {
            // a heartbeat every 20s keeps a proxy from closing a quiet stream
            let changed = app.changed(std::time::Duration::from_secs(20)).await;
            yield Ok(axum::response::sse::Event::default().data(if changed { "board" } else { "tick" }));
        }
    };
    axum::response::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

#[derive(Deserialize)]
struct Limit {
    limit: Option<usize>,
    format: Option<String>,
}

async fn peers(State(app): State<App>, caller: Caller) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    Json(app.peers.lock().unwrap().clone()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app(tmp: &std::path::Path) -> App {
        fenced_app(tmp, vec![])
    }

    /// The same node with a private topic set, so the fence is exercised by
    /// the door rather than by a unit test of its predicate.
    fn fenced_app(tmp: &std::path::Path, private: Vec<String>) -> App {
        let agents = tmp.join("agents");
        std::fs::write(
            &agents,
            "# comment\nclaude:claude-key:worker:roof,fence\nana:ana-key:lead:*\nnurse:nurse-key:worker:ward\n",
        )
        .unwrap();
        App::new(Config {
            writer: "test".into(),
            dir: tmp.join("data"),
            port: 0,
            peers: vec![],
            mesh_token: Some("mesh-secret".into()),
            agents_file: agents,
            private_topics: private,
            public_url: "http://127.0.0.1:0".into(),
            secret_key_base: None,
            oidc: None,
        })
    }

    async fn call(app: &App, req: Request<Body>) -> (StatusCode, String) {
        let resp = router(app.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&body).to_string())
    }

    fn get_req(path: &str, bearer: Option<&str>) -> Request<Body> {
        let mut b = Request::get(path);
        if let Some(t) = bearer {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(Body::empty()).unwrap()
    }

    fn post_req(path: &str, bearer: &str, body: Value) -> Request<Body> {
        Request::post(path)
            .header("authorization", format!("Bearer {bearer}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn say(app: &App, key: &str, body: Value) -> Value {
        let (st, body) = call(app, post_req("/api/event", key, body)).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    #[tokio::test]
    async fn the_manual_and_healthz_are_open_and_everything_else_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        assert_eq!(
            call(&app, get_req("/healthz", None)).await.0,
            StatusCode::OK
        );
        // the root and its alias hand a stranger the whole manual
        for p in ["/", "/api/protocol"] {
            let (st, body) = call(&app, get_req(p, None)).await;
            assert_eq!(st, StatusCode::OK, "{p}");
            assert!(
                body.contains("say") && body.contains("take") && body.contains("/api/event"),
                "{p}"
            );
            // it teaches how to get in, and never lets anyone in
            assert!(body.contains("cannot be asked for"), "{p}");
            assert!(
                body.contains("kapwa join") && body.contains("invite"),
                "{p}"
            );
        }
        for p in [
            "/api/writers",
            "/api/board.txt",
            "/api/prime.txt",
            "/api/mine",
            "/api/state",
            "/api/day.txt",
            "/api/stats.txt",
        ] {
            assert_eq!(
                call(&app, get_req(p, None)).await.0,
                StatusCode::UNAUTHORIZED,
                "{p}"
            );
        }
        assert_eq!(
            call(&app, get_req("/api/writers", Some("wrong"))).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, get_req("/api/writers", Some("mesh-secret")))
                .await
                .0,
            StatusCode::OK
        );
    }

    fn line(writer: &str, seq: u64) -> Value {
        json!({"kind":"say","id":"x","writer":writer,"seq":seq,"by":writer,
               "at":format!("2026-09-17T10:00:0{seq}.000Z")})
    }

    #[tokio::test]
    async fn a_peer_may_hand_over_lines_under_the_same_rule_as_a_pull() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let post = |tok: &str, w: &str, body: Value| post_req(&format!("/api/log/{w}"), tok, body);
        let (st, body) = call(
            &app,
            post(
                "mesh-secret",
                "leaf",
                json!([line("leaf", 1), line("leaf", 2)]),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (v["wrote"].as_u64(), v["have"].as_u64()),
            (Some(2), Some(2))
        );
        // handed over again: nothing written, nothing broken
        let (_, body) = call(
            &app,
            post(
                "mesh-secret",
                "leaf",
                json!([line("leaf", 1), line("leaf", 2)]),
            ),
        )
        .await;
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["wrote"], 0);
        // a gap is not written, and the answer says where to resume
        let (_, body) = call(&app, post("mesh-secret", "leaf", json!([line("leaf", 5)]))).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (v["wrote"].as_u64(), v["have"].as_u64()),
            (Some(0), Some(2))
        );
        // nobody hands me my own log, and an agent key is not a node
        assert_eq!(
            call(&app, post("mesh-secret", "test", json!([line("test", 1)])))
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            call(&app, post("claude-key", "leaf", json!([line("leaf", 3)])))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }

    /// The whole point of the front door being open: it explains the way in
    /// without being one. A stranger who finds the URL learns how the mesh
    /// works and still cannot join it.
    #[tokio::test]
    async fn a_stranger_cannot_ask_for_a_key_and_a_worker_cannot_vouch() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let post_open = |body: Value| {
            Request::post("/api/join")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        for body in [
            json!({"name": "intruder"}),
            json!({"name": "intruder", "secret": "guess"}),
            json!({"invite": "made-up"}),
            json!({"name": "intruder", "secret": ""}),
        ] {
            let (st, out) = call(&app, post_open(body.clone())).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{body} got in: {out}");
        }
        // a worker may not widen the circle it was let into
        let (st, _) = call(
            &app,
            post_req("/api/invite", "claude-key", json!({"name": "friend"})),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        // …and nothing was written to the agents file by any of it
        let agents = std::fs::read_to_string(&app.cfg.agents_file).unwrap();
        assert!(!agents.contains("intruder") && !agents.contains("friend"));
    }

    /// Both ways in, end to end: the machine's own secret, and a vouching.
    #[tokio::test]
    async fn a_key_comes_from_being_here_or_from_somebody_vouching() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let secret = crate::join::enroll_secret(&app.cfg);
        let post_open = |body: Value| {
            Request::post("/api/join")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };

        // on the machine
        let (st, out) = call(
            &app,
            post_open(json!({"name": "scribe", "secret": secret, "t": "roof"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        let key = v["key"].as_str().unwrap().to_string();
        assert_eq!(v["topics"], "roof");
        // the key works, and the name is now spoken for
        let (st, who) = call(&app, get_req("/api/whoami", Some(&key))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<Value>(&who).unwrap()["name"],
            "scribe"
        );
        let (st, _) = call(&app, post_open(json!({"name": "scribe", "secret": secret}))).await;
        assert_eq!(st, StatusCode::CONFLICT);

        // a lead vouches for someone who is not here
        let (st, out) = call(
            &app,
            post_req(
                "/api/invite",
                "ana-key",
                json!({"name": "bot", "role": "lead", "t": "a2p"}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{out}");
        let token = serde_json::from_str::<Value>(&out).unwrap()["invite"]
            .as_str()
            .unwrap()
            .to_string();
        let (st, out) = call(&app, post_open(json!({"invite": token}))).await;
        assert_eq!(st, StatusCode::OK, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        // the invitation decides, not the asker
        assert_eq!(
            (v["name"].as_str(), v["role"].as_str(), v["topics"].as_str()),
            (Some("bot"), Some("lead"), Some("a2p"))
        );
        // and it is good exactly once
        assert_eq!(
            call(&app, post_open(json!({"invite": token}))).await.0,
            StatusCode::FORBIDDEN
        );
        // joining is on the board
        let (_, board) = call(&app, get_req("/api/board.txt", Some("ana-key"))).await;
        assert!(
            board.contains("bot joined") && board.contains("scribe joined"),
            "{board}"
        );
    }

    /// A leaked key has to be replaceable by the person who noticed, which
    /// is not always the one holding it.
    #[tokio::test]
    async fn a_key_can_be_replaced_by_its_owner_or_by_a_lead() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());

        // your own, with no name and nobody's permission
        let (st, out) = call(&app, post_req("/api/rotate", "claude-key", json!({}))).await;
        assert_eq!(st, StatusCode::OK, "{out}");
        let fresh = serde_json::from_str::<Value>(&out).unwrap()["key"]
            .as_str()
            .unwrap()
            .to_string();
        // the old key is dead the moment it is replaced, the new one works
        let (st, _) = call(&app, get_req("/api/whoami", Some("claude-key"))).await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "the leaked key must stop working"
        );
        let (st, who) = call(&app, get_req("/api/whoami", Some(&fresh))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<Value>(&who).unwrap()["name"],
            "claude"
        );

        // a worker may not rotate somebody else's out from under them
        let (st, _) = call(
            &app,
            post_req("/api/rotate", &fresh, json!({"name": "ana"})),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        let (st, _) = call(&app, get_req("/api/whoami", Some("ana-key"))).await;
        assert_eq!(st, StatusCode::OK, "ana's key must be untouched");

        // a lead may, which is what a leak needs
        let (st, out) = call(
            &app,
            post_req("/api/rotate", "ana-key", json!({"name": "claude"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{out}");
        let (st, _) = call(&app, get_req("/api/whoami", Some(&fresh))).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);

        // a stranger, and a name that is not here
        assert_eq!(
            call(&app, post_req("/api/rotate", "nope", json!({})))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(
                &app,
                post_req("/api/rotate", "ana-key", json!({"name": "ghost"}))
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );

        // it is on the board, and the board never holds the secret
        let (_, board) = call(&app, get_req("/api/board.txt", Some("ana-key"))).await;
        assert!(board.contains("rotated"), "{board}");
        assert!(
            !board.contains(&fresh),
            "a secret must never reach the board"
        );
    }

    #[tokio::test]
    async fn the_mesh_token_syncs_but_cannot_speak() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let (st, _) = call(
            &app,
            post_req(
                "/api/event",
                "mesh-secret",
                json!({"kind":"say","text":"x"}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn say_mints_an_id_and_by_is_the_key_never_the_body() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let v = say(
            &app,
            "ana-key",
            json!({"kind":"say","text":"Roof leaks","by":"impostor","t":["roof"]}),
        )
        .await;
        let id = v["id"].as_str().unwrap().to_string();
        assert_eq!(id.len(), 5);
        assert_eq!(
            (v["event"]["by"].as_str(), v["item"]["created_by"].as_str()),
            (Some("ana"), Some("ana"))
        );
        // a unique prefix is enough to mean it
        let v = say(&app, "claude-key", json!({"kind":"take","id":&id[..3]})).await;
        assert_eq!(
            (v["id"].as_str(), v["item"]["owner"].as_str()),
            (Some(id.as_str()), Some("claude"))
        );
        // but a verb about nothing is refused, with a hint
        let (st, body) = call(
            &app,
            post_req(
                "/api/event",
                "claude-key",
                json!({"kind":"take","id":"zzzz"}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert!(body.contains("say"));
    }

    #[tokio::test]
    async fn a_tag_signs_one_session_of_a_key_and_only_ever_as_a_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let req = |tag: &str| {
            Request::post("/api/event")
                .header("authorization", "Bearer claude-key")
                .header("x-kapwa-tag", tag)
                .header("content-type", "application/json")
                .body(Body::from(json!({"kind":"say","text":"hi"}).to_string()))
                .unwrap()
        };
        let (_, body) = call(&app, req("ab12")).await;
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["event"]["by"],
            "claude/ab12"
        );
        // a tag that tries to be a name is ignored, not honoured
        let (_, body) = call(&app, req("../ana")).await;
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["event"]["by"],
            "claude"
        );
    }

    #[tokio::test]
    async fn a_private_topic_is_on_no_default_board_and_no_key_may_tag_one_uninvited() {
        let tmp = tempfile::tempdir().unwrap();
        // one word fences a family: `ward` and `ward-notes` both go behind it
        let app = fenced_app(tmp.path(), vec!["ward".into()]);
        say(&app, "nurse-key", json!({"kind":"say","id":"w","text":"a record","t":["ward-notes"]})).await;
        say(&app, "ana-key", json!({"kind":"say","id":"r","text":"the roof","t":["roof"]})).await;

        // ana watches `*` — everything, which is exactly who must not see it
        for p in ["/api/prime.txt", "/api/board.txt", "/api/state", "/api/stats.txt", "/api/day.txt"] {
            let (st, body) = call(&app, get_req(p, Some("ana-key"))).await;
            assert_eq!(st, StatusCode::OK, "{p}");
            assert!(!body.contains("a record"), "{p} still carries it:\n{body}");
        }
        // naming the topic is not how you get in: a scope narrows, never widens
        let (_, body) = call(&app, get_req("/api/board.txt?t=ward-notes", Some("ana-key"))).await;
        assert!(!body.contains("a record"), "{body}");
        // nor is knowing the id
        assert_eq!(
            call(&app, get_req("/api/item/w", Some("ana-key"))).await.0,
            StatusCode::NOT_FOUND
        );
        // the key whose line names the topic reads it, and the rest of the board too
        let (_, body) = call(&app, get_req("/api/board.txt", Some("nurse-key"))).await;
        assert!(body.contains("a record"), "{body}");
        let (_, body) = call(&app, get_req("/api/board.txt", Some("ana-key"))).await;
        assert!(body.contains("the roof"), "the fence subtracts one topic, not the board:\n{body}");

        // and the write side: the import that started this cannot happen again
        let (st, body) = call(
            &app,
            post_req("/api/event", "ana-key", json!({"kind":"say","text":"imported","t":["ward","roof"]})),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
        assert!(body.contains("private"), "{body}");
        let (st, _) = call(
            &app,
            post_req("/api/event", "nurse-key", json!({"kind":"say","text":"mine to write","t":["ward"]})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }

    #[tokio::test]
    async fn prime_is_what_involves_you_and_a_key_s_topics_scope_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        say(
            &app,
            "ana-key",
            json!({"kind":"say","id":"roofing","text":"Roof leaks","t":["roof"]}),
        )
        .await;
        say(
            &app,
            "ana-key",
            json!({"kind":"say","id":"taxes","text":"File the taxes","t":["money"]}),
        )
        .await;
        say(
            &app,
            "ana-key",
            json!({"kind":"say","id":"plan","text":"Which plan?"}),
        )
        .await;
        say(
            &app,
            "ana-key",
            json!({"kind":"ask","id":"plan","to":"claude","text":"A or B?"}),
        )
        .await;
        let (_, prime) = call(&app, get_req("/api/prime.txt", Some("claude-key"))).await;
        assert!(
            prime.contains("asked of you (1)") && prime.contains("A or B?"),
            "{prime}"
        );
        assert!(prime.contains("roofing"), "{prime}");
        assert!(
            !prime.contains("taxes"),
            "claude watches roof and fence only:\n{prime}"
        );
        // asking for a topic overrides the key's own
        let (_, prime) = call(&app, get_req("/api/prime.txt?t=money", Some("claude-key"))).await;
        assert!(prime.contains("taxes") && !prime.contains("roofing"));
        // the board is never narrowed unless asked
        let (_, board) = call(&app, get_req("/api/board.txt", Some("claude-key"))).await;
        assert!(board.contains("taxes") && board.contains("roofing"));
        let (_, mine) = call(&app, get_req("/api/mine", Some("claude-key"))).await;
        assert_eq!(
            serde_json::from_str::<Value>(&mine).unwrap()["asked"][0]["id"],
            "plan"
        );
    }

    #[tokio::test]
    async fn the_day_is_what_everyone_did_and_your_own_lines_say_you() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let v = say(
            &app,
            "ana-key",
            json!({"kind":"say","text":"Roof leaks","t":["roof"]}),
        )
        .await;
        let id = v["id"].as_str().unwrap().to_string();
        say(&app, "claude-key", json!({"kind":"take","id":&id})).await;

        let (st, body) = call(&app, get_req("/api/day.txt", Some("claude-key"))).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.contains("the day"), "{body}");
        assert!(body.contains("ana") && body.contains("opened"), "{body}");
        // the reader's own move is theirs, and says so
        assert!(body.contains("you") && body.contains("took"), "{body}");

        // a day with nothing in it says so rather than lying with an empty list
        let (_, body) = call(
            &app,
            get_req("/api/day.txt?on=2001-01-01", Some("claude-key")),
        )
        .await;
        assert!(body.contains("nothing happened on 2001-01-01"), "{body}");
        // and something that is not a day is refused, with what one looks like
        let (st, body) = call(&app, get_req("/api/day.txt?on=soon", Some("claude-key"))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(body.contains("YYYY-MM-DD"), "{body}");

        // the numbers are the same events counted: one opened, one taken
        let (st, body) = call(&app, get_req("/api/stats", Some("claude-key"))).await;
        assert_eq!(st, StatusCode::OK);
        let m: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (m["opened"].as_u64(), m["taken"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(m["held_now"], 1);
        let (_, body) = call(&app, get_req("/api/stats.txt?t=fence", Some("claude-key"))).await;
        assert!(body.contains("0 opened"), "a topic narrows it: {body}");
    }

    /// A session lives in a cookie set at `Path=/`. A removal that does not
    /// say the same path leaves that cookie in place, and the next visit is
    /// still signed in — which is exactly what happened.
    #[tokio::test]
    async fn signing_out_removes_the_session_cookie_at_the_path_it_was_set_on() {
        use axum_extra::extract::cookie::{Cookie, PrivateCookieJar};
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());

        // a session cookie, exactly as a completed sign-in leaves one
        let mut c = Cookie::new("_kapwa_session", r#"{"subject":"s","name":"someone"}"#);
        c.set_path("/");
        let jar: PrivateCookieJar = PrivateCookieJar::new(app.key.clone()).add(c);
        let set = jar.into_response();
        let sent: String = set
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
            .collect::<Vec<_>>()
            .join("; ");

        let req = Request::get("/auth/logout")
            .header("cookie", &sent)
            .body(Body::empty())
            .unwrap();
        let resp = router(app.clone()).oneshot(req).await.unwrap();
        let removal: Vec<String> = resp
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .filter(|v| v.starts_with("_kapwa_session="))
            .collect();
        assert_eq!(removal.len(), 1, "sign-out must clear the session cookie");
        let removal = &removal[0];
        assert!(
            removal.contains("Path=/"),
            "removal needs the path it was set on: {removal}"
        );
        assert!(
            removal.contains("Max-Age=0") || removal.contains("Expires=Thu, 01 Jan 1970"),
            "removal must expire it: {removal}"
        );
    }

    #[tokio::test]
    async fn signing_out_lands_somewhere_that_does_not_sign_you_back_in() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let resp = router(app.clone())
            .oneshot(get_req("/auth/logout", None))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(resp.headers()["location"], "/auth/bye");
        let (st, body) = call(&app, get_req("/auth/bye", None)).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.contains("Signed out") && body.contains("/auth/login"));
    }

    #[tokio::test]
    async fn the_root_is_the_board_for_a_key_and_the_manual_for_a_stranger() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        // a key: the board
        let (st, body) = call(&app, get_req("/", Some("claude-key"))).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.contains("read-only"));
        // a browser: a page that says what this is, never a bounce to sign-in
        let req = Request::get("/")
            .header("accept", "text/html,application/xhtml+xml")
            .body(Body::empty())
            .unwrap();
        let (st, body) = call(&app, req).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.contains("<pre>") && body.contains("If you are an agent"));
        // anything else: the text itself
        let req = Request::get("/")
            .header("accept", "*/*")
            .body(Body::empty())
            .unwrap();
        let (st, body) = call(&app, req).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.starts_with("kapwa ·"));
    }
}
