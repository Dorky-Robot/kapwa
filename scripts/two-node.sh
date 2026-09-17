#!/usr/bin/env bash
# Two nodes on one laptop, pulling each other. Exercises the failure cases:
# (raw HTTP on purpose: this is what a client in any language sends)
# a peer down, writes while partitioned, catch-up on return, a concurrent
# claim that has to lose on both sides the same way, and the auth edges.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${KAPWA_BIN:-target/debug/kapwa}
[ -x "$BIN" ] || { echo "build first: cargo build"; exit 1; }

PA_=${A_PORT:-3410}; PB_=${B_PORT:-3411}   # override to run beside a live node
A=http://127.0.0.1:$PA_
B=http://127.0.0.1:$PB_
TMP=$(mktemp -d /tmp/kapwa-demo.XXXX)
mkdir -p "$TMP/out"
# nodes share a mesh token; each node has its own agent (claude on A, grok on B)
printf 'claude:claude-key:lead:*\ngrok:grok-key:lead:*\n' >"$TMP/agents"
AUTH=(-H 'Authorization: Bearer claude-key')
KEY_A=claude-key; KEY_B=grok-key

start() { # name port peer
  KAPWA_ENV_FILE=/dev/null KAPWA_WRITER=$1 KAPWA_PORT=$2 KAPWA_PEERS=$3 KAPWA_DIR=$TMP/$1 \
  KAPWA_MESH_TOKEN=demo-mesh KAPWA_AGENTS_FILE=$TMP/agents \
    "$BIN" serve >"$TMP/out/$1.log" 2>&1 &
  echo $!
}
post() { curl -sS -X POST "$1/api/event" -H "Authorization: Bearer $2" -H 'content-type: application/json' -d "$3" >/dev/null; }
board() { curl -s "${AUTH[@]}" "$1/api/board.txt"; }
until_up() { for _ in $(seq 40); do curl -sf "$1/healthz" >/dev/null && return; sleep 0.25; done; echo "$1 never came up"; cat "$TMP"/out/*.log; exit 1; }
pull() { sleep "${1:-6}"; }  # pull interval is 5s
say() { printf '\n\033[1m%s\033[0m\n' "$*"; }

trap 'kill $PA $PB 2>/dev/null || true; echo; echo "scratch: $TMP"' EXIT

say "1. start A (claude@a :3410) and B (grok@b :3411), each pulling the other"
PA=$(start claude@a $PA_ $B)
PB=$(start grok@b $PB_ $A)
until_up $A; until_up $B

say "2. A creates + claims an item; B has not seen it yet"
post $A $KEY_A '{"kind":"say","id":"fence-quote","text":"Get a second quote for the fence","p":"P1"}'
post $A $KEY_A '{"kind":"take","id":"fence-quote"}'
board $B | head -3

say "   ...after one pull cycle B has it"
pull
board $B

say "3. kill A. B keeps working: reads its mirror, writes its own log"
kill $PA; wait $PA 2>/dev/null || true
post $B $KEY_B '{"kind":"say","id":"roof-leak","text":"Roof leaks over the back door","p":"P0"}'
post $B $KEY_B '{"kind":"say","id":"fence-quote","text":"first quote was 900"}'
pull 7
board $B

say "4. A comes back and catches up on what B did while A was gone"
PA=$(start claude@a $PA_ $B)
until_up $A; pull
board $A

say "5. concurrent claim: B claims roof-leak, A claims it 1s later, before either has pulled"
post $B $KEY_B '{"kind":"take","id":"roof-leak"}'
sleep 1
post $A $KEY_A '{"kind":"take","id":"roof-leak"}'
echo "   A, right after its own take (sync is two-way now, so it may already know better):"
curl -s "${AUTH[@]}" $A/api/item/roof-leak | grep -o '"owner":"[^"]*"'
pull
echo "   A, after pulling, agrees with B — earliest claim wins on both sides:"
curl -s "${AUTH[@]}" $A/api/item/roof-leak | grep -o '"owner":"[^"]*"'
curl -s "${AUTH[@]}" $B/api/item/roof-leak | grep -o '"owner":"[^"]*"'
board $A | sed -n '/contested/,$p'

say "6. a node without the mesh token cannot replicate; a wrong agent key cannot write"
curl -s -o /dev/null -w '   /api/writers, no token:    HTTP %{http_code}\n' $A/api/writers
curl -s -o /dev/null -w '   /api/writers, mesh token:  HTTP %{http_code}\n' -H 'Authorization: Bearer demo-mesh' $A/api/writers
curl -s -o /dev/null -w '   POST /api/event, bad key:  HTTP %{http_code}\n' -X POST -H 'Authorization: Bearer nope' -H 'content-type: application/json' -d '{"kind":"note","id":"x"}' $A/api/event

say "7. the logs on disk: each node appended only to its own"
for n in claude@a grok@b; do
  echo "  $n:"; (cd "$TMP/$n" && wc -l log/*.jsonl mirror/*.jsonl | sed 's/^/    /')
done
