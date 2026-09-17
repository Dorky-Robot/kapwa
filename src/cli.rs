//! The client half of the one binary: a thin keyboard over the local node's
//! HTTP surface. It holds no rules of its own — two copies of the rules
//! disagree quietly — so everything it prints, the node decided.
//!
//! Made for two readers at once: a person at a terminal, and an agent that
//! ran `kapwa --help` a second ago. Text on a TTY, JSON when piped, exit
//! codes that mean something, and nothing interactive.

use std::io::IsTerminal;
use std::time::Duration;

use serde_json::{json, Value};

pub const HELP: &str = "\
kapwa — what participants owe each other

  kapwa                     the board              [--t topic]
  kapwa say \"…\"             a new item             [--t topic] [--to who] [--p P1] [--as name]
  kapwa say <id> \"…\"        a note on one
  kapwa take <id>           it's mine              [--wait]
  kapwa drop <id>           not mine anymore
  kapwa done <id> [\"…\"]     finished
  kapwa ask <id> \"…\"        someone must answer    [--to who]
  kapwa ask \"…\" --to who    …about something new

  kapwa mine                what waits on me, what I hold
  kapwa show <id>           one item and its history
  kapwa prime               what an agent should know right now
  kapwa how                 how this works, for someone new (open to anyone)
  kapwa whoami

  kapwa serve               run this machine's node
  kapwa setup claude        print the hook that primes every session

  <id>     any unique prefix will do, as with git
  --t      a topic; repeat it, or comma-separate. an item can have many
  --wait   (take) wait one sync, then answer CLAIMED · LOST to x
  --tag    sign as <name>/<tag>: one session of many (or KAPWA_TAG)
  --me     which of your keys: ~/.config/kapwa/keys/<name> (or KAPWA_ME)
  --json   force JSON; it is already the default when piped

  key      KAPWA_KEY, else ~/.config/kapwa/key
  node     KAPWA_URL, else http://127.0.0.1:3410
  exit     0 ok · 1 no · 2 usage · 3 not identified

examples
  kapwa say \"Roof leaks over the back door\" --t roof
  kapwa take 7f3 --wait
  kapwa ask 7f3 --to felix \"patch it, or replace the flashing?\"
  kapwa mine --json | jq -r '.asked[].id'
";

#[derive(Default)]
struct Args {
    pos: Vec<String>,
    t: Vec<String>,
    to: Option<String>,
    p: Option<String>,
    name: Option<String>,
    tag: Option<String>,
    me: Option<String>,
    json: bool,
    wait: bool,
    hook: bool,
}

fn parse(raw: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = raw.iter();
    while let Some(x) = it.next() {
        let mut val = |flag: &str| it.next().cloned().ok_or(format!("{flag} needs a value"));
        match x.as_str() {
            "--t" | "-t" => a.t.extend(val("--t")?.split(',').map(String::from)),
            "--to" => a.to = Some(val("--to")?),
            "--p" => a.p = Some(val("--p")?),
            "--as" => a.name = Some(val("--as")?),
            "--tag" => a.tag = Some(val("--tag")?),
            "--me" => a.me = Some(val("--me")?),
            "--json" => a.json = true,
            "--wait" => a.wait = true,
            "--hook" => a.hook = true,
            "-m" => a.pos.push(val("-m")?),
            f if f.starts_with("--") => return Err(format!("unknown flag {f}")),
            _ => a.pos.push(x.clone()),
        }
    }
    Ok(a)
}

struct Node {
    http: reqwest::Client,
    url: String,
    key: Option<String>,
    tag: Option<String>,
}

enum Fail {
    /// exit 1: the node said no, or isn't there
    No(String),
    /// exit 3: no key, or the node doesn't know it
    Who(String),
}

impl Node {
    fn new(a: &Args) -> Node {
        let env = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let url = env("KAPWA_URL").unwrap_or_else(|| {
            format!(
                "http://127.0.0.1:{}",
                env("KAPWA_PORT").unwrap_or_else(|| "3410".into())
            )
        });
        // on one OS account every key is readable by every process, so
        // which one you sign with is a choice, not a wall
        let file = match a.me.clone().or_else(|| env("KAPWA_ME")) {
            Some(me) if crate::log::valid_name(&me) => format!(".config/kapwa/keys/{me}"),
            _ => ".config/kapwa/key".to_string(),
        };
        let key = env("KAPWA_KEY").or_else(|| {
            std::fs::read_to_string(crate::config::home().join(&file))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
        Node {
            http: reqwest::Client::new(),
            url: url.trim_end_matches('/').to_string(),
            key,
            tag: a.tag.clone().or_else(|| env("KAPWA_TAG")),
        }
    }

    async fn send(&self, req: reqwest::RequestBuilder, open: bool) -> Result<String, Fail> {
        let mut req = req.timeout(Duration::from_secs(15));
        if !open {
            let Some(k) = &self.key else {
                return Err(Fail::Who(
                    "no key: set KAPWA_KEY, or put one in ~/.config/kapwa/key".into(),
                ));
            };
            req = req.bearer_auth(k);
            if let Some(t) = &self.tag {
                req = req.header("x-kapwa-tag", t);
            }
        }
        let resp = req.send().await.map_err(|_| {
            Fail::No(format!(
                "no node at {} — is `kapwa serve` running?",
                self.url
            ))
        })?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(body);
        }
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["error"].as_str().map(String::from))
            .unwrap_or(body);
        Err(if matches!(status.as_u16(), 401 | 403) {
            Fail::Who(msg)
        } else {
            Fail::No(msg)
        })
    }

    async fn get(&self, path: &str) -> Result<String, Fail> {
        self.send(
            self.http.get(format!("{}{path}", self.url)),
            path == "/api/protocol",
        )
        .await
    }

    async fn event(&self, body: Value) -> Result<Value, Fail> {
        let out = self
            .send(
                self.http
                    .post(format!("{}/api/event", self.url))
                    .json(&body),
                false,
            )
            .await?;
        serde_json::from_str(&out).map_err(|e| Fail::No(e.to_string()))
    }
}

fn scope(a: &Args) -> String {
    if a.t.is_empty() {
        String::new()
    } else {
        format!("?t={}", a.t.join(","))
    }
}

fn history(item: &Value) -> String {
    let mut out = vec![format!(
        "{}  {}\n  {} · {}{}{}",
        item["id"].as_str().unwrap_or(""),
        item["title"].as_str().unwrap_or(""),
        item["status"].as_str().unwrap_or(""),
        match item["owner"].as_str().unwrap_or("") {
            "" => "nobody holds it".to_string(),
            o => format!("held by {o}"),
        },
        match item["asked_of"].as_str().unwrap_or("") {
            "" => String::new(),
            w => format!(" · waiting on {w}"),
        },
        match item["topics"]
            .as_array()
            .map(|t| t.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .filter(|t| !t.is_empty())
        {
            Some(t) => format!(" · #{}", t.join(" #")),
            None => String::new(),
        },
    )];
    for h in item["history"].as_array().into_iter().flatten() {
        let at = h["at"].as_str().unwrap_or("");
        let mut l = format!(
            "  {}  {:<14} {:<5}",
            at.get(5..16).unwrap_or(at).replace('T', " "),
            h["by"].as_str().unwrap_or(""),
            h["verb"].as_str().unwrap_or("")
        );
        if let Some(to) = h["to"].as_str() {
            l += &format!(" → {to}");
        }
        if let Some(t) = h["text"].as_str() {
            l += &format!("  {t}");
        }
        if let Some(f) = h["fold"].as_str() {
            l += &format!("  ({f})");
        }
        out.push(l);
    }
    out.join("\n")
}

const SETUP_CLAUDE: &str = r#"{
  "hooks": {
    "SessionStart": [{
      "matcher": "startup|resume|compact",
      "hooks": [{ "type": "command", "command": "kapwa prime --hook" }]
    }]
  }
}"#;

/// Returns the process exit code.
pub async fn run(raw: Vec<String>) -> i32 {
    let mut a = match parse(&raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("kapwa: {e}\n\n{HELP}");
            return 2;
        }
    };
    // the command is the first bare word, wherever the flags sit; with none,
    // it's the board
    const COMMANDS: [&str; 12] = [
        "board", "say", "take", "drop", "done", "ask", "mine", "show", "prime", "protocol",
        "whoami", "setup",
    ];
    let cmd_owned = match a.pos.first() {
        Some(c) if COMMANDS.contains(&c.as_str()) => a.pos.remove(0),
        Some(c) => {
            eprintln!("kapwa: unknown command `{c}`\n       kapwa --help");
            return 2;
        }
        None => "board".to_string(),
    };
    let cmd = cmd_owned.as_str();
    // a SessionStart hook hands us the session on stdin: make it this
    // session's tag, so many sessions of one key are told apart
    let mut hook_tag = None;
    if cmd == "prime"
        && a.hook
        && a.tag.is_none()
        && std::env::var("KAPWA_TAG").is_err()
        && !std::io::stdin().is_terminal()
    {
        let mut buf = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
        if let Some(sid) = serde_json::from_str::<Value>(&buf)
            .ok()
            .and_then(|v| v["session_id"].as_str().map(String::from))
        {
            let tag: String = sid
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(6)
                .collect();
            if !tag.is_empty() {
                a.tag = Some(tag.clone());
                hook_tag = Some(tag);
            }
        }
    }
    let as_json = a.json || !std::io::stdout().is_terminal();
    let node = Node::new(&a);
    let usage = |m: &str| -> i32 {
        eprintln!("kapwa: {m}\n       kapwa --help");
        2
    };
    let mut body = json!({});
    if !a.t.is_empty() {
        body["t"] = json!(a.t);
    }
    if let Some(v) = &a.to {
        body["to"] = json!(v);
    }
    if let Some(v) = &a.p {
        body["p"] = json!(v);
    }

    let result: Result<i32, Fail> = async {
        match (cmd, a.pos.as_slice()) {
            ("board", []) => print!("{}", node.get(&format!("/api/board.txt{}", scope(&a))).await?),
            ("how" | "protocol", []) => print!("{}", node.get("/api/protocol").await?),
            ("whoami", []) => println!("{}", node.get("/api/whoami").await?),
            ("prime", []) => {
                let got = node.get(&format!("/api/prime.txt{}", scope(&a))).await;
                if a.hook {
                    // a hook must never break the session it starts: say
                    // nothing rather than fail
                    if let Ok(mut text) = got {
                        if let Some(tag) = &hook_tag {
                            text += &format!("\nthis session signs with --tag {tag} (add it to every kapwa command, or export KAPWA_TAG={tag})\n");
                            // some harnesses let a start hook set the session's environment
                            if let Ok(f) = std::env::var("CLAUDE_ENV_FILE") {
                                use std::io::Write;
                                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(f) {
                                    let _ = writeln!(f, "export KAPWA_TAG={tag}");
                                }
                            }
                        }
                        println!("{}", json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}}));
                    }
                    return Ok(0);
                }
                print!("{}", got?);
            }
            ("mine", []) => {
                let out = node.get(&format!("/api/mine{}", scope(&a))).await?;
                if as_json {
                    println!("{out}");
                } else {
                    print!("{}", node.get(&format!("/api/prime.txt{}", scope(&a))).await?);
                }
            }
            ("show", [id]) => {
                let out = node.get(&format!("/api/item/{id}")).await?;
                if as_json {
                    println!("{out}");
                } else {
                    println!("{}", history(&serde_json::from_str(&out).unwrap_or_default()));
                }
            }
            ("say", [text]) => {
                body["kind"] = json!("say");
                body["text"] = json!(text);
                if let Some(n) = &a.name {
                    body["id"] = json!(n);
                }
                let v = node.event(body).await?;
                done(&v, as_json, "said");
            }
            ("say", [id, text]) => {
                body["kind"] = json!("say");
                body["id"] = json!(id);
                body["text"] = json!(text);
                done(&node.event(body).await?, as_json, "noted");
            }
            ("take", [id]) => {
                body["kind"] = json!("take");
                body["id"] = json!(id);
                let v = node.event(body).await?;
                let me = v["event"]["by"].as_str().unwrap_or("").to_string();
                let full = v["id"].as_str().unwrap_or(id).to_string();
                let mut owner = v["item"]["owner"].as_str().unwrap_or("").to_string();
                if a.wait && owner == me {
                    // one sync interval: long enough for a peer's earlier
                    // take to arrive and win
                    tokio::time::sleep(Duration::from_secs(7)).await;
                    let now: Value = serde_json::from_str(&node.get(&format!("/api/item/{full}")).await?).unwrap_or_default();
                    owner = now["owner"].as_str().unwrap_or("").to_string();
                }
                let won = owner == me;
                if as_json {
                    println!("{}", json!({"id": full, "owner": owner, "yours": won, "provisional": won && !a.wait}));
                } else if won && a.wait {
                    println!("CLAIMED {full}");
                } else if won {
                    println!("took {full} (provisional for a few seconds; --wait to be sure)");
                } else {
                    println!("LOST {full} to {owner}");
                }
                return Ok(if won { 0 } else { 1 });
            }
            ("drop", [id]) | ("done", [id]) => {
                body["kind"] = json!(cmd);
                body["id"] = json!(id);
                done(&node.event(body).await?, as_json, if cmd == "drop" { "dropped" } else { "done" });
            }
            ("done", [id, text]) => {
                body["kind"] = json!("done");
                body["id"] = json!(id);
                body["text"] = json!(text);
                done(&node.event(body).await?, as_json, "done");
            }
            ("ask", [text]) => {
                // about something new: say it, then ask on it
                let made = node.event(json!({"kind":"say","text":text,"t":a.t})).await?;
                body["kind"] = json!("ask");
                body["id"] = made["id"].clone();
                body["text"] = json!(text);
                done(&node.event(body).await?, as_json, "asked");
            }
            ("ask", [id, text]) => {
                body["kind"] = json!("ask");
                body["id"] = json!(id);
                body["text"] = json!(text);
                done(&node.event(body).await?, as_json, "asked");
            }
            ("setup", [what]) if what == "claude" => {
                println!("# add to ~/.claude/settings.json (merge with any hooks you have).\n# it puts `kapwa prime` into every session at start, on resume, and\n# again after compaction, so the board survives long sessions.\n{SETUP_CLAUDE}");
            }
            ("say" | "take" | "drop" | "done" | "ask" | "show" | "setup", _) => return Ok(usage(&format!("`{cmd}` wants different arguments"))),
            (other, _) => return Ok(usage(&format!("unknown command `{other}`"))),
        }
        Ok(0)
    }
    .await;

    match result {
        Ok(code) => code,
        Err(Fail::No(m)) => {
            eprintln!("kapwa: {m}");
            1
        }
        Err(Fail::Who(m)) => {
            eprintln!("kapwa: {m}");
            3
        }
    }
}

fn done(v: &Value, as_json: bool, word: &str) {
    if as_json {
        println!(
            "{}",
            json!({"id": v["id"], "by": v["event"]["by"], "item": v["item"]})
        );
    } else {
        println!("{word} {}", v["id"].as_str().unwrap_or(""));
    }
}
