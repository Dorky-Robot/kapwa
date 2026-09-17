//! The day: one chronological path through every log, read as sentences.
//!
//! The board answers "where does this stand". It cannot answer "what
//! happened today, and who did it" — that reading is across items, in the
//! order things actually occurred, and it is the one a person wants when
//! they come back to a desk other people (and other sessions) have been
//! working at.
//!
//! Another fold, not another kind of write: every step below is an event
//! already in a log. The protocol does not move for this.

use chrono::{DateTime, Local, NaiveDate};
use serde::Serialize;

use crate::board::{Item, State};

/// One thing somebody did, placed in the day.
#[derive(Clone, Debug, Serialize)]
pub struct Step {
    /// the event's own stamp, as written (UTC)
    pub at: String,
    /// the same moment where the reader is: `14:32`
    pub clock: String,
    pub by: String,
    /// say · take · drop · done · ask
    pub verb: String,
    /// how it reads: opened · wrote · answered · took · missed · let go · finished · asked
    pub word: String,
    pub id: String,
    pub title: String,
    /// what to show after the id — the words if there are any, else the title
    pub what: String,
    pub to: Option<String>,
    pub fold: Option<String>,
    pub topics: Vec<String>,
    /// the reader did this one
    pub mine: bool,
    /// how many events this line stands for: one, unless a run of them
    /// was collapsed
    pub times: usize,
    /// when this step freed an ask, who had been waiting on it. Read here,
    /// in the fold, because only the fold can see that an item has one
    /// open ask at a time — a reader walking backwards for the nearest
    /// `asked` is guessing at an invariant it cannot check.
    pub answers: Option<String>,
}

fn when(at: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|t| t.with_timezone(&Local))
}

/// `today` · `yesterday` · `2026-09-17`. Anything else is not a day.
pub fn parse_on(s: &str) -> Option<NaiveDate> {
    let today = Local::now().date_naive();
    match s.trim().to_lowercase().as_str() {
        "today" | "" => Some(today),
        "yesterday" => today.pred_opt(),
        other => NaiveDate::parse_from_str(other, "%Y-%m-%d").ok(),
    }
}

fn wanted(i: &Item, topics: &[String]) -> bool {
    topics.is_empty() || i.topics.iter().any(|t| topics.contains(t))
}

/// How one event reads. The first `say` brings an item into being, so it
/// opened it; a later one is a word on something that already exists.
fn word_for(verb: &str, first: bool, fold: Option<&str>) -> String {
    match verb {
        "say" if first => "opened",
        "say" if fold == Some("answers the ask") => "answered",
        "say" => "wrote",
        "take" if fold.is_some_and(|f| f.contains("lost")) => "missed",
        "take" => "took",
        "drop" => "let go",
        "done" => "finished",
        "ask" => "asked",
        other => return other.to_string(),
    }
    .to_string()
}

/// Every step in every item, in the order they happened: `(at, writer, seq)`,
/// the same order the board folds in.
pub fn walk(st: &State, me: &str, topics: &[String]) -> Vec<Step> {
    let mut steps: Vec<(String, String, u64, Step)> = vec![];
    for item in st.items.values().filter(|i| wanted(i, topics)) {
        // the ask still open as the history plays forward; the fold clears
        // an ask when it is answered, so there is never more than one
        let mut asked_by: Option<String> = None;
        for (n, h) in item.history.iter().enumerate() {
            let word = word_for(&h.verb, n == 0, h.fold.as_deref());
            let answers = match word.as_str() {
                "asked" => {
                    asked_by = Some(h.by.clone());
                    None
                }
                "answered" => asked_by.take(),
                _ => None,
            };
            let text = h.text.clone().filter(|t| !t.is_empty());
            let what = match (word.as_str(), &text) {
                ("wrote" | "answered" | "asked" | "finished", Some(t)) => t.clone(),
                _ => item.title.clone(),
            };
            steps.push((
                h.at.clone(),
                h.writer.clone(),
                h.seq,
                Step {
                    at: h.at.clone(),
                    clock: when(&h.at)
                        .map(|t| t.format("%H:%M").to_string())
                        .unwrap_or_default(),
                    by: h.by.clone(),
                    verb: h.verb.clone(),
                    word,
                    id: item.id.clone(),
                    title: item.title.clone(),
                    what,
                    to: h.to.clone(),
                    fold: h.fold.clone(),
                    topics: item.topics.clone(),
                    // the exact name, not the key: two sessions of one
                    // key are two hands, and telling them apart is the
                    // whole point of marking your own
                    mine: h.by == me,
                    times: 1,
                    answers,
                },
            ));
        }
    }
    steps.sort_by(|a, b| (&a.0, &a.1, a.2).cmp(&(&b.0, &b.1, b.2)));
    steps.into_iter().map(|(_, _, _, s)| s).collect()
}

/// One person writing several notes on one item, one after another, is one
/// move in a day, not six. Collapse those runs — an import, or a session
/// catching a record up, otherwise drowns everything else that happened.
/// The events are untouched; only the reading is.
pub fn collapse(steps: Vec<Step>) -> Vec<Step> {
    let mut out: Vec<Step> = vec![];
    for s in steps {
        match out.last_mut() {
            Some(p) if p.by == s.by && p.id == s.id && p.word == s.word => {
                p.times += 1;
                // the last word on it is the one that still stands
                p.what = s.what;
                p.answers = s.answers;
                p.at = s.at;
                p.clock = s.clock;
            }
            _ => out.push(s),
        }
    }
    out
}

/// The path through one day, in the reader's own timezone. A day is where
/// the person is, not where the clock says UTC is: work done at eight in
/// the evening belongs to that evening.
pub fn day(st: &State, on: NaiveDate, me: &str, topics: &[String]) -> Vec<Step> {
    collapse(
        walk(st, me, topics)
            .into_iter()
            .filter(|s| when(&s.at).is_some_and(|t| t.date_naive() == on))
            .collect(),
    )
}

/// Today — unless nothing has happened yet today, and then the last day
/// that did have something. A board opened first thing in the morning
/// should show the day it is catching you up on, not a blank.
pub fn latest(st: &State, me: &str, topics: &[String]) -> (NaiveDate, Vec<Step>) {
    let all = walk(st, me, topics);
    let today = Local::now().date_naive();
    let on = all
        .iter()
        .rev()
        .find_map(|s| when(&s.at))
        .map(|t| t.date_naive().min(today))
        .unwrap_or(today);
    let steps = collapse(
        all.into_iter()
            .filter(|s| when(&s.at).is_some_and(|t| t.date_naive() == on))
            .collect(),
    );
    (on, steps)
}

/// Everyone who did something, in the order they first appear.
pub fn hands(steps: &[Step]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for s in steps {
        if !out.contains(&s.by) {
            out.push(s.by.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::fold;
    use serde_json::{json, Value};

    fn ev(kind: &str, id: &str, at: &str, by: &str, extra: Value) -> crate::log::Event {
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

    /// The times are written so the local day is the same wherever this
    /// runs: midday UTC is the same date in every timezone on earth.
    fn at(day: u32, hour: u32) -> String {
        format!("2026-09-{day:02}T{hour:02}:00:00.000Z")
    }

    #[test]
    fn the_path_is_one_line_per_event_across_items_in_time_order() {
        let st = state(&[
            ev(
                "say",
                "x",
                &at(17, 12),
                "ana",
                json!({"text":"Roof leaks","t":["roof"]}),
            ),
            ev("say", "y", &at(17, 13), "ben", json!({"text":"Fence"})),
            ev("take", "x", &at(17, 14), "me/1", json!({})),
            ev(
                "say",
                "x",
                &at(17, 15),
                "ben",
                json!({"text":"over the back door"}),
            ),
        ]);
        let steps = day(
            &st,
            NaiveDate::from_ymd_opt(2026, 9, 17).unwrap(),
            "me/1",
            &[],
        );
        let read: Vec<(&str, &str, &str)> = steps
            .iter()
            .map(|s| (s.by.as_str(), s.word.as_str(), s.what.as_str()))
            .collect();
        assert_eq!(
            read,
            [
                ("ana", "opened", "Roof leaks"),
                ("ben", "opened", "Fence"),
                ("me/1", "took", "Roof leaks"),
                ("ben", "wrote", "over the back door"),
            ]
        );
        assert_eq!(steps.iter().filter(|s| s.mine).count(), 1);
        assert_eq!(hands(&steps), ["ana", "ben", "me/1"]);
    }

    #[test]
    fn a_day_holds_only_that_day_and_topics_narrow_it() {
        let st = state(&[
            ev(
                "say",
                "x",
                &at(16, 12),
                "ana",
                json!({"text":"Yesterday","t":["roof"]}),
            ),
            ev(
                "say",
                "y",
                &at(17, 12),
                "ana",
                json!({"text":"Today","t":["fence"]}),
            ),
        ]);
        let d17 = NaiveDate::from_ymd_opt(2026, 9, 17).unwrap();
        assert_eq!(day(&st, d17, "me", &[]).len(), 1);
        assert_eq!(day(&st, d17.pred_opt().unwrap(), "me", &[]).len(), 1);
        assert!(day(&st, d17, "me", &["fence".into()]).len() == 1);
        assert!(day(&st, d17, "me", &["roof".into()]).is_empty());
    }

    #[test]
    fn a_lost_take_and_an_answer_read_as_what_they_were() {
        let st = state(&[
            ev("say", "x", &at(17, 12), "ana", json!({"text":"X"})),
            ev("take", "x", &at(17, 13), "ana", json!({})),
            ev("take", "x", &at(17, 14), "ben", json!({})),
            ev(
                "ask",
                "x",
                &at(17, 15),
                "ana",
                json!({"to":"felix","text":"A or B?"}),
            ),
            ev("say", "x", &at(17, 16), "felix", json!({"text":"B"})),
        ]);
        let words: Vec<String> = day(
            &st,
            NaiveDate::from_ymd_opt(2026, 9, 17).unwrap(),
            "me",
            &[],
        )
        .into_iter()
        .map(|s| s.word)
        .collect();
        assert_eq!(words, ["opened", "took", "missed", "asked", "answered"]);
    }

    #[test]
    fn a_run_of_notes_on_one_item_is_one_move_in_the_day() {
        let st = state(&[
            ev("say", "x", &at(17, 12), "ana", json!({"text":"X"})),
            ev("say", "x", &at(17, 13), "ana", json!({"text":"one"})),
            ev("say", "x", &at(17, 13), "ana", json!({"text":"two"})),
            ev("say", "x", &at(17, 13), "ana", json!({"text":"three"})),
            // a different hand breaks the run, and so does a different verb
            ev("say", "x", &at(17, 14), "ben", json!({"text":"mine"})),
            ev("take", "x", &at(17, 15), "ana", json!({})),
            ev("say", "x", &at(17, 16), "ana", json!({"text":"back"})),
        ]);
        let steps = day(
            &st,
            NaiveDate::from_ymd_opt(2026, 9, 17).unwrap(),
            "me",
            &[],
        );
        let read: Vec<(&str, &str, usize)> = steps
            .iter()
            .map(|s| (s.by.as_str(), s.what.as_str(), s.times))
            .collect();
        assert_eq!(
            read,
            [
                ("ana", "X", 1),
                ("ana", "three", 3),
                ("ben", "mine", 1),
                ("ana", "X", 1),
                ("ana", "back", 1),
            ],
            "a run keeps its last word and says how many it stands for"
        );
    }

    #[test]
    fn the_step_that_freed_an_ask_names_who_was_waiting() {
        let st = state(&[
            ev("say", "x", &at(17, 12), "ana", json!({"text":"X"})),
            ev(
                "ask",
                "x",
                &at(17, 13),
                "ana",
                json!({"to":"felix","text":"A or B?"}),
            ),
            ev("say", "x", &at(17, 14), "felix", json!({"text":"B"})),
            // a second round on the same item, asked by somebody else
            ev(
                "ask",
                "x",
                &at(17, 15),
                "ben",
                json!({"to":"felix","text":"and C?"}),
            ),
            ev("say", "x", &at(17, 16), "felix", json!({"text":"no"})),
        ]);
        let steps = day(
            &st,
            NaiveDate::from_ymd_opt(2026, 9, 17).unwrap(),
            "me",
            &[],
        );
        let pairs: Vec<(&str, Option<&str>)> = steps
            .iter()
            .map(|s| (s.word.as_str(), s.answers.as_deref()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("opened", None),
                ("asked", None),
                ("answered", Some("ana")),
                ("asked", None),
                ("answered", Some("ben")),
            ],
            "each answer names the one who asked it, not the last asker on the item"
        );
    }

    #[test]
    fn a_session_of_a_key_is_not_its_sibling() {
        let st = state(&[
            ev("say", "x", &at(17, 12), "claude/aaa", json!({"text":"X"})),
            ev("say", "y", &at(17, 13), "claude/bbb", json!({"text":"Y"})),
        ]);
        let steps = day(
            &st,
            NaiveDate::from_ymd_opt(2026, 9, 17).unwrap(),
            "claude/aaa",
            &[],
        );
        assert_eq!(steps.iter().filter(|s| s.mine).count(), 1);
        assert!(steps[0].mine && !steps[1].mine);
    }

    #[test]
    fn the_latest_day_is_today_or_the_last_one_that_had_anything() {
        let st = state(&[ev(
            "say",
            "x",
            &at(16, 12),
            "ana",
            json!({"text":"Yesterday"}),
        )]);
        let (on, steps) = latest(&st, "me", &[]);
        assert_eq!(
            (on, steps.len()),
            (NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(), 1)
        );
        // nothing at all: today, empty, and no panic
        let (on, steps) = latest(&State::default(), "me", &[]);
        assert_eq!((on, steps.len()), (Local::now().date_naive(), 0));
    }

    #[test]
    fn a_day_can_be_named_in_the_words_people_use() {
        let today = Local::now().date_naive();
        assert_eq!(parse_on("today"), Some(today));
        assert_eq!(parse_on("yesterday"), today.pred_opt());
        assert_eq!(parse_on("2026-09-15"), NaiveDate::from_ymd_opt(2026, 9, 15));
        assert_eq!(parse_on("last tuesday"), None);
    }
}
