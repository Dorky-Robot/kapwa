//! Pulls every log a peer holds — its own and its mirrors — that we are
//! behind on. One task per peer, under a tiny supervisor.
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
                Ok(n) => {
                    interval = BASE;
                    st.last_ok = Some(now_s());
                    st.last_error = None;
                    st.pulled += n;
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

async fn pull(app: &App, url: &str) -> Result<usize, String> {
    let writers: HashMap<String, u64> = get(app, &format!("{url}/api/writers")).await?;
    let me = &app.cfg.writer;
    let mut n = 0;
    for (w, remote) in writers {
        if &w == me || !crate::log::valid_name(&w) {
            continue;
        }
        let since = app.logs.last_seq(&w);
        if remote <= since {
            continue;
        }
        let events: Vec<Event> = get(app, &format!("{url}/api/log/{w}?since={since}")).await?;
        let k = app.logs.ingest(&w, &events).map_err(|e| e.to_string())?;
        n += k;
    }
    if n > 0 {
        app.refresh_board();
    }
    Ok(n)
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

fn now_s() -> String {
    now().split('.').next().unwrap_or("").to_string() + "Z"
}
