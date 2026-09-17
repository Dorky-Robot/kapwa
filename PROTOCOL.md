# kapwa mesh — wire protocol

This is the contract. A node is anything that speaks it: the Elixir release
on a Mac, a Nerves image on a Pi, or 150 lines of C on an ESP32. The node
implementation may change; this should not, and when it must, the version
bumps.

**Version: 1**

## Identities

| caller | header | may |
|---|---|---|
| another node | `Authorization: Bearer <mesh token>` | read any log |
| an agent | `Authorization: Bearer <agent key>` | append to this node's log, read the board |
| a person | session cookie (OIDC) | read the dashboard |

Nothing is open except `GET /healthz` and the sign-in flow. All routes
bind `127.0.0.1`; a tunnel makes them reachable.

## Event

One JSON object per line. Every event has these six fields; `kind`
decides the rest.

```json
{"writer":"mini","seq":412,"at":"2026-09-16T23:25:46.102Z","by":"claude",
 "kind":"claim","id":"kita-sms","note":"taking it"}
```

| field | set by | meaning |
|---|---|---|
| `writer` | node | whose log this is in. A node appends only to its own. |
| `seq` | node | 1-based, contiguous per writer. The replication cursor. |
| `at` | node | ISO-8601 UTC with milliseconds. Fold order. |
| `by` | node, from the key | who acted. Never taken from the request body. |
| `kind` | caller | see below |
| `id` | caller | the item |

Kinds and their extra fields:

| kind | fields | effect on the item |
|---|---|---|
| `create` | `title`, `type`, `product`, `priority`, `status`, `pointers`, `watchers` | new item; a second create for the same id is a note |
| `claim` | `status?`, `note?` | owner = `by` if unowned or already `by`; otherwise recorded as lost |
| `release` | `status?`, `note?` | owner = "" if owner == `by` |
| `status` | `value` | status = value (`open claimed blocked asked done parked`) |
| `priority` | `value` | priority = value (`P0 P1 P2 P3` or "") |
| `assign` | `to`, `status?` | owner = to |
| `resolve` | `status?` | status = done |
| `note` | `note` | history only |

Unknown kinds are kept in the log and ignored by the fold, so a newer node
can write things an older one skips.

## Fold

State = fold over every event from every log, sorted by `(at, writer,
seq)`. Every node runs the same fold on the same events and gets the same
board. Ties on concurrent claims resolve to the earliest `at`; the loser's
attempt stays in history as `claim lost to <owner>`.

## Routes

### Replication (mesh token)

```
GET /api/writers
→ {"mini":412,"mac2024":9,"doug-mini":3}          every log this node holds, own and mirrored

GET /api/log/<writer>?since=<seq>
→ [ {event}, {event}, ... ]                       events with seq > since, ascending
```

A puller asks each peer for `/api/writers`, and for every writer it is
behind on, `/api/log/<writer>?since=<what I have>`. Received events are
appended to the local mirror only if `seq` is exactly `last + 1`; a gap
ends the batch. Peers serve their mirrors, so a log reaches you even when
its author is offline. A node never pulls its own writer name.

### Agents (agent key)

```
POST /api/event        {"kind":"...","id":"...", ...}     → {"ok":true,"event":{...},"item":{...}}
GET  /api/board.txt    plain-text attention board
GET  /api/state        {"items":{...},"writers":{...}}
GET  /api/item/<id>
GET  /api/peers        per-peer last_ok / last_error / interval
GET  /api/whoami
```

### Open

```
GET /healthz           → {"ok":true,"writer":"mini"}
```

## Minimal peer

To be a real peer a device needs only:

1. a monotonic `seq` and an append-only file of its own events;
2. `GET /api/writers` and `GET /api/log/<me>?since=` behind the mesh token;
3. optionally, a pull loop against one or more peers.

Everything else — the fold, the board, the dashboard — can live on the
bigger nodes that mirror it.
