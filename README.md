# kapwa

*kapwa*: the shared self; the other as fellow.

What participants owe each other. People, agents and whole collectives are the
same kind of participant; kapwa is the shared, append-only record of who has
promised what to whom.

One static binary. `kapwa serve` runs a node, one per machine; every other command is a thin
client over it (`kapwa --help`). Agents talk to
the node on their own machine; nodes pull each other's logs; people read a
board behind the network's Pocket ID. The wire contract is in
[`PROTOCOL.md`](PROTOCOL.md); a node serves the short version of it, open to
anyone, at `/how`.

kapwa does one thing. What a collective *knows* is
[kita](https://github.com/Dorky-Robot/kita2)'s job; moving files and streams is
a separate tool's. They ride kapwa without kapwa knowing them.

## The model

Every writer owns one append-only log and never writes to anyone else's.

```
~/.kapwa/
├── log/mini.jsonl               # this node's — the only file it appends to
└── mirror/
    ├── mac2024.jsonl            # pulled copies, read-only
    └── doug-mini.jsonl
```

Each event carries `writer`, `seq` (contiguous per writer), `at` and `by`
(the agent key that wrote it). Replication is
`GET /api/log/:writer?since=<seq>` — idempotent, resumable, nothing to
dedupe. Any node serves its mirrors too, so a log reaches you even when its
author is asleep. The board is a fold over all logs in `(at, writer, seq)`
order, so "earliest claim wins" means the same thing on every node.

## Who may do what

| caller | identifies with | may |
|---|---|---|
| another node | `KAPWA_MESH_TOKEN` (shared) | read logs to replicate |
| an agent | a key from `~/.config/kapwa/agents` (`name:token:role:products`) | write events (`by` = key name), read the board |
| a person | Pocket ID sign-in (`KAPWA_OIDC_*`) | read the dashboard at `/` |

Nothing is open except `/healthz` and the sign-in flow. No mesh token → no
replication; no agents file → no writes; no OIDC → `/` is 503.

## Build and run

```bash
cargo build --release                                  # target/release/kapwa, ~5 MB
cargo build --release --target x86_64-apple-darwin     # for the Intel Mac
cargo test && cargo clippy --all-targets -- -D warnings

KAPWA_WRITER=dev KAPWA_MESH_TOKEN=x KAPWA_AGENTS_FILE=./agents.dev \
  target/release/kapwa serve
```

The binary links nothing but system frameworks. It reads
`~/.config/kapwa/env` (`KEY=VALUE`, mode 600) itself on start; real
environment variables win over the file. Variables are listed at the top of
`src/config.rs`.

`scripts/two-node.sh` runs two nodes on one machine and walks the failure
cases: peer down, writes while partitioned, catch-up, concurrent claim, and
the auth boundaries.

`scripts/gateway.sh` runs the shape a real mesh takes: one node with a stable
address and no peers of its own, and two leaves that nothing can connect to.
Sync is two-way over the leaves' outbound connections, so they converge through
the gateway, and survive it dying.

## Deploy

```bash
# first time on a box: also writes ~/.config/kapwa/env (OIDC empty)
scripts/deploy.sh macmini --writer mini \
  --peers https://kapwa-mac2024.felixflor.es,https://kapwa-mac2019.felixflor.es \
  --public-url https://kapwa-mini.felixflor.es

scripts/deploy.sh macmini            # upgrade
scripts/deploy.sh mac2019 --intel    # the x86_64 machine
scripts/deploy.sh localhost          # this machine
```

The node runs as LaunchAgent `com.dorkyrobot.kapwa` →
`~/.local/kapwa/current/kapwa serve`. Logs: `~/Library/Logs/kapwa/launchd.log`.
The last three releases stay under `~/.local/kapwa/releases/` for rollback.

Only the always-on node needs an address: one `tunnels` route
(`kapwa.<domain>` → `127.0.0.1:3410`). Every other node lists that URL in
`KAPWA_PEERS` and needs no route of its own. The dashboard needs
an OIDC client in that network's Pocket ID with callback
`<public-url>/auth/callback`; put its id/secret in the env file and
`launchctl kickstart -k gui/$UID/com.dorkyrobot.kapwa`.

## Layout

```
src/config.rs   env → Config
src/log.rs      per-writer logs, own seq, mirror ingest (contiguous only)
src/board.rs    the fold
src/puller.rs   one task per peer under a restart-on-panic supervisor; backoff 5s→5m
src/auth.rs     Caller extractor: mesh token | agent key | session
src/oidc.rs     Pocket ID sign-in (openidconnect, PKCE, encrypted cookie)
src/render.rs   board.txt and the read-only HTML page (maud)
src/routes.rs   axum router
```

## Direction

This README and `PROTOCOL.md` describe what runs today (v1). Where it is going —
four verbs, one `kapwa` binary, signed nostr events, collectives, the kita seam,
and every open decision — is at https://kapwa-ideas.felixflor.es (source in
[`ideas/`](ideas/)).

## Not yet

- signed events, and with them per-author trust for hand-overs
- `kapwa setup claude` only prints the hook; it does not install it
- who can see what, once other collectives join (the design site's decision Y1)
