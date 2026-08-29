#!/usr/bin/env bash
# End-to-end test against a throwaway headless Herdr session.
# Usage: scripts/e2e.sh   (needs herdr, nvim, cargo, python3)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SESSION="${HERDR_NVIM_E2E_SESSION:-hn-e2e}"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/herdr/plugins/herdr-nvim"
STATE="$STATE_DIR/daemons.json"
SOCK="${XDG_CONFIG_HOME:-$HOME/.config}/herdr/sessions/$SESSION/herdr.sock"
BIN="$ROOT/target/release/herdr-nvim"
export PATH="$HOME/.cargo/bin:$PATH"
TMP="$(mktemp -d)"
FOREIGN_PID=""

hs() { herdr --session "$SESSION" "$@"; }
py() { python3 -c "$1" "${@:2}"; }
json() { py 'import json,sys; d=json.load(sys.stdin); r=d.get("result",d); print(eval(sys.argv[1]))' "$1"; }
state_get() { py 'import json,sys; d=json.load(open(sys.argv[1])); s=d["sessions"].get(sys.argv[2],{}); r=s.get(sys.argv[3]); print(json.dumps(r.get(sys.argv[4])) if r else "null")' "$STATE" "$SOCK" "$1" "$2"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
pane_gone() { local out; out=$(hs pane get "$1" 2>&1 || true); py 'import json,sys
raw=sys.argv[1]
try: d=json.loads(raw)
except Exception: sys.exit(0 if "not_found" in raw else 1)
sys.exit(0 if "error" in d else 1)' "$out"; }
wait_pane_gone() { for _ in $(seq 1 25); do pane_gone "$1" && return 0; sleep 0.2; done; return 1; }
# hide the sidebar of $TAB if one is recorded
hide_sidebar() { local sb; sb=$(state_get "$TAB" sidebar_pane_id | tr -d '"'); if [ "$sb" != "null" ] && ! pane_gone "$sb"; then hs plugin action invoke toggle --plugin herdr-nvim >/dev/null; wait_pane_gone "$sb" || fail "could not hide sidebar $sb"; fi; }
# rects of the tab containing pane $1: "pane_id x y w h" lines (relative to the area) + "AREA w h" + "ZOOMED b"
rects() { hs pane layout --pane "$1" | py 'import json,sys
l=json.load(sys.stdin)["result"]["layout"]; a=l["area"]
print("AREA", a["width"], a["height"]); print("ZOOMED", str(l.get("zoomed", False)).lower())
for p in l["panes"]: r=p["rect"]; print(p["pane_id"], r["x"]-a["x"], r["y"]-a["y"], r["width"], r["height"])'; }
step() { echo "== $*"; }

cleanup() {
  set +e
  hs workspace list 2>/dev/null | py 'import json,sys
try:
  r=json.load(sys.stdin)["result"]
  print("\n".join(w["workspace_id"] for w in r["workspaces"]))
except Exception: pass' | while read -r ws; do [ -n "$ws" ] && hs workspace close "$ws" >/dev/null 2>&1; done
  sleep 2
  herdr session stop "$SESSION" >/dev/null 2>&1
  herdr session delete "$SESSION" >/dev/null 2>&1
  [ -n "$FOREIGN_PID" ] && kill -9 "$FOREIGN_PID" 2>/dev/null
  rm -rf "$TMP"
}
trap cleanup EXIT

step "build"
(cd "$ROOT" && cargo build --release 2>&1 | tail -1)

step "start session $SESSION"
herdr --session "$SESSION" server > "$TMP/server.log" 2>&1 &
for _ in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.2; done
[ -S "$SOCK" ] || fail "server did not start: $(cat "$TMP/server.log")"
hs plugin list --json | grep -q '"herdr-nvim"' || hs plugin link "$ROOT" >/dev/null

step "workspace in a temp git repo"
REPO="$TMP/repo"; mkdir -p "$REPO"; (cd "$REPO" && git init -q && printf 'one\ntwo\nthree\n' > a.txt && git add a.txt && git -c user.email=e2e@x -c user.name=e2e commit -qm init)
TAB=$(hs workspace create --cwd "$REPO" --label e2e --focus | json 'r["tab"]["tab_id"]')
ROOT_PANE=$(hs pane list | json '[p["pane_id"] for p in r["panes"] if p["tab_id"]=="'"$TAB"'"][0]')
echo "tab=$TAB root pane=$ROOT_PANE"

step "toggle opens a sidebar with identity"
hs plugin action invoke toggle --plugin herdr-nvim >/dev/null; sleep 3
PANE=$(state_get "$TAB" sidebar_pane_id | tr -d '"'); TERM_ID=$(state_get "$TAB" sidebar_terminal_id | tr -d '"'); PID=$(state_get "$TAB" pid)
[ "$PANE" != "null" ] || fail "no sidebar pane recorded: $(hs plugin log list --plugin herdr-nvim --limit 1)"
LIVE_TERM=$(hs pane get "$PANE" | json 'r["pane"]["terminal_id"]')
[ "$TERM_ID" = "$LIVE_TERM" ] || fail "terminal id mismatch: $TERM_ID vs $LIVE_TERM"
hs pane process-info --pane "$PANE" | grep -q -- '--remote-ui' || fail "sidebar pane is not running a --remote-ui client"
HERDR_NVIM_STATE_DIR="$STATE_DIR" "$BIN" status | grep -q '"running": true' || fail "status does not report running"
DSOCK=$(state_get "$TAB" socket | tr -d '"')
echo "pane=$PANE pid=$PID socket=$DSOCK"

step "toggle hides the sidebar, daemon survives"
hs plugin action invoke toggle --plugin herdr-nvim >/dev/null
wait_pane_gone "$PANE" || fail "sidebar pane still exists: $(hs pane get "$PANE" 2>&1 | head -c 300); log: $(hs plugin log list --plugin herdr-nvim --limit 2 | head -c 600)"
[ "$(state_get "$TAB" sidebar_pane_id)" = "null" ] || fail "sidebar_pane_id not cleared"
kill -0 "$PID" || fail "daemon died on toggle"

step "state survives: open a file, toggle again"
nvim --server "$DSOCK" --remote-expr "execute('edit a.txt')" >/dev/null
hs plugin action invoke toggle --plugin herdr-nvim >/dev/null; sleep 3
[ "$(state_get "$TAB" pid)" = "$PID" ] || fail "daemon was respawned"
nvim --server "$DSOCK" --remote-expr 'bufname()' | grep -q 'a.txt' || fail "open file lost"
PANE2=$(state_get "$TAB" sidebar_pane_id | tr -d '"')

step "layout round-trip: 3-pane tab -> full-height sidebar -> restored"
hide_sidebar
P2=$(hs pane split "$ROOT_PANE" --direction right --ratio 0.4 --no-focus | json 'r["pane"]["pane_id"]')
P3=$(hs pane split "$P2" --direction down --ratio 0.3 --no-focus | json 'r["pane"]["pane_id"]')
sleep 1; BEFORE=$(rects "$ROOT_PANE" | grep -v -E '^(AREA|ZOOMED)' | sort)
hs plugin action invoke toggle --plugin herdr-nvim >/dev/null; sleep 4
AFTER=$(rects "$ROOT_PANE"); echo "$AFTER"
SB=$(state_get "$TAB" sidebar_pane_id | tr -d '"')
echo "$AFTER" | py 'import sys
lines=sys.stdin.read().splitlines(); area=[l for l in lines if l.startswith("AREA")][0].split(); W,H=int(area[1]),int(area[2])
assert [l for l in lines if l.startswith("ZOOMED")][0].endswith("false"), "zoomed"
panes={l.split()[0]: list(map(int,l.split()[1:])) for l in lines if not l.startswith(("AREA","ZOOMED"))}
sb=panes[sys.argv[1]]; assert len(panes)==4, panes
assert sb[3]==H, f"sidebar not full height: {sb} vs {H}"
assert sb[0]+sb[2]>=W-1, f"sidebar not flush right: {sb} W={W}"
assert abs(sb[2]-round(0.45*W))<=3, f"sidebar width {sb[2]} vs {0.45*W}"
for pid,r in panes.items():
    if pid!=sys.argv[1]: assert r[0]+r[2]<=sb[0]+1, f"{pid} overlaps the sidebar: {r} sb={sb}"' "$SB" || fail "layout after toggle is wrong"
[ "$(state_get "$TAB" layout | py 'import json,sys; l=json.loads(sys.stdin.read()); print(l["phase"], len(l["parked"]))')" = "open 0" ] || fail "layout record not open/empty: $(state_get "$TAB" layout)"
hs tab list --workspace "${TAB%%:*}" | json 'len(r["tabs"])' | grep -q '^1$' || fail "parking tab still exists"
hs plugin action invoke toggle --plugin herdr-nvim >/dev/null; wait_pane_gone "$SB" || fail "sidebar not closed"
sleep 1; AFTER2=$(rects "$ROOT_PANE" | grep -v -E '^(AREA|ZOOMED)' | sort)
py 'import sys
b={l.split()[0]: list(map(int,l.split()[1:])) for l in sys.argv[1].splitlines()}
a={l.split()[0]: list(map(int,l.split()[1:])) for l in sys.argv[2].splitlines()}
assert b.keys()==a.keys(), (b,a)
for k in b: assert all(abs(x-y)<=2 for x,y in zip(b[k],a[k])), (k,b[k],a[k])' "$BEFORE" "$AFTER2" || fail "layout not restored: before=$BEFORE after=$AFTER2"
[ "$(state_get "$TAB" layout)" = "null" ] || fail "layout record not cleared"

step "left side + zoomed tab"
hide_sidebar
# env is not passed through `plugin action invoke`; use the config file instead
CFGDIR=$(hs plugin config-dir herdr-nvim); mkdir -p "$CFGDIR"; [ -f "$CFGDIR/config.env" ] && cp "$CFGDIR/config.env" "$TMP/config.env.bak"
printf 'HERDR_NVIM_SIDE=left\n' > "$CFGDIR/config.env"
hs pane zoom --on --pane "$ROOT_PANE" >/dev/null
hs plugin action invoke toggle --plugin herdr-nvim >/dev/null; sleep 4
SB=$(state_get "$TAB" sidebar_pane_id | tr -d '"')
rects "$ROOT_PANE" | grep -q "^ZOOMED false" || fail "tab still zoomed"
rects "$ROOT_PANE" | grep "^$SB " | awk '{exit ($2==0)?0:1}' || fail "left sidebar not at x=0: $(rects "$ROOT_PANE")"
hs plugin action invoke toggle --plugin herdr-nvim >/dev/null; wait_pane_gone "$SB" || fail "left sidebar not closed"
rm -f "$CFGDIR/config.env"; [ -f "$TMP/config.env.bak" ] && cp "$TMP/config.env.bak" "$CFGDIR/config.env"
hs pane zoom --off --pane "$ROOT_PANE" >/dev/null 2>&1 || true

step "recovery: fabricated mid-open record is finished by gc"
PARK=$(hs tab create --workspace "${TAB%%:*}" --label park --no-focus | json 'r["tab"]["tab_id"]')
PH=$(hs pane list | json '[p["pane_id"] for p in r["panes"] if p["tab_id"]=="'"$PARK"'"][0]')
hs pane move "$P2" --tab "$PARK" --split right --no-focus >/dev/null; hs pane move "$P3" --tab "$PARK" --split right --no-focus >/dev/null
py 'import json,sys
p,sock,tab,anchor,p2,p3,park,ph=sys.argv[1:]
d=json.load(open(p)); r=d["sessions"][sock][tab]
r["layout"]={"phase":"evacuating","anchor":anchor,"parking_tab":park,"parking_placeholder":ph,"parked":[p2,p3],
 "steps":[{"pane":p2,"dir":"right","target":anchor,"ratio":0.4},{"pane":p3,"dir":"down","target":p2,"ratio":0.3}]}
json.dump(d,open(p,"w"))' "$STATE" "$SOCK" "$TAB" "$ROOT_PANE" "$P2" "$P3" "$PARK" "$PH"
hs plugin action invoke gc --plugin herdr-nvim >/dev/null; sleep 4
hs pane get "$P2" | json 'r["pane"]["tab_id"]' | grep -q "^$TAB$" || fail "P2 not moved back"
hs pane get "$P3" | json 'r["pane"]["tab_id"]' | grep -q "^$TAB$" || fail "P3 not moved back"
hs tab list --workspace "${TAB%%:*}" | json 'len(r["tabs"])' | grep -q '^1$' || fail "parking tab not closed after recovery"
[ "$(state_get "$TAB" layout)" = "null" ] || fail "layout record left after recovery: $(state_get "$TAB" layout)"
hs pane close "$P3" >/dev/null; hs pane close "$P2" >/dev/null; sleep 1

step "pick-file: scrape layer from a shell pane"
hs pane run "$ROOT_PANE" "echo touched $REPO/a.txt:2" >/dev/null; sleep 1.5
hs pane report-agent "$ROOT_PANE" --source herdr-nvim-e2e --agent claude --state idle >/dev/null
hs agent focus "$ROOT_PANE" >/dev/null 2>&1 || hs pane focus --pane "$ROOT_PANE" >/dev/null 2>&1 || true
OUT=$(HERDR_SOCKET_PATH="$SOCK" HERDR_NVIM_TAB_ID="$TAB" HERDR_WORKSPACE_ID="${TAB%%:*}" HERDR_BIN_PATH="$(command -v herdr)" "$BIN" pick-file --json || true)
echo "$OUT" | py 'import json,sys; d=json.load(sys.stdin); c=[x for x in d["handoff"]["candidates"] if x["path"].endswith("/a.txt")]; assert len(c)==1, ("expected one merged entry", c); assert c[0]["session"] and c[0]["line"]==2, ("scrape layer missing", c)' || fail "pick-file --json did not list a.txt as a scraped session file: $OUT"
hs plugin action invoke pick-file --plugin herdr-nvim >/dev/null; sleep 6
nvim --server "$DSOCK" --remote-expr "luaeval(\"require('herdr-nvim.picker').is_open()\")" | grep -q 'true' || fail "picker not open in the daemon: $(hs plugin log list --plugin herdr-nvim --limit 1 | head -c 400)"
nvim --server "$DSOCK" --remote-expr "luaeval(\"require('herdr-nvim.picker').close()\")" >/dev/null 2>&1 || true
[ -z "$(ls -A "$STATE_DIR/handoff" 2>/dev/null)" ] || fail "handoff files left behind"
hs pane release-agent "$ROOT_PANE" --source herdr-nvim-e2e --agent claude >/dev/null 2>&1 || true
SB=$(state_get "$TAB" sidebar_pane_id | tr -d '"'); [ "$SB" != "null" ] && { hs plugin action invoke toggle --plugin herdr-nvim >/dev/null; wait_pane_gone "$SB" || true; }

step "manifest link patterns"
py 'import re,sys
raw=open(sys.argv[1]).read()
pats=dict(re.findall(r"id = \"(file-[a-z]+)\"\ntitle = \"[^\"]*\"\npattern = \x27([^\x27]+)\x27", raw))
fp=re.compile(pats["file-path"]); fu=re.compile(pats["file-url"])
for ok in ["src/main.rs","./src/bridge.rs:42","/Users/a/src/lib.rs:10:3","app/(marketing)/page.tsx","~/sub/notes.md"]: assert fp.match(ok), ok
for bad in ["Node.js","e.g.","README.md","and/or","v0.7.0"]: assert not fp.match(bad), bad
assert fu.match("file:///tmp/a.py") and not fu.match("https://x")' "$ROOT/herdr-plugin.toml" || fail "link handler patterns"

step "identity: a foreign pane with a reused id is left alone"
hide_sidebar
SHELL_PANE=$(hs pane split "$ROOT_PANE" --direction down --no-focus | json 'r["pane"]["pane_id"]')
py 'import json,sys
p,sock,tab,pane,term=sys.argv[1:]
d=json.load(open(p)); r=d["sessions"][sock][tab]; r["sidebar_pane_id"]=pane; r["sidebar_terminal_id"]=term
json.dump(d,open(p,"w"))' "$STATE" "$SOCK" "$TAB" "$SHELL_PANE" "$TERM_ID"
hs plugin action invoke toggle --plugin herdr-nvim >/dev/null; sleep 3
pane_gone "$SHELL_PANE" && fail "toggle closed a foreign pane"
NEWPANE=$(state_get "$TAB" sidebar_pane_id | tr -d '"')
[ "$NEWPANE" != "$SHELL_PANE" ] && [ "$NEWPANE" != "null" ] || fail "no new sidebar opened (got $NEWPANE)"
hs pane close "$SHELL_PANE" >/dev/null
hs plugin action invoke toggle --plugin herdr-nvim >/dev/null; sleep 2   # hide

step "concurrency: two opens, one daemon"
hs plugin action invoke open --plugin herdr-nvim >/dev/null &
P1=$!
hs plugin action invoke open --plugin herdr-nvim >/dev/null &
P2=$!
wait "$P1" "$P2"; sleep 3
[ "$(state_get "$TAB" pid)" = "$PID" ] || fail "concurrent open respawned the daemon"
pgrep -f -- "--listen $DSOCK" | wc -l | grep -q '^ *1$' || fail "more than one daemon for $DSOCK"
for p in $(hs pane list | json '" ".join(p["pane_id"] for p in r["panes"] if p.get("label")=="Neovim")'); do hs pane close "$p" >/dev/null 2>&1 || true; done
sleep 1

step "send --dry-run from a pane environment"
cat > "$TMP/payload.json" <<JSON
{"cwd":"$REPO","comments":[{"file":"$REPO/a.txt","line":2,"end_line":2,"text":"why two?","code":"two","filetype":"text","modified":false}]}
JSON
HERDR_SOCKET_PATH="$SOCK" HERDR_WORKSPACE_ID="${TAB%%:*}" HERDR_TAB_ID="$TAB" "$BIN" send --dry-run --file "$TMP/payload.json" | grep -q '"ok":true' || fail "dry-run failed"
OUT=$(HERDR_SOCKET_PATH="$SOCK" HERDR_WORKSPACE_ID="${TAB%%:*}" HERDR_TAB_ID="$TAB" "$BIN" send --file "$TMP/payload.json" || true)
echo "$OUT" | grep -q '"code":"no_agents"' || fail "expected no_agents, got: $OUT"

step "ask --dry-run and ask with no agents"
cat > "$TMP/ask.json" <<JSON
{"cwd":"$REPO","message":"why two?","selection":{"file":"$REPO/a.txt","line":2,"end_line":2,"code":"two","filetype":"text","modified":false}}
JSON
ENV_ASK=(HERDR_SOCKET_PATH="$SOCK" HERDR_WORKSPACE_ID="${TAB%%:*}" HERDR_TAB_ID="$TAB")
PROMPT=$(env "${ENV_ASK[@]}" "$BIN" ask --dry-run --file "$TMP/ask.json")
echo "$PROMPT" | grep -q '"dry_run":true' || fail "ask dry-run failed: $PROMPT"
echo "$PROMPT" | grep -q 'From Neovim' || fail "ask prompt lost its header: $PROMPT"
echo "$PROMPT" | grep -q 'a.txt:2' || fail "ask prompt lost the location: $PROMPT"
if echo "$PROMPT" | grep -q 'refer to them by number'; then fail "the annotation framing leaked into ask"; fi
OUT=$(env "${ENV_ASK[@]}" "$BIN" ask --file "$TMP/ask.json" || true)
echo "$OUT" | grep -q '"code":"no_agents"' || fail "expected no_agents, got: $OUT"
# `ask` exits 1 on a refusal and pipefail would propagate that, so capture first.
BLANK=$(printf '%s' '{"message":"  "}' | env "${ENV_ASK[@]}" "$BIN" ask || true)
echo "$BLANK" | grep -q '"code":"no_message"' || fail "a blank ask was not refused: $BLANK"

step "tab close saves modified buffers and stops the daemon"
nvim --server "$DSOCK" --remote-expr "execute(['edit a.txt','call setline(1,\"changed by e2e\")'])" >/dev/null
TAB2=$(hs tab create --workspace "${TAB%%:*}" --label two --no-focus | json 'r["tab"]["tab_id"]')
hs tab close "$TAB" >/dev/null; sleep 4
grep -q 'changed by e2e' "$REPO/a.txt" || fail "modified buffer was not saved on tab close"
kill -0 "$PID" 2>/dev/null && fail "daemon still alive after tab close"
[ "$(state_get "$TAB" pid)" = "null" ] || fail "record not removed"
hs plugin log list --plugin herdr-nvim --limit 3 | grep -q 'tab.closed: stopped 1' || fail "event hook did not report the stop"

step "gc reclaims a daemon of a deleted session and forgets dead records"
FSOCK="$TMP/foreign.sock"
nvim --headless --listen "$FSOCK" >/dev/null 2>&1 &
FOREIGN_PID=$!; sleep 1
LSTART=$(ps -o lstart= -p "$FOREIGN_PID" | awk '{print $1,$2,$3,$4,$5}')
py 'import json,sys
p,fsock,pid,lstart=sys.argv[1:]
d=json.load(open(p))
d["sessions"]["/nonexistent/session/herdr.sock"]={"w9:t9":{"pid":int(pid),"socket":fsock,"cwd":"/tmp","ps_lstart":lstart,"started_unix":1,"starting":False}}
d["sessions"]["/nonexistent/other/herdr.sock"]={"w8:t8":{"pid":999999,"socket":"/nonexistent.sock","cwd":"/tmp","started_unix":1,"starting":False}}
json.dump(d,open(p,"w"))' "$STATE" "$FSOCK" "$FOREIGN_PID" "$LSTART"
hs plugin action invoke gc --plugin herdr-nvim >/dev/null; sleep 5
kill -0 "$FOREIGN_PID" 2>/dev/null && fail "gc did not stop the foreign-session daemon"
FOREIGN_PID=""
py 'import json,sys; d=json.load(open(sys.argv[1])); s=d["sessions"]; assert "/nonexistent/other/herdr.sock" not in s and "/nonexistent/session/herdr.sock" not in s, s' "$STATE" || fail "gc left fabricated records"

echo "ALL E2E CHECKS PASSED"
