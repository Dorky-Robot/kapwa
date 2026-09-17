#!/usr/bin/env bash
# One node with a stable address (the gateway) and two leaves that nothing
# can connect to. The gateway has NO peers configured: it never reaches out.
# Everything it learns, the leaves hand it; everything the leaves learn from
# each other, they learn through it. Then the gateway dies and comes back.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${KAPWA_BIN:-target/debug/kapwa}
[ -x "$BIN" ] || { echo "build first: cargo build"; exit 1; }

HP=${HUB_PORT:-3520}; AP=$((HP + 1)); BP=$((HP + 2))
H=http://127.0.0.1:$HP; A=http://127.0.0.1:$AP; B=http://127.0.0.1:$BP
TMP=$(mktemp -d /tmp/kapwa-gw.XXXX); mkdir -p "$TMP/out"
printf 'ana:ana-key:lead:*\nben:ben-key:lead:*\nviewer:view-key:worker:*\n' >"$TMP/agents"

start() { # name port peers
  KAPWA_ENV_FILE=/dev/null KAPWA_WRITER=$1 KAPWA_PORT=$2 KAPWA_PEERS=$3 KAPWA_DIR=$TMP/$1 \
  KAPWA_MESH_TOKEN=demo-mesh KAPWA_AGENTS_FILE=$TMP/agents \
    "$BIN" serve >"$TMP/out/$1.log" 2>&1 &
  echo $!
}
post() { curl -sS -X POST "$1/api/event" -H "Authorization: Bearer $2" -H 'content-type: application/json' -d "$3" >/dev/null; }
board() { curl -s -H 'Authorization: Bearer view-key' "$1/api/board.txt"; }
until_up() { for _ in $(seq 40); do curl -sf "$1/healthz" >/dev/null && return; sleep 0.25; done; echo "$1 never came up"; cat "$TMP"/out/*.log; exit 1; }
tick() { sleep "${1:-6}"; }   # sync interval is 5s
say() { printf '\n\033[1m%s\033[0m\n' "$*"; }
trap 'kill $PH $PA $PB 2>/dev/null || true; echo; echo "scratch: $TMP"' EXIT

say "1. a gateway with no peers, and two leaves that only know the gateway"
PH=$(start gateway $HP "")
PA=$(start leaf-a $AP $H)
PB=$(start leaf-b $BP $H)
until_up $H; until_up $A; until_up $B
board $H | sed -n '1,3p'

say "2. ana, on leaf-a, writes. the gateway never asked, yet it has it:"
post $A ana-key '{"kind":"create","id":"fence-quote","title":"Get a second quote for the fence","priority":"P1"}'
post $A ana-key '{"kind":"claim","id":"fence-quote","note":"mine"}'
tick
board $H

say "3. …and leaf-b, which has never heard of leaf-a, has it too:"
tick
board $B | sed -n '1,2p;4,$p'

say "4. the other way: ben, on leaf-b, writes; leaf-a sees it through the gateway"
post $B ben-key '{"kind":"create","id":"roof-leak","title":"Roof leaks over the back door","priority":"P0"}'
tick 11
board $A | sed -n '2p;4,$p'

say "5. the gateway dies. both leaves keep working"
kill $PH; wait $PH 2>/dev/null || true
post $A ana-key '{"kind":"note","id":"roof-leak","note":"ana: I can look Thursday"}'
post $B ben-key '{"kind":"claim","id":"roof-leak"}'
tick 7
echo "   leaf-a: $(board $A | sed -n '3p' | cut -c1-90)"
echo "   leaf-b still thinks roof-leak has $(curl -s -H 'Authorization: Bearer view-key' $B/api/item/roof-leak | grep -o '"history":\[[^]]*\]' | grep -o '"kind"' | wc -l | tr -d ' ') events"

say "6. the gateway comes back (same disk). everyone converges, nothing was lost"
PH=$(start gateway $HP "")
until_up $H; tick 16
for n in "$H gateway" "$A leaf-a" "$B leaf-b"; do set -- $n
  printf '   %-8s ' "$2"; curl -s -H 'Authorization: Bearer view-key' $1/api/item/roof-leak \
    | python3 -c "import json,sys; i=json.load(sys.stdin); print('owner', i['owner'], '· history', [h['by']+':'+h['kind'] for h in i['history']])"
done

say "7. who reached out to whom"
echo "   gateway peers: $(curl -s -H 'Authorization: Bearer view-key' $H/api/peers)"
curl -s -H 'Authorization: Bearer view-key' $A/api/peers | python3 -c "import json,sys; [print('   leaf-a →', u, '· pulled', s['pulled'], '· pushed', s['pushed']) for u,s in json.load(sys.stdin).items()]"
curl -s -H 'Authorization: Bearer view-key' $B/api/peers | python3 -c "import json,sys; [print('   leaf-b →', u, '· pulled', s['pulled'], '· pushed', s['pushed']) for u,s in json.load(sys.stdin).items()]"
