//! Syncs with a peer over one outbound connection: pulls every log the peer
//! holds that we are behind on, then offers the peer whatever *it* is behind
//! on. One task per peer, under a tiny supervisor.
//!
//! The second half is what lets a node that nothing can connect *to* — a
//! laptop, a phone, anything behind NAT — take full part: it reaches out to
//! a node with a stable address, and both directions ride that connection.
//! So only always-on nodes need a name.
//!
//! A peer that is down is not an error to handle; it is the normal shape
//! of a machine that is asleep. So a failed pull widens the interval and
//! tries again, and nothing else in the node hears about it except the
//! status line on the board. If a pull task ever panics, the supervisor
//! notices the JoinError and starts a fresh one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::log::{now, Event};
use crate::App;

const BASE: u64 = 5_000;
const MAX: u64 = 300_000;

#[derive(Clone, Debug, Serialize, Default)]
pub struct PeerStatus {
    pub interval: u64,
    pub last_ok: Option<String>,
    pub last_error: Option<String>,
    pub pulled: usize,
    pub pushed: usize,
}

pub type Peers = Arc<Mutex<HashMap<String, PeerStatus>>>;

pub fn supervise(app: App) {
    for url in app.cfg.peers.clone() {
        app.peers.lock().unwrap().insert(
            url.clone(),
            PeerStatus {
                interval: BASE,
                ..Default::default()
            },
        );
        let app = app.clone();
        tokio::spawn(async move {
            loop {
                let handle = tokio::spawn(run(app.clone(), url.clone()));
                if let Err(e) = handle.await {
                    tracing::error!("puller {url} died ({e}); restarting");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        });
    }
}

async fn run(app: App, url: String) {
    let mut interval = BASE;
    loop {
        let outcome = pull(&app, &url).await;
        {
            let mut peers = app.peers.lock().unwrap();
            let st = peers.entry(url.clone()).or_default();
            match outcome {
                Ok((pulled, pushed)) => {
                    interval = BASE;
                    st.last_ok = Some(now_s());
                    st.last_error = None;
                    st.pulled += pulled;
                    st.pushed += pushed;
                }
                Err(e) => {
                    interval = (interval * 2).min(MAX);
                    tracing::debug!("pull {url}: {e}");
                    st.last_error = Some(format!("{} {e}", now_s()));
                }
            }
            st.interval = interval;
        }
        tokio::time::sleep(Duration::from_millis(interval)).await;
    }
}

async fn pull(app: &App, url: &str) -> Result<(usize, usize), String> {
    let theirs: HashMap<String, u64> = get(app, &format!("{url}/api/writers")).await?;
    let me = &app.cfg.writer;
    let mut n = 0;
    for (w, remote) in &theirs {
        if w == me || !crate::log::valid_name(w) {
            continue;
        }
        let since = app.logs.last_seq(w);
        if *remote <= since {
            continue;
        }
        let events: Vec<Event> = get(app, &format!("{url}/api/log/{w}?since={since}")).await?;
        let k = app.logs.ingest(w, &events).map_err(|e| e.to_string())?;
        n += k;
    }
    if n > 0 {
        app.refresh_board();
    }
    let pushed = offer(app, url, &theirs).await;
    Ok((n, pushed))
}

/// How many lines to hand over per request; the rest go next tick.
const BATCH: usize = 500;

/// Hand the peer whatever it is behind on: my own log, and any log I mirror
/// that it lacks. The peer applies the same rule it applies to a pull — only
/// the next contiguous seq — so this is as idempotent as pulling is.
async fn offer(app: &App, url: &str, theirs: &HashMap<String, u64>) -> usize {
    let mut pushed = 0;
    for w in app.logs.writers() {
        let have = theirs.get(&w).copied().unwrap_or(0);
        if app.logs.last_seq(&w) <= have {
            continue;
        }
        let mut lines = app.logs.read(&w, have);
        lines.truncate(BATCH);
        match post(app, &format!("{url}/api/log/{w}"), &lines).await {
            Ok(n) => pushed += n,
            Err(e) => tracing::debug!("offer {w} → {url}: {e}"),
        }
    }
    pushed
}

// nodes identify to each other with the shared mesh token
async fn get<T: serde::de::DeserializeOwned>(app: &App, u: &str) -> Result<T, String> {
    let mut req = app.http.get(u).timeout(Duration::from_secs(10));
    if let Some(t) = &app.cfg.mesh_token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    resp.json::<T>()
        .await
        .map_err(|e| e.without_url().to_string())
}

async fn post(app: &App, u: &str, lines: &[Event]) -> Result<usize, String> {
    let mut req = app
        .http
        .post(u)
        .timeout(Duration::from_secs(20))
        .json(lines);
    if let Some(t) = &app.cfg.mesh_token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| e.without_url().to_string())?;
    Ok(v.get("wrote").and_then(|n| n.as_u64()).unwrap_or(0) as usize)
}

fn now_s() -> String {
    now().split('.').next().unwrap_or("").to_string() + "Z"
}
