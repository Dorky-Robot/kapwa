//! How it went: the ways of working, counted.
//!
//! Rule 8 in CLAUDE.md is "measure, do not assert". This is that rule
//! turned on the work itself. Every number here is a fold over events that
//! are already in the logs — no new verb, no new field, nothing an agent
//! has to remember to record. If a number is wrong, the fix is here, and
//! the record it was read from is untouched.
//!
//! What is worth measuring is not output. It is where work waits:
//!
//!   before taken   said, and nobody picked it up yet
//!   to finish      taken, and not done yet
//!   to answer      asked a person, and nothing moves until they speak
//!   contested      two of us reached for the same thing
//!   quiet          held by someone who has not said anything since
//!
//! The first three are durations; the last two are counts of friction.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::board::{Item, Lens, State};

/// Held, and nothing said for this long: long enough that a session has
/// probably ended without dropping what it held.
const QUIET: i64 = 120;
/// Open this long with nobody taking it: a day, so a thing said in the
/// evening is not "ignored" by breakfast.
const UNTAKEN: i64 = 60 * 24;
/// How many of any list of stuck things to name before saying "and N more".
const NAMES: usize = 5;

/// A spread of durations, in minutes. Median, not mean: one item left over
/// a weekend should not move the number that says how it usually goes.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Dist {
    pub n: usize,
    pub median: i64,
    pub worst: i64,
    pub worst_id: String,
}

impl Dist {
    fn of(mut v: Vec<(i64, String)>) -> Dist {
        v.sort_by_key(|(m, _)| *m);
        let n = v.len();
        if n == 0 {
            return Dist::default();
        }
        let (worst, worst_id) = v[n - 1].clone();
        Dist {
            n,
            median: v[n / 2].0,
            worst,
            worst_id,
        }
    }
}

/// Something that is waiting, and how long it has waited.
#[derive(Clone, Debug, Serialize)]
pub struct Stuck {
    pub id: String,
    pub title: String,
    /// who it waits on, when anyone
    pub who: String,
    pub since: String,
    pub mins: i64,
}

/// What one participant did in the window.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Hand {
    pub name: String,
    pub said: usize,
    pub took: usize,
    pub finished: usize,
    pub asked: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Metrics {
    pub days: i64,
    pub from: String,
    pub to: String,
    pub events: usize,

    // flow, inside the window
    pub opened: usize,
    pub taken: usize,
    pub finished: usize,
    pub let_go: usize,
    pub asked: usize,

    // where it waits
    pub before_taken: Dist,
    pub to_finish: Dist,
    pub to_answer: Dist,

    // friction
    pub contested: usize,
    pub handed_back: usize,

    // and how things stand now, which no window decides
    pub open_now: usize,
    pub held_now: usize,
    pub waiting_now: usize,
    pub quiet: Vec<Stuck>,
    pub unanswered: Vec<Stuck>,
    pub untaken: Vec<Stuck>,

    pub who: Vec<Hand>,
    pub topics: Vec<(String, usize)>,
}

fn when(at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Minutes from one stamp to another; `None` if either is unreadable or
/// they run backwards (two clocks that disagree must not become a metric).
fn mins(from: &str, to: &str) -> Option<i64> {
    let (a, b) = (when(from)?, when(to)?);
    let m = (b - a).num_minutes();
    (m >= 0).then_some(m)
}

/// A duration in the fewest characters that still mean something.
pub fn span(m: i64) -> String {
    match m {
        m if m < 1 => "under a minute".into(),
        m if m < 60 => format!("{m}m"),
        m if m < 60 * 24 => match (m / 60, m % 60) {
            (h, 0) => format!("{h}h"),
            (h, r) => format!("{h}h{r:02}m"),
        },
        m => match (m / 1440, (m % 1440) / 60) {
            (d, 0) => format!("{d}d"),
            (d, h) => format!("{d}d{h}h"),
        },
    }
}

fn first(i: &Item, f: impl Fn(&crate::board::Hist) -> bool) -> Option<&crate::board::Hist> {
    i.history.iter().find(|h| f(h))
}

/// A take that the fold accepted. A lost one is friction, not a start.
fn won(h: &crate::board::Hist) -> bool {
    h.verb == "take" && h.fold.is_none()
}

pub fn of(st: &State, days: i64, lens: &Lens) -> Metrics {
    let now = Utc::now();
    let from = now - chrono::Duration::days(days.max(1));
    let inside = |at: &str| when(at).is_some_and(|t| t >= from && t <= now);
    let age = |at: &str| mins(at, &now.to_rfc3339()).unwrap_or(0);

    let mut m = Metrics {
        days: days.max(1),
        from: from.to_rfc3339(),
        to: now.to_rfc3339(),
        ..Default::default()
    };
    let mut before_taken = vec![];
    let mut to_finish = vec![];
    let mut to_answer = vec![];
    let mut hands: std::collections::BTreeMap<String, Hand> = Default::default();
    let mut topic_count: std::collections::BTreeMap<String, usize> = Default::default();

    for i in st.items.values().filter(|i| lens.wanted(i)) {
        let took = first(i, won);
        let did = first(i, |h| h.verb == "done");
        let mut touched = false;

        for (n, h) in i.history.iter().enumerate() {
            if !inside(&h.at) {
                continue;
            }
            touched = true;
            m.events += 1;
            let hand = hands.entry(h.by.clone()).or_default();
            hand.name = h.by.clone();
            match h.verb.as_str() {
                "say" if n == 0 => {
                    m.opened += 1;
                    hand.said += 1;
                }
                "say" => hand.said += 1,
                "take" if won(h) => {
                    m.taken += 1;
                    hand.took += 1;
                }
                "take" => m.contested += 1,
                "drop" => m.let_go += 1,
                "done" => {
                    m.finished += 1;
                    hand.finished += 1;
                }
                "ask" => {
                    m.asked += 1;
                    hand.asked += 1;
                    // how long until somebody said the thing that freed it
                    if let Some(a) = i.history[n + 1..]
                        .iter()
                        .find(|x| x.fold.as_deref() == Some("answers the ask"))
                    {
                        if let Some(d) = mins(&h.at, &a.at) {
                            to_answer.push((d, i.id.clone()));
                        }
                    }
                }
                _ => {}
            }
        }
        if touched {
            for t in &i.topics {
                *topic_count.entry(t.clone()).or_default() += 1;
            }
        }
        // a thing said, then let go again with nothing finished: the work
        // was started and handed back, which is worth seeing on its own
        if i.history.iter().any(|h| h.verb == "drop" && inside(&h.at)) && did.is_none() {
            m.handed_back += 1;
        }
        if let Some(t) = took.filter(|t| inside(&t.at)) {
            if let Some(d) = mins(&i.created_at, &t.at) {
                before_taken.push((d, i.id.clone()));
            }
        }
        if let (Some(t), Some(d)) = (took, did.filter(|d| inside(&d.at))) {
            if let Some(v) = mins(&t.at, &d.at) {
                to_finish.push((v, i.id.clone()));
            }
        }

        // how it stands now
        let stuck = |who: &str, since: &str| Stuck {
            id: i.id.clone(),
            title: i.title.clone(),
            who: who.to_string(),
            since: since.to_string(),
            mins: age(since),
        };
        match i.status.as_str() {
            "open" => {
                m.open_now += 1;
                if took.is_none() && age(&i.created_at) >= UNTAKEN {
                    m.untaken.push(stuck("", &i.created_at));
                }
            }
            "taken" => {
                m.held_now += 1;
                if age(&i.updated_at) >= QUIET {
                    m.quiet.push(stuck(&i.owner, &i.updated_at));
                }
            }
            "asked" => {
                m.waiting_now += 1;
                let at = i
                    .history
                    .iter()
                    .rev()
                    .find(|h| h.verb == "ask")
                    .map(|h| h.at.clone())
                    .unwrap_or_else(|| i.updated_at.clone());
                let who = if i.asked_of.is_empty() {
                    "anyone"
                } else {
                    &i.asked_of
                };
                m.unanswered.push(stuck(who, &at));
            }
            _ => {}
        }
    }

    for v in [&mut m.quiet, &mut m.unanswered, &mut m.untaken] {
        v.sort_by_key(|a| std::cmp::Reverse(a.mins));
    }
    m.before_taken = Dist::of(before_taken);
    m.to_finish = Dist::of(to_finish);
    m.to_answer = Dist::of(to_answer);
    m.who = {
        let mut v: Vec<Hand> = hands.into_values().collect();
        v.sort_by_key(|h| std::cmp::Reverse(h.said + h.took + h.finished + h.asked));
        v
    };
    m.topics = {
        let mut v: Vec<(String, usize)> = topic_count.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    };
    m
}

/// The reading, as label/value lines. One source, two skins: the terminal
/// pads these into columns and the page puts them in a table, so the text
/// and the dashboard can never drift into saying different things.
/// An empty label continues the line above it.
pub fn rows(m: &Metrics) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = vec![];
    let dist = |d: &Dist, of_what: &str| -> Option<String> {
        (d.n > 0).then(|| {
            format!(
                "{} {of_what} (median of {}) · longest {} ({})",
                span(d.median),
                d.n,
                span(d.worst),
                d.worst_id
            )
        })
    };
    out.push((
        "flow",
        format!(
            "{} opened · {} taken · {} finished · {} let go · {} asked",
            m.opened, m.taken, m.finished, m.let_go, m.asked
        ),
    ));
    if let Some(v) = dist(&m.before_taken, "before anyone took it") {
        out.push(("waiting", v));
    }
    if let Some(v) = dist(&m.to_finish, "from take to done") {
        out.push(("doing", v));
    }
    if let Some(v) = dist(&m.to_answer, "to an answer") {
        out.push(("answering", v));
    }
    if m.contested > 0 || m.handed_back > 0 {
        let mut bits = vec![];
        if m.contested > 0 {
            bits.push(format!(
                "{} take{} lost a race",
                m.contested,
                if m.contested == 1 { "" } else { "s" }
            ));
        }
        if m.handed_back > 0 {
            bits.push(format!("{} taken and let go again", m.handed_back));
        }
        out.push(("friction", bits.join(" · ")));
    }
    out.push((
        "now",
        format!(
            "{} open · {} held · {} waiting on someone",
            m.open_now, m.held_now, m.waiting_now
        ),
    ));

    let mut list = |label: &'static str, v: &[Stuck], say: &dyn Fn(&Stuck) -> String| {
        for (n, s) in v.iter().take(NAMES).enumerate() {
            out.push((if n == 0 { label } else { "" }, say(s)));
        }
        if v.len() > NAMES {
            out.push(("", format!("… and {} more", v.len() - NAMES)));
        }
    };
    list("quiet", &m.quiet, &|s| {
        format!("{:<12} {} · {} since a word", s.id, s.who, span(s.mins))
    });
    list("unanswered", &m.unanswered, &|s| {
        format!("{:<12} {} · {} since the ask", s.id, s.who, span(s.mins))
    });
    list("untaken", &m.untaken, &|s| {
        format!("{:<12} open {}, nobody has taken it", s.id, span(s.mins))
    });

    for (n, h) in m.who.iter().take(NAMES + 3).enumerate() {
        let mut bits = vec![];
        for (c, w) in [
            (h.said, "said"),
            (h.took, "took"),
            (h.finished, "finished"),
            (h.asked, "asked"),
        ] {
            if c > 0 {
                bits.push(format!("{c} {w}"));
            }
        }
        out.push((
            if n == 0 { "who" } else { "" },
            format!("{:<16} {}", h.name, bits.join(" · ")),
        ));
    }
    if !m.topics.is_empty() {
        out.push((
            "topics",
            m.topics
                .iter()
                .take(8)
                .map(|(t, c)| format!("#{t} {c}"))
                .collect::<Vec<_>>()
                .join(" · "),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::fold;
    use serde_json::{json, Value};

    fn ev(kind: &str, id: &str, mins_ago: i64, by: &str, extra: Value) -> crate::log::Event {
        let at = (Utc::now() - chrono::Duration::minutes(mins_ago))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut m = json!({"kind":kind,"id":id,"at":at,"by":by,"writer":"w","seq":1})
            .as_object()
            .unwrap()
            .clone();
        if let Some(o) = extra.as_object() {
            m.extend(o.clone());
        }
        m
    }

    fn state(events: &[crate::log::Event]) -> State {
        State {
            items: fold(events),
            ..Default::default()
        }
    }

    #[test]
    fn the_durations_are_where_the_work_waited_not_how_much_there_was() {
        // said 300m ago, taken 240m ago, finished 180m ago
        let st = state(&[
            ev("say", "x", 300, "ana", json!({"text":"X","t":["roof"]})),
            ev("take", "x", 240, "ben", json!({})),
            ev("done", "x", 180, "ben", json!({})),
        ]);
        let m = of(&st, 7, &Lens::all());
        assert_eq!((m.opened, m.taken, m.finished), (1, 1, 1));
        assert_eq!((m.before_taken.n, m.before_taken.median), (1, 60));
        assert_eq!((m.to_finish.n, m.to_finish.median), (1, 60));
        assert_eq!(m.topics, [("roof".to_string(), 1)]);
        assert_eq!(m.who[0].name, "ben");
    }

    #[test]
    fn a_lost_take_is_friction_and_never_a_start() {
        let st = state(&[
            ev("say", "x", 100, "ana", json!({"text":"X"})),
            ev("take", "x", 90, "ben", json!({})),
            ev("take", "x", 80, "cal", json!({})),
        ]);
        let m = of(&st, 7, &Lens::all());
        assert_eq!((m.taken, m.contested), (1, 1));
        assert_eq!(m.before_taken.median, 10, "the winning take, not the loser");
    }

    #[test]
    fn an_ask_is_timed_to_the_word_that_freed_it_and_counted_while_it_waits() {
        let answered = [
            ev("say", "x", 200, "ana", json!({"text":"X"})),
            ev("take", "x", 190, "ben", json!({})),
            ev("ask", "x", 180, "ben", json!({"to":"ana","text":"A or B?"})),
            ev("say", "x", 150, "ana", json!({"text":"B"})),
        ];
        let m = of(&state(&answered), 7, &Lens::all());
        assert_eq!((m.asked, m.to_answer.n, m.to_answer.median), (1, 1, 30));
        assert_eq!(m.waiting_now, 0);

        // unanswered: it does not time, it accumulates
        let m = of(&state(&answered[..3]), 7, &Lens::all());
        assert_eq!((m.asked, m.to_answer.n, m.waiting_now), (1, 0, 1));
        assert_eq!(m.unanswered[0].who, "ana");
        assert!(m.unanswered[0].mins >= 180);
    }

    #[test]
    fn what_is_stuck_is_measured_from_now_and_not_from_the_window() {
        let st = state(&[
            // held, and quiet for three hours
            ev("say", "x", 400, "ana", json!({"text":"X"})),
            ev("take", "x", 200, "ben", json!({})),
            // open for two days, nobody has taken it
            ev("say", "y", 60 * 24 * 2, "ana", json!({"text":"Y"})),
            // open for ten minutes, which is not yet a problem
            ev("say", "z", 10, "ana", json!({"text":"Z"})),
        ]);
        let m = of(&st, 1, &Lens::all());
        assert_eq!((m.held_now, m.open_now), (1, 2));
        assert_eq!(m.quiet.len(), 1);
        assert_eq!(m.quiet[0].who, "ben");
        assert_eq!(
            m.untaken.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["y"],
            "ten minutes old is not untaken; two days is"
        );
        // the window bounds the flow, never the standing
        assert_eq!(m.opened, 2, "y was said two days ago, outside the window");
    }

    #[test]
    fn a_topic_narrows_every_number() {
        let st = state(&[
            ev("say", "x", 100, "ana", json!({"text":"X","t":["roof"]})),
            ev("say", "y", 100, "ana", json!({"text":"Y","t":["fence"]})),
        ]);
        assert_eq!(of(&st, 7, &Lens::under(&["roof"])).opened, 1);
        assert_eq!(of(&st, 7, &Lens::all()).opened, 2);
    }

    #[test]
    fn clocks_that_disagree_do_not_become_a_metric() {
        // taken "before" it was said: two machines, two clocks
        let st = state(&[
            ev("say", "x", 100, "ana", json!({"text":"X"})),
            ev("take", "x", 120, "ben", json!({})),
        ]);
        assert_eq!(of(&st, 7, &Lens::all()).before_taken.n, 0);
    }

    #[test]
    fn a_duration_reads_as_a_person_would_say_it() {
        assert_eq!(span(0), "under a minute");
        assert_eq!(span(45), "45m");
        assert_eq!(span(60), "1h");
        assert_eq!(span(190), "3h10m");
        assert_eq!(span(60 * 24 * 2), "2d");
        assert_eq!(span(60 * 27), "1d3h");
    }
}
