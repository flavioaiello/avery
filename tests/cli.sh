#!/bin/bash
# Tests for everything around the mail server: bin/avery, libexec/avery-make-app, tools/*.sh, formula, cask.
#
#   tests/cli.sh [J L N P G O W Q M]   all sections, or only the named ones
#
# The mail server has its own tests: cargo test --manifest-path avery-mail/Cargo.toml
#
# Sections: J bin/avery, L hygiene of the repository, N service commands and formula, P the Avery.app builder,
# G opening Goose with the Avery profile, O the setup dialogs (everything that needs no screen), W the watcher,
# Q icons, disk image, cask and publish-dmg.sh, M versions agree.
#
# No Mail access, no real model, no Goose, no "brew services", no dialog on your screen: whatever would start something
# runs with AVERY_DRY_RUN=1 or against stand-ins in temporary folders, and the setup dialogs are only run in states in
# which they stay silent. bin/avery always runs with its own HOME and without
# the AVERY_* and MAIL_MCP_* variables of your environment.
# Output: "PASS  <name>" or "FAIL  <name>   <detail>", at the end "N bestanden, M fehlgeschlagen" (exit code 1 if
# any check failed). A check that needs a tool you do not have (ruby, brew, ...) prints "SKIP  <name>   <reason>".
set -u
LC_ALL=C
export LC_ALL

resolve_path() {
  local target="$1" dir
  while [ -L "$target" ]; do
    dir="$(cd "$(dirname "$target")" && pwd)"
    target="$(readlink "$target")"
    case "$target" in /*) ;; *) target="$dir/$target" ;; esac
  done
  echo "$(cd "$(dirname "$target")" && pwd)/$(basename "$target")"
}
SELF="$(resolve_path "$0")"
HERE="$(dirname "$SELF")"
ROOT="$(dirname "$HERE")"
if [ ! -f "$ROOT/bin/avery" ] || [ ! -f "$ROOT/avery-mail/Cargo.toml" ]; then
  echo "tests/cli.sh: $ROOT sieht nicht wie ein Avery-Checkout aus" >&2
  exit 2
fi
AVERY="$ROOT/bin/avery"
MAKE_APP="$ROOT/libexec/avery-make-app"
SHARE="$ROOT/share/avery"
MAIL_BIN="$ROOT/avery-mail/target/debug/avery-mail"
PATH="${HOME:-/nonexistent}/.cargo/bin:$PATH:/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

SELECTED="$*"
for section in $SELECTED; do
  case "$section" in
    J|L|N|P|G|O|W|Q|M) ;;
    *) echo "Aufruf: tests/cli.sh [J L N P G O W Q M]" >&2; exit 2 ;;
  esac
done

TMPBASE="${TMPDIR:-/tmp}"
TMPBASE="${TMPBASE%/}"
TT="$(mktemp -d "$TMPBASE/averytest.XXXXXX")" || { echo "tests/cli.sh: kein temporaeres Verzeichnis in $TMPBASE" >&2; exit 2; }

PASS=0
FAIL=0
SKIP=0
FAKE_PIDS=""
HEALTH_PORT=""
DEAD_URL="http://127.0.0.1:9/health"
ALIVE_URL="http://127.0.0.1:1/health"

# ------------------------------------------------------------------ clean up: processes first, then files
cleanup() {
  local pid waited=0 jobs_list fake_list="" still file
  trap - EXIT INT TERM HUP
  jobs_list="$(jobs -p)"
  for pid in $FAKE_PIDS; do
    case "$(ps -p "$pid" -o command= 2>/dev/null)" in *"$TT/"*) fake_list="$fake_list $pid" ;; esac
  done
  for pid in $jobs_list $fake_list; do kill "$pid" 2>/dev/null; done
  while [ "$waited" -lt 100 ]; do
    still=""
    for pid in $jobs_list $fake_list; do if kill -0 "$pid" 2>/dev/null; then still=1; fi; done
    [ -n "$still" ] || break
    sleep 0.05
    waited=$((waited + 1))
  done
  for pid in $jobs_list $fake_list; do if kill -0 "$pid" 2>/dev/null; then kill -9 "$pid" 2>/dev/null; fi; done
  { wait; } 2>/dev/null
  for file in "$TT"/health*.pid; do
    [ -f "$file" ] || continue
    pid="$(cat "$file" 2>/dev/null)"
    case "$(ps -p "$pid" -o command= 2>/dev/null)" in "nc -l 127.0.0.1 $HEALTH_PORT"*) kill "$pid" 2>/dev/null ;; esac
  done
  rm -rf "$TT"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# ------------------------------------------------------------------ results
brief() { printf '%s' "$1" | awk 'NR > 1 { printf " | " } { printf "%s", $0 }' | cut -c1-600; }
pass() { PASS=$((PASS + 1)); printf 'PASS  %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); printf 'FAIL  %s   %s\n' "$1" "$(brief "${2:-}")"; }
skip() { SKIP=$((SKIP + 1)); printf 'SKIP  %s   %s\n' "$1" "$2"; }
# Usage: <condition>; check "name" $? "detail"
check() { if [ "$2" -eq 0 ]; then pass "$1"; else fail "$1" "${3:-}"; fi; }

# ------------------------------------------------------------------ text helpers
has() { case "$1" in *"$2"*) return 0 ;; esac; return 1; }
lacks() { case "$1" in *"$2"*) return 1 ;; esac; return 0; }
has_i() { has "$(printf '%s' "$1" | tr 'A-Z' 'a-z')" "$(printf '%s' "$2" | tr 'A-Z' 'a-z')"; }
begins() { case "$1" in "$2"*) return 0 ;; esac; return 1; }
ends() { case "$1" in *"$2") return 0 ;; esac; return 1; }
count() { printf '%s\n' "$1" | grep -F -o -- "$2" | wc -l | tr -d ' '; }
last_line() { printf '%s\n' "$1" | tail -n 1; }
any_line_ends() {
  local line
  while IFS= read -r line; do
    case "$line" in *"$2") return 0 ;; esac
  done <<< "$1"
  return 1
}
trim() {
  local s="$1"
  s="${s#"${s%%[![:space:]]*}"}"
  s="${s%"${s##*[![:space:]]}"}"
  printf '%s' "$s"
}
have() { command -v "$1" >/dev/null 2>&1; }
mkd() { mktemp -d "$TT/${1:-d}.XXXXXX"; }
write_exec() { cat > "$1" && chmod 755 "$1"; }
# plist_get FILE KEY: value of a top-level plist key (exit status 1 and no output if the key does not exist).
plist_get() { /usr/libexec/PlistBuddy -c "Print :$2" "$1" 2>/dev/null; }

# ------------------------------------------------------------------ running commands
# capture SECONDS COMMAND...: sets OUT, ERR and RC (124 when the time limit is exceeded).
capture() {
  local limit="$1" pid ticks=0 max
  shift
  max=$((limit * 50))
  "$@" >"$TT/cap.out" 2>"$TT/cap.err" </dev/null &
  pid=$!
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$ticks" -ge "$max" ]; then
      kill "$pid" 2>/dev/null
      { wait "$pid"; } 2>/dev/null
      OUT="$(cat "$TT/cap.out")"
      ERR="$(cat "$TT/cap.err") Zeitlimit von $limit s ueberschritten"
      RC=124
      return 0
    fi
    sleep 0.02
    ticks=$((ticks + 1))
  done
  { wait "$pid"; RC=$?; } 2>/dev/null
  OUT="$(cat "$TT/cap.out")"
  ERR="$(cat "$TT/cap.err")"
}

# av_build [NAME=VALUE ...] ARGS...: AV_CMD runs bin/avery in a clean environment (only PATH, HOME, TMPDIR,
# AVERY_MAIL_BIN, AVERY_NO_ONBOARD=1 and the given variables). HOME=... picks the home (default: a new temporary
# folder), SCRIPT=... another copy of bin/avery, AVERY_MAIL_BIN= (empty) leaves the variable out. AHOME is the home
# used. AVERY_NO_ONBOARD=1 keeps the setup dialogs off, so that no check can ever open a dialog on your screen:
# only the checks of section O switch it off (AVERY_NO_ONBOARD=), and they run stand-ins or states without a question.
av_build() {
  local home="" script="$AVERY" mailbin="$MAIL_BIN" entry
  local -a extra
  extra=()
  while [ $# -gt 0 ]; do
    case "$1" in
      HOME=*) home="${1#HOME=}" ;;
      SCRIPT=*) script="${1#SCRIPT=}" ;;
      AVERY_MAIL_BIN=*) mailbin="${1#AVERY_MAIL_BIN=}" ;;
      [A-Z_]*=*) extra[${#extra[@]}]="$1" ;;
      *) break ;;
    esac
    shift
  done
  [ -n "$home" ] || home="$(mkd home)"
  AHOME="$home"
  AV_CMD=(env -i "PATH=$PATH" "HOME=$home" "TMPDIR=$TMPBASE" "AVERY_NO_ONBOARD=1")
  if [ -n "$mailbin" ]; then AV_CMD[${#AV_CMD[@]}]="AVERY_MAIL_BIN=$mailbin"; fi
  for entry in ${extra[@]+"${extra[@]}"}; do AV_CMD[${#AV_CMD[@]}]="$entry"; done
  AV_CMD[${#AV_CMD[@]}]="$script"
  for entry in "$@"; do AV_CMD[${#AV_CMD[@]}]="$entry"; done
}

# avery [NAME=VALUE ...] ARGS...: runs bin/avery, sets OUT, ERR, RC and AHOME.
avery() {
  av_build "$@"
  capture 60 "${AV_CMD[@]}"
}

# avery_bg OUTFILE [NAME=VALUE ...] ARGS...: starts bin/avery in the background (BG_PID), output goes to OUTFILE.
avery_bg() {
  local file="$1"
  shift
  av_build "$@"
  "${AV_CMD[@]}" >"$file" 2>&1 </dev/null &
  BG_PID=$!
}

# wait_gone PID SECONDS: 0 if the process ended in time, otherwise it is killed and 1 is returned.
wait_gone() {
  local pid="$1" ticks=0 max=$(($2 * 50))
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$ticks" -ge "$max" ]; then
      kill "$pid" 2>/dev/null
      { wait "$pid"; } 2>/dev/null
      return 1
    fi
    sleep 0.02
    ticks=$((ticks + 1))
  done
  { wait "$pid"; } 2>/dev/null
  return 0
}

# ------------------------------------------------------------------ stand-ins
# A health endpoint: two loops of "nc -l" that answer every request with 200 "ok". Two, because nc stops
# listening as soon as it has accepted a connection; the other loop covers the gap until its twin is back.
health_loop() {
  local port="$1" pidfile="$2" nc_pid="" failures=0
  trap - EXIT
  trap 'if [ -n "$nc_pid" ]; then kill "$nc_pid" 2>/dev/null; fi; exit 0' TERM INT HUP
  while :; do
    printf 'HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok' \
      | nc -l 127.0.0.1 "$port" >/dev/null 2>&1 &
    nc_pid=$!
    echo "$nc_pid" > "$pidfile"
    if wait "$nc_pid"; then
      failures=0
    else
      failures=$((failures + 1))
      [ "$failures" -lt 40 ] || exit 0
      sleep 0.05
    fi
  done
}

setup_health() {
  local attempt port waited body loop
  [ -n "$HEALTH_PORT" ] && return 0
  for attempt in 1 2 3 4 5 6 7 8; do
    port=$((20000 + RANDOM % 20001))
    health_loop "$port" "$TT/health1.pid" &
    loop=$!
    waited=0
    body=""
    while [ "$waited" -lt 30 ]; do
      body="$(curl -s -m 1 --noproxy '*' "http://127.0.0.1:$port/health" 2>/dev/null)"
      [ "$body" = "ok" ] && break
      sleep 0.1
      waited=$((waited + 1))
    done
    if [ "$body" = "ok" ] && kill -0 "$loop" 2>/dev/null; then
      health_loop "$port" "$TT/health2.pid" &
      HEALTH_PORT="$port"
      ALIVE_URL="http://127.0.0.1:$port/health"
      sleep 0.3
      return 0
    fi
    kill "$loop" 2>/dev/null
    { wait "$loop"; } 2>/dev/null
  done
  fail "N0 Attrappe fuer den Modellserver (nc) startet" "kein freier Port zwischen 20000 und 40000 antwortet"
  return 1
}

# A process whose command line looks like Goose.app's (and, with PROFILE, names its profile).
fake_goose() {
  local app="$1" root="${2:-}" waited=0
  if [ -n "$root" ]; then
    "$app/Contents/MacOS/Goose" "{\"GOOSE_PATH_ROOT\":\"$root\"}" >/dev/null 2>&1 &
  else
    "$app/Contents/MacOS/Goose" >/dev/null 2>&1 &
  fi
  FAKE_PID=$!
  disown "$FAKE_PID" 2>/dev/null
  FAKE_PIDS="$FAKE_PIDS $FAKE_PID"
  while ! pgrep -f "$app/Contents/MacOS/Goose" 2>/dev/null | grep -q -x "$FAKE_PID"; do
    waited=$((waited + 1))
    [ "$waited" -lt 100 ] || break
    sleep 0.05
  done
}

stop_process() {
  local pid="$1" waited=0 p rest=""
  kill "$pid" 2>/dev/null
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 100 ]; do
    sleep 0.05
    waited=$((waited + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then kill -9 "$pid" 2>/dev/null; fi
  for p in $FAKE_PIDS; do [ "$p" = "$pid" ] || rest="$rest $p"; done
  FAKE_PIDS="$rest"
}

# Stand-in for the avery command line: logs its arguments (used by the app builder tests).
setup_stub() {
  [ -z "${STUB:-}" ] || return 0
  STUB_DIR="$(mkd stub)"
  STUB="$STUB_DIR/fake-avery"
  STUB_LOG="$STUB_DIR/calls.log"
  write_exec "$STUB" <<EOF
#!/bin/sh
echo "\$@" >> "$STUB_LOG"
echo "Modell bereit"
exit 0
EOF
}

# A fake Goose.app with a profile, plus an "open" stand-in on PATH (shared by the sections G and W).
setup_goose() {
  [ -z "${FAKE_APP:-}" ] || return 0
  GOOSE_DIR="$(mkd goose)"
  FAKE_APP="$GOOSE_DIR/Fake.app"
  mkdir -p "$FAKE_APP/Contents/MacOS"
  write_exec "$FAKE_APP/Contents/MacOS/Goose" <<'EOF'
#!/bin/bash
exec -a "$0 $*" /bin/sleep 120
EOF
  PROFILE="$GOOSE_DIR/profile"
  mkdir -p "$PROFILE/config" "$GOOSE_DIR/stubs"
  printf 'x: 1\n' > "$PROFILE/config/config.yaml"
  OPEN_LOG="$GOOSE_DIR/open.log"
  write_exec "$GOOSE_DIR/stubs/open" <<EOF
#!/bin/sh
echo "\$@" >> "$OPEN_LOG"
EOF
}

# The mail server is built once; the checks use the debug binary through AVERY_MAIL_BIN.
ensure_mail_build() {
  [ -z "${MAIL_BUILT:-}" ] || return 0
  MAIL_BUILT=1
  if have cargo; then
    capture 900 env -u CARGO_TARGET_DIR cargo build --locked --manifest-path "$ROOT/avery-mail/Cargo.toml"
    [ "$RC" = 0 ] && [ -x "$MAIL_BIN" ]
    check "J0 cargo build: avery-mail (Debug-Binary) ist gebaut" $? "$ERR"
  elif [ -x "$MAIL_BIN" ]; then
    skip "J0 cargo build: avery-mail (Debug-Binary) ist gebaut" "cargo fehlt, das vorhandene Binary wird verwendet"
  else
    fail "J0 cargo build: avery-mail (Debug-Binary) ist gebaut" "cargo fehlt und $MAIL_BIN gibt es nicht"
  fi
}

AVERY_VERSION="$(sed -n 's/^AVERY_VERSION="\([0-9.][0-9.]*\)".*$/\1/p' "$AVERY" | head -n 1)"
CARGO_VERSION="$(sed -n '/^\[package\]/,/^\[/s/^version *= *"\([^"]*\)".*$/\1/p' "$ROOT/avery-mail/Cargo.toml" | head -n 1)"

# ------------------------------------------------------------------ J: bin/avery
run_J() {
  ensure_mail_build
  local home goose f_cfg f_perm f_prov f_tom f_env cfg cfg2 perm tmpl all f weird home4 pwned expected_model
  local nb backup layout linkdir cellar prefix cfg6 v_ok i_ok a_ok

  avery version
  [ "$(trim "$OUT")" = "avery $AVERY_VERSION" ]
  check "J1 avery version entspricht der Version in bin/avery" $? "$OUT"
  avery help
  [ "$RC" = 0 ] && has "$OUT" "Befehle:" && has "$OUT" "avery init"
  check "J2 help listet die Befehle" $? "$OUT"
  avery foo
  [ "$RC" = 2 ] && has "$ERR" "unbekannter Befehl: foo"
  check "J3 unbekannter Befehl: Exit 2 und Meldung" $? "$ERR"
  avery version.
  [ "$RC" = 0 ] && begins "$OUT" "avery "
  check "J4 Satzzeichen am Ende wird toleriert" $? "$OUT"

  avery init
  home="$AHOME"
  goose="$home/.avery/goose"
  f_cfg="$goose/config/config.yaml"
  f_perm="$goose/config/permission.yaml"
  f_prov="$goose/config/custom_providers/custom_splash.json"
  f_tom="$goose/top-of-mind.md"
  f_env="$home/.avery/avery.env"
  all=0
  for f in "$f_cfg" "$f_perm" "$f_prov" "$f_tom" "$f_env"; do [ -e "$f" ] || all=1; done
  [ "$RC" = 0 ] && [ "$all" = 0 ]
  check "J5 init schreibt alle Dateien" $? "$OUT$ERR"
  cfg="$(cat "$f_cfg" 2>/dev/null)"
  lacks "$cfg" "@GOOSE_ROOT@" && lacks "$cfg" "@AVERY_MAIL@" \
    && has "$cfg" "GOOSE_MOIM_MESSAGE_FILE: \"$goose/top-of-mind.md\"" \
    && has "$cfg" "cmd: \"$MAIL_BIN\"" && has "$cfg" "- serve" \
    && has "$cfg" "envs: {}" && lacks "$cfg" "PYTHONUNBUFFERED"
  check "J6 Platzhalter ersetzt, Pfade passen" $? "$(tail -c 700 "$f_cfg" 2>/dev/null)"
  if have ruby; then
    ruby -ryaml -e 'YAML.load_file(ARGV[0])' "$f_cfg" >/dev/null 2>&1 && ruby -ryaml -e 'YAML.load_file(ARGV[0])' "$f_perm" >/dev/null 2>&1
    check "J7 YAML der Konfiguration ist gueltig" $?
  else
    skip "J7 YAML der Konfiguration ist gueltig" "ruby fehlt"
  fi
  perm="$(cat "$f_perm" 2>/dev/null)"
  has "$perm" $'ask_before:\n  - mail__send_message\n  - mail__create_event\n  - mail__update_event\n  - mail__delete_event' && has "$perm" "- mail__create_draft" && has "$perm" "- mail__list_events" && has "$perm" "never_allow: []"
  check "J8 Senden und Kalenderaenderungen stehen auf 'nachfragen', Entwurf erlaubt" $? "$perm"
  [ "$(stat -f '%Lp' "$home/.avery" 2>/dev/null)" = "700" ]
  check "J9 Profil-Ordner nur fuer dich lesbar (700)" $? "$(stat -f '%Lp' "$home/.avery" 2>/dev/null)"
  tmpl="$(cat "$SHARE/goose/config.yaml.tmpl")"
  lacks "$tmpl" "/Users/" && lacks "$tmpl" "private-agent" && lacks "$cfg" "private-agent"
  check "J10 Die Vorlage enthaelt keine persoenlichen Pfade oder Altnamen" $?

  printf '\nEIGENE ZEILE\n' >> "$f_tom"
  avery HOME="$home" init
  [ "$(count "$OUT" "nicht ueberschrieben")" = 5 ] && grep -q 'EIGENE ZEILE' "$f_tom"
  check "J11 zweiter init ueberschreibt nichts" $? "$OUT"
  avery HOME="$home" init --force
  set -- "$goose"/top-of-mind.md.bak-*
  nb=$#
  backup="$1"
  [ -e "$backup" ] || nb=0
  [ "$(count "$OUT" "Sicherung:")" = 5 ] && [ "$nb" = 1 ] && ! grep -q 'EIGENE ZEILE' "$f_tom" && grep -q 'EIGENE ZEILE' "$backup"
  check "J12 init --force sichert und ersetzt" $? "$OUT"
  avery init --bogus
  [ "$RC" = 1 ] && has "$ERR" "Unbekannte Option"
  check "J13 init mit falscher Option scheitert" $? "$ERR"

  weird="$(mkd weird)/a&b|c d"
  avery AVERY_HOME="$weird" init
  cfg2="$(cat "$weird/goose/config/config.yaml" 2>/dev/null)"
  [ "$RC" = 0 ] && has "$cfg2" "GOOSE_MOIM_MESSAGE_FILE: \"$weird/goose/top-of-mind.md\""
  check "J14 Sonderzeichen im Pfad (& | Leerzeichen) bleiben erhalten" $? "$(printf '%s' "$cfg2" | head -c 200)"

  avery AVERY_DRY_RUN=1 mail
  [ "$(trim "$OUT")" = "$MAIL_BIN help" ]
  check "J15 'avery mail' ohne Argument zeigt die Hilfe" $? "$OUT"
  # Without AVERY_MAIL_BIN: bin/avery-mail, then the release build of the checkout, otherwise the path bin/avery-mail.
  layout="$(mkd layout)"
  mkdir -p "$layout/bin"
  cp "$AVERY" "$layout/bin/avery"
  avery AVERY_MAIL_BIN= SCRIPT="$layout/bin/avery" AVERY_DRY_RUN=1 mail status
  v_ok=0
  [ "$(trim "$OUT")" = "$layout/bin/avery-mail status" ] && v_ok=1
  mkdir -p "$layout/avery-mail/target/release"
  printf '#!/bin/sh\nexit 0\n' | write_exec "$layout/avery-mail/target/release/avery-mail"
  avery AVERY_MAIL_BIN= SCRIPT="$layout/bin/avery" AVERY_DRY_RUN=1 mail status
  i_ok=0
  [ "$(trim "$OUT")" = "$layout/avery-mail/target/release/avery-mail status" ] && i_ok=1
  printf '#!/bin/sh\nexit 0\n' | write_exec "$layout/bin/avery-mail"
  avery AVERY_MAIL_BIN= SCRIPT="$layout/bin/avery" AVERY_DRY_RUN=1 mail status
  a_ok=0
  [ "$(trim "$OUT")" = "$layout/bin/avery-mail status" ] && a_ok=1
  [ "$v_ok$i_ok$a_ok" = 111 ]
  check "J15b mail_bin: ohne AVERY_MAIL_BIN gilt bin/avery-mail, dann der Release-Build, sonst bleibt es bei bin/avery-mail" $? "fehlend=$v_ok bin=$a_ok release=$i_ok"
  layout="$(mkd layout)"
  mkdir -p "$layout/bin"
  cp "$AVERY" "$layout/bin/avery"
  avery AVERY_MAIL_BIN= SCRIPT="$layout/bin/avery" mail status
  [ "$RC" = 1 ] && has "$ERR" "avery-mail fehlt: $layout/bin/avery-mail"
  check "J15c 'avery mail' ohne gebautes avery-mail: klare Meldung statt Absturz" $? "$RC $ERR"

  avery mail status
  has "$OUT" "Konfiguration: $AHOME/.avery/mail/config.json (fehlt)" && has "$OUT" "Mail: keine" && has "$OUT" "Kalender: keine"
  check "J16 AVERY_HOME bestimmt den Konfigurationspfad des Mail-Servers" $? "$OUT"
  avery AVERY_HOME="$weird" mail status
  has "$OUT" "Konfiguration: $weird/mail/config.json (fehlt)"
  check "J16b ein gesetztes AVERY_HOME gilt auch fuer den Mail-Server" $? "$OUT"

  expected_model="serve --model unsloth/Qwen3.8-27B-GGUF:UD-Q4_K_M --served-model-name splash-local --max-memory 28G --max-context 100K --max-cache-disk 4G"
  avery AVERY_DRY_RUN=1 model
  ends "$(trim "$OUT")" "$expected_model --offline"
  check "J17 model: Standardwerte wie im bisherigen Setup, offline" $? "$OUT"
  avery AVERY_DRY_RUN=1 AVERY_OFFLINE=0 model
  ends "$(trim "$OUT")" "$expected_model"
  check "J18 model: AVERY_OFFLINE=0 laesst --offline weg" $? "$OUT"
  avery AVERY_DRY_RUN=1 AVERY_OFFLINE=1 pull
  ends "$(trim "$OUT")" "$expected_model" && has "$OUT" "einmalig" && lacks "$OUT" "--offline"
  check "J18b pull laedt immer online, auch bei AVERY_OFFLINE=1" $? "$OUT"
  home4="$(mkd home)"
  mkdir -p "$home4/.avery"
  pwned="$home4/PWNED"
  {
    printf '# Kommentar\nAVERY_MAX_MEMORY=20G\nAVERY_MAX_CONTEXT=8K\nAVERY_BOGUS=1\n'
    printf 'AVERY_MODEL=$(touch %s)\n' "$pwned"
  } > "$home4/.avery/avery.env"
  avery HOME="$home4" AVERY_DRY_RUN=1 AVERY_MAX_CONTEXT=16K model
  has "$OUT" "--max-memory 20G" && has "$OUT" "--max-context 16K"
  check "J19 avery.env: bekannte Werte gelten, die Umgebung hat Vorrang" $? "$OUT"
  [ ! -e "$pwned" ] && has "$OUT" '$(touch'
  check "J20 avery.env wird gelesen, nicht ausgefuehrt" $? "$OUT"

  avery AVERY_DRY_RUN=1 AVERY_NO_ONBOARD=1 app
  [ "$(trim "$OUT")" = "open -n -a /Applications/Goose.app --env GOOSE_PATH_ROOT=$AHOME/.avery/goose --env GOOSE_MOIM_MESSAGE_FILE=$AHOME/.avery/goose/top-of-mind.md" ]
  check "J21 app startet Goose mit dem Avery-Profil" $? "$OUT"
  avery AVERY_DRY_RUN=1 "AVERY_GOOSE_APP=/Opt/Mein Goose.app" app
  has "$OUT" "-a /Opt/Mein Goose.app --env"
  check "J22 AVERY_GOOSE_APP wird beachtet" $? "$OUT"
  avery AVERY_DRY_RUN=1 cli session
  ends "$(trim "$OUT")" "/Contents/Resources/bin/goose session"
  check "J23 cli ruft die Goose-CLI der App auf" $? "$OUT"
  avery doctor
  { [ "$RC" = 0 ] || [ "$RC" = 1 ]; } && has "$OUT" "Avery $CARGO_VERSION" && has "$OUT" "Mail: keine" \
    && has "$OUT" "ok      avery-mail ($MAIL_BIN)" && { has "$OUT" "Alles bereit." || has "$OUT" "Punkt(e) offen."; }
  check "J24 doctor laeuft durch und zeigt Version, Profil und Konten" $? "$OUT$ERR"

  setup_stub
  linkdir="$(mkd link)"
  ln -s "$AVERY" "$linkdir/avery"
  avery SCRIPT="$linkdir/avery" version
  v_ok=1
  [ "$(trim "$OUT")" = "avery $CARGO_VERSION" ] || v_ok=0
  avery SCRIPT="$linkdir/avery" init
  i_ok=1
  [ "$RC" = 0 ] && [ -f "$AHOME/.avery/goose/config/config.yaml" ] && [ -f "$AHOME/.avery/goose/top-of-mind.md" ] || i_ok=0
  avery SCRIPT="$linkdir/avery" "AVERY_APP_DIR=$linkdir/apps" "AVERY_APP_COMMAND=$STUB" AVERY_MACOS_VERSION=26.0 install-app
  a_ok=1
  [ "$RC" = 0 ] && [ -d "$linkdir/apps/Avery.app" ] || a_ok=0
  [ "$v_ok$i_ok$a_ok" = 111 ]
  check "J25 Aufruf ueber Symlink (wie in Homebrew) findet libexec und share" $? "version=$v_ok share=$i_ok libexec=$a_ok $ERR"

  # Homebrew layout: the keg holds bin/avery and bin/avery-mail, the Homebrew bin directory holds symlinks to them.
  cellar="$(mkd cellar)"
  prefix="$cellar/Cellar/avery/$CARGO_VERSION"
  mkdir -p "$prefix/bin" "$prefix/libexec" "$cellar/bin"
  cp "$AVERY" "$prefix/bin/avery"
  cp "$MAKE_APP" "$prefix/libexec/avery-make-app"
  cp -R "$ROOT/share" "$prefix/share"
  printf '#!/bin/sh\nexit 0\n' | write_exec "$prefix/bin/avery-mail"
  ln -s "$prefix/bin/avery" "$cellar/bin/avery"
  ln -s "$prefix/bin/avery-mail" "$cellar/bin/avery-mail"
  avery AVERY_MAIL_BIN= SCRIPT="$cellar/bin/avery" "PATH=$cellar/bin:$PATH" init
  cfg6="$(cat "$AHOME/.avery/goose/config/config.yaml" 2>/dev/null)"
  has "$cfg6" "cmd: \"$cellar/bin/avery-mail\"" && lacks "$cfg6" "/Cellar/"
  check "J26 Homebrew-Layout: die Konfiguration nutzt den stabilen Symlink statt des Cellar-Pfads" $? "$(printf '%s' "$cfg6" | tail -c 400)$ERR"
}

# ------------------------------------------------------------------ L: nothing personal or secret in the repository
# Files below DIR that the hygiene checks inspect; the remaining arguments go to find.
scan_files() {
  local dir="$1"
  shift
  find "$dir" \( -name .git -o -path "$dir/avery-mail/target" -o -path "$dir/dist" \) -prune \
    -o -type f ! -path "$SELF" "$@"
}

# l1_scan DIR: one line per finding ("path: pattern" or "path: address"); a failing tool counts as a finding.
l1_scan() {
  local dir="$1" pattern file errors="$TT/scan.err"
  : > "$errors"
  for pattern in \
    '/Users/' \
    'gh''o_[A-Za-z0-9]{20}' 'gh''p_[A-Za-z0-9]{20}' 'github''_pat_' \
    '-----BEGIN [A-Z ]*PRIVATE KEY' 'aiello''\.ch' '@micro''soft\.com'; do
    scan_files "$dir" -exec grep -I -l -E -e "$pattern" {} + 2>>"$errors" \
      | while IFS= read -r file; do printf '%s: %s\n' "${file#"$dir"/}" "$pattern"; done
  done
  scan_files "$dir" -exec grep -I -o -H -E -e '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' {} + 2>>"$errors" \
    | awk -v root="$dir/" '
      {
        line = $0
        if (index(line, root) == 1) line = substr(line, length(root) + 1)
        n = split(line, parts, ":")
        found = parts[n]
        domain = tolower(substr(found, index(found, "@") + 1))
        if (domain ~ /\.example$/) next
        if (domain == "example.org" || domain == "example.com" || domain == "example.invalid" || domain == "github.com") next
        if (domain ~ /^[0-9]x\.(png|tiff?)$/) next
        if (!seen[line]++) print substr(line, 1, length(line) - length(found) - 1) ": " found
      }' || echo "awk ist fehlgeschlagen"
  [ ! -s "$errors" ] || echo "die Suche meldet Fehler: $(head -n 1 "$errors")"
}

# python_scan DIR: files that mention python (in any case) or PYTHONUNBUFFERED.
python_scan() {
  local dir="$1" file errors="$TT/scan.err"
  : > "$errors"
  scan_files "$dir" -exec grep -I -i -l -E -e 'python|PYTHONUNBUFFERED' {} + 2>>"$errors" \
    | while IFS= read -r file; do printf '%s\n' "${file#"$dir"/}"; done
  [ ! -s "$errors" ] || echo "die Suche meldet Fehler: $(head -n 1 "$errors")"
}

# Both checks prove an absence, so first make sure the searches find planted offences and skip what they should.
scan_selftest() {
  local d="$TT/canary" found f
  rm -rf "$d"
  mkdir -p "$d/src" "$d/.git" "$d/dist" "$d/avery-mail/target"
  printf 'pfad /Users/jane/Documents\n' > "$d/src/path.txt"
  printf 'token %s%s\n' 'gh''p_' 'abcdefghijklmnopqrst' > "$d/src/token.txt"
  printf '%sBEGIN RSA PRIVATE KEY-----\n' '-----' > "$d/src/key.txt"
  printf 'mail someone@evil.test\n' > "$d/src/mail.txt"
  printf 'Python3 und PYTHONUNBUFFERED\n' > "$d/src/py.txt"
  printf '/home/x/.avery demo@gmail.example me@example.org git@github.com icon_16x16@2x.png\n' > "$d/src/ok.txt"
  printf '\0/Users/jane python someone@evil.test\0\n' > "$d/src/binary.dat"
  for f in "$d/.git/c" "$d/dist/c" "$d/avery-mail/target/c"; do printf '/Users/jane python someone@evil.test\n' > "$f"; done
  found="$(l1_scan "$d")"
  has "$found" "src/path.txt" && has "$found" "src/token.txt" && has "$found" "src/key.txt" \
    && has "$found" "src/mail.txt: someone@evil.test" || return 1
  lacks "$found" "ok.txt" && lacks "$found" "binary.dat" && lacks "$found" ".git" && lacks "$found" "dist" \
    && lacks "$found" "target" || return 1
  [ "$(python_scan "$d")" = "src/py.txt" ]
}

run_L() {
  local selftest found note=""
  scan_selftest
  selftest=$?
  [ "$selftest" = 0 ] || note="Selbsttest der Suche fehlgeschlagen. "
  found="$(l1_scan "$ROOT")"
  [ "$selftest" = 0 ] && [ -z "$found" ]
  check "L1 keine persoenlichen Adressen, Pfade oder Zugangsdaten im Projekt" $? "$note$(printf '%s' "$found" | head -n 5)"
  found="$(python_scan "$ROOT")"
  [ "$selftest" = 0 ] && [ -z "$found" ]
  check "L2 kein Python und kein PYTHONUNBUFFERED im Projekt" $? "$note$(printf '%s' "$found" | head -n 5)"
}

# ------------------------------------------------------------------ N: service commands and YAML check
run_N() {
  local empty_cache full_cache only_foo home_y broken_path formula_text brew_dir brew_log
  setup_health

  avery AVERY_DRY_RUN=1 "AVERY_MODEL_URL=$DEAD_URL" up
  any_line_ends "$OUT" "services run avery" && begins "$(last_line "$OUT")" "open -n -a"
  check "N1 up (Modell aus): startet erst den Dienst, dann Goose" $? "$OUT"
  avery AVERY_DRY_RUN=1 "AVERY_MODEL_URL=$ALIVE_URL" up
  has "$OUT" "laeuft bereits" && lacks "$OUT" "services run" && has "$OUT" "open -n -a"
  check "N2 up (Modell laeuft): kein zweiter Start, nur Goose" $? "$OUT"
  avery AVERY_DRY_RUN=1 down
  has "$OUT" "services stop avery" && has "$OUT" "Cmd+Q"
  check "N3 down beendet den Dienst" $? "$OUT"
  avery "AVERY_MODEL_URL=$ALIVE_URL" doctor
  has "$OUT" "ok      Modellserver laeuft"
  check "N4 doctor: Modell erreichbar" $? "$OUT"
  avery help
  has "$OUT" "avery up" && has "$OUT" "avery down"
  check "N5 help nennt up und down" $? "$OUT"

  # Without a cached model "up" must stop at once. A stand-in for brew makes sure that nothing real is started
  # even if it did not (it is never called, and it would fail).
  empty_cache="$(mkd cache)"
  brew_dir="$(mkd brew)"
  brew_log="$brew_dir/brew.log"
  write_exec "$brew_dir/brew" <<EOF
#!/bin/sh
echo "\$@" >> "$brew_log"
exit 1
EOF
  avery "AVERY_MODEL_URL=$DEAD_URL" "HF_HOME=$empty_cache" "PATH=$brew_dir:$PATH" AVERY_UP_TIMEOUT=3 up
  [ "$RC" = 1 ] && has "$ERR" "avery pull" && lacks "$OUT" "services run" && [ ! -e "$brew_log" ]
  check "N5b up ohne geladenes Modell: sofortiger Hinweis auf 'avery pull', kein Dienststart" $? "$OUT$ERR"
  full_cache="$(mkd cache)"
  mkdir -p "$full_cache/hub/models--unsloth--Qwen3.8-27B-GGUF/snapshots"
  avery "AVERY_MODEL_URL=$DEAD_URL" "HF_HOME=$full_cache" doctor
  has "$OUT" "ok      Modell im Cache"
  check "N5c doctor: Modell im Cache erkannt" $? "$OUT"
  avery "AVERY_MODEL_URL=$DEAD_URL" "HF_HOME=$empty_cache" doctor
  has "$OUT" "Modell nicht im Cache -> avery pull"
  check "N5d doctor: fehlendes Modell mit Hinweis auf pull" $? "$OUT"
  only_foo="$(mkd cache)"
  mkdir -p "$only_foo/hub/models--foo--bar/snapshots"
  avery "AVERY_MODEL_URL=$DEAD_URL" "HF_HOME=$only_foo" AVERY_MODEL=foo/bar:Q4 doctor
  has "$OUT" "ok      Modell im Cache"
  check "N5e Cache-Pruefung folgt AVERY_MODEL" $? "$OUT"

  if have ruby; then
    avery init
    home_y="$AHOME"
    avery HOME="$home_y" "AVERY_MODEL_URL=$ALIVE_URL" doctor
    has "$OUT" "YAML gueltig: config.yaml" && lacks "$OUT" "UNGUELTIG"
    check "N6 doctor: gueltiges YAML wird bestaetigt" $? "$OUT"
    broken_path="$home_y/.avery/goose/config/config.yaml"
    sed 's/description: Private Mail und Kalender (Gmail, Outlook.com; lesen/description: Private Mail und Kalender (Gmail, Outlook.com: lesen/' "$broken_path" > "$broken_path.new"
    ! cmp -s "$broken_path" "$broken_path.new"
    check "N7 (Testaufbau) der Fehler vom 12.11. laesst sich nachstellen" $?
    mv "$broken_path.new" "$broken_path"
    avery HOME="$home_y" "AVERY_MODEL_URL=$ALIVE_URL" doctor
    [ "$RC" = 1 ] && has "$OUT" "YAML UNGUELTIG"
    check "N8 doctor erkennt ungueltiges YAML und meldet es" $? "$OUT"
  else
    skip "N6 doctor: gueltiges YAML wird bestaetigt" "ruby fehlt"
    skip "N7 (Testaufbau) der Fehler vom 12.11. laesst sich nachstellen" "ruby fehlt"
    skip "N8 doctor erkennt ungueltiges YAML und meldet es" "ruby fehlt"
  fi

  formula_text="$(cat "$ROOT/Formula/avery.rb")"
  has "$formula_text" "service do" && has "$formula_text" 'run [opt_bin/"avery", "model"]' \
    && has "$formula_text" "keep_alive crashed: true" && has "$formula_text" "std_service_path_env"
  check "N9 Formel definiert den Modell-Dienst" $?
  has "$formula_text" 'depends_on "rust" => :build' && has "$formula_text" 'system "cargo", "install"' \
    && has "$formula_text" 'std_cargo_args(path: "avery-mail")'
  check "N9b Formel baut den Mail-Server mit cargo (Rust nur zum Bauen)" $?
  lacks "$formula_text" "libexec/avery-mail" && ! has_i "$formula_text" "python"
  check "N9c Formel enthaelt keine Reste des alten Mail-Servers (libexec/avery-mail, Python)" $?
}

# ------------------------------------------------------------------ P: the Avery app (one icon, own icon)
# p15 LABEL EXPECTED ARGS...: avery-make-app must fail with exit 1 and EXPECTED on stderr.
p15() {
  local label="$1" expected="$2"
  shift 2
  capture 60 "$MAKE_APP" "$@"
  [ "$RC" = 1 ] && has "$ERR" "$expected"
  check "P15 avery-make-app $label -> $expected" $? "$ERR"
}

run_P() {
  local app_root apps start_app source info keep unreadable cask_app home_dup same_home same_app first pair
  local label bad_path version_string expected_icon icon_apps
  setup_stub
  app_root="$(mkd app)"
  apps="$app_root/Applications"
  mkdir -p "$apps/Avery beenden.app"
  # The version is pinned: it decides which icon install-app picks (P11 tests both).
  avery "AVERY_APP_DIR=$apps" "AVERY_APP_COMMAND=$STUB" AVERY_MACOS_VERSION=26.0.1 install-app
  start_app="$apps/Avery.app"
  info="$start_app/Contents/Info.plist"
  [ "$RC" = 0 ] && [ "$(ls -A "$apps")" = "Avery.app" ] && has "$OUT" "entfernt"
  check "P1 genau eine App; die alte 'Avery beenden.app' ist entfernt" $? "$OUT$ERR"
  source="$(osadecompile "$start_app" 2>/dev/null)"
  has "$source" "set cliPath to \"$STUB\"" && has "$source" 'quoted form of cliPath & " up 2>&1"' \
    && has "$source" "display dialog" && lacks "$source" " down"
  check "P2 die App ruft 'avery up' auf und zeigt Fehler in einem Dialog" $? "$source"
  has "$source" 'do shell script "test -x " & quoted form of cliPath' && has "$source" "brew install flavioaiello/avery/avery"
  check "P2b fehlt die Kommandozeile, erklaert die App, was zu installieren ist" $? "$source"
  [ "$(plist_get "$info" CFBundleName)" = "Avery" ] && [ "$(plist_get "$info" CFBundleDisplayName)" = "Avery" ] \
    && [ "$(plist_get "$info" CFBundleIdentifier)" = "io.github.flavioaiello.avery" ] \
    && [ "$(plist_get "$info" CFBundleShortVersionString)" = "$CARGO_VERSION" ]
  check "P3 Name, Kennung und Version sind gesetzt" $? "$(plutil -p "$info" 2>/dev/null)"
  cmp -s "$SHARE/Avery.icns" "$start_app/Contents/Resources/applet.icns"
  check "P4 eigenes Icon ist eingebaut" $?
  codesign --verify --deep --strict "$start_app" >/dev/null 2>&1
  check "P5 die Signatur der App ist gueltig" $?
  capture 60 osascript "$start_app/Contents/Resources/Scripts/main.scpt"
  first="$(head -n 1 "$STUB_LOG" 2>/dev/null)"
  [ "$RC" = 0 ] && [ "$first" = "up" ]
  check "P6 die App fuehrt den Befehl aus (im Erfolgsfall ohne Dialog)" $? "rc=$RC stdout=$OUT stderr=$ERR aufruf=$first"
  avery "AVERY_APP_DIR=$apps" "AVERY_APP_COMMAND=$STUB" AVERY_MACOS_VERSION=26.0.1 install-app
  [ "$RC" = 0 ] && [ "$(ls -A "$apps")" = "Avery.app" ] && lacks "$OUT" "entfernt"
  check "P7 install-app ist wiederholbar" $? "$OUT"
  avery "AVERY_APP_DIR=$app_root/x" 'AVERY_APP_COMMAND=/tmp/a"b' install-app
  [ "$RC" = 1 ] && has "$ERR" "Anfuehrungszeichen"
  check "P8 Pfad mit Anfuehrungszeichen wird abgelehnt" $? "$ERR"
  for pair in 'ein einzelner Backslash|/x/avery\' 'ein Anfuehrungszeichen|/x/a"b'; do
    label="${pair%%|*}"
    bad_path="${pair#*|}"
    capture 60 "$MAKE_APP" --output "$app_root/y.app" --command "$bad_path"
    [ "$RC" = 1 ] && has "$ERR" "Anfuehrungszeichen oder Backslashes"
    check "P8b avery-make-app lehnt $label im Pfad ab" $? "$ERR"
  done
  keep="$app_root/keep.app"
  mkdir -p "$keep/Contents"
  printf 'alt' > "$keep/Contents/marker"
  unreadable="$app_root/unreadable.icns"
  printf 'x' > "$unreadable"
  chmod 0 "$unreadable"
  capture 60 "$MAKE_APP" --output "$keep" --icon "$unreadable"
  [ "$RC" != 0 ] && [ -e "$keep/Contents/marker" ]
  check "P8c ein fehlgeschlagener Bau laesst eine vorhandene App unangetastet" $? "$OUT$ERR"
  chmod 600 "$unreadable"
  avery help
  has "$OUT" "avery install-app" && lacks "$OUT" "beenden.app"
  check "P9 help nennt install-app, nicht mehr 'beenden.app'" $? "$OUT"

  [ ! -e "$start_app/Contents/Resources/Assets.car" ] && ! plist_get "$info" CFBundleIconName >/dev/null
  check "P10 kein Asset-Katalog und keine CFBundleIconName (sonst zeigt macOS das Skript-Icon statt Avery)" $? "$(plutil -p "$info" 2>/dev/null)"
  for pair in "26.0.1 Avery.icns" "15.4 AveryClassic.icns"; do
    version_string="${pair% *}"
    expected_icon="${pair#* }"
    icon_apps="$app_root/icons-$version_string"
    avery "AVERY_APP_DIR=$icon_apps" "AVERY_APP_COMMAND=$STUB" "AVERY_MACOS_VERSION=$version_string" install-app
    [ "$RC" = 0 ] && cmp -s "$SHARE/$expected_icon" "$icon_apps/Avery.app/Contents/Resources/applet.icns"
    check "P11 macOS $version_string bekommt $expected_icon" $? "$OUT$ERR"
  done
  ! cmp -s "$SHARE/Avery.icns" "$SHARE/AveryClassic.icns"
  check "P11b die beiden Icons unterscheiden sich" $?

  # With the cask installed, install-app must not create a second icon and removes a leftover copy.
  cask_app="$app_root/Programme/Avery.app"
  mkdir -p "$cask_app"
  home_dup="$(mkd home)"
  mkdir -p "$home_dup/Applications/Avery.app"
  avery HOME="$home_dup" "AVERY_SYSTEM_APP=$cask_app" install-app
  [ "$RC" = 0 ] && has "$OUT" "schon installiert" && [ ! -e "$home_dup/Applications/Avery.app" ]
  check "P12 mit installiertem Cask legt install-app keine zweite App an und raeumt die Kopie ab" $? "$OUT$ERR"
  same_home="$(mkd home)"
  same_app="$same_home/Applications/Avery.app"
  mkdir -p "$same_app"
  avery HOME="$same_home" "AVERY_SYSTEM_APP=$same_app" install-app
  [ "$RC" = 0 ] && [ -d "$same_app" ] && lacks "$OUT" "entfernt"
  check "P12b install-app loescht nie die App des Cask, auch wenn beide Pfade zusammenfallen" $? "$OUT$ERR"
  avery "AVERY_SYSTEM_APP=$cask_app" doctor
  has "$OUT" "ok      Avery.app ($cask_app)"
  check "P13 doctor findet die App im Programme-Ordner" $? "$OUT"
  avery "AVERY_SYSTEM_APP=$app_root/nirgends/Avery.app" doctor
  has "$OUT" "Hinweis Avery.app fehlt" && has "$OUT" "brew install --cask flavioaiello/avery/avery" && lacks "$OUT" "FEHLT   Avery.app"
  check "P14 fehlt die App, ist das nur ein Hinweis und zaehlt nicht als fehlend" $? "$OUT"
  p15 "(ohne Argumente)" "--output fehlt"
  p15 "--output <tmp>/x" "muss auf .app enden" --output "$app_root/x"
  p15 "--output <tmp>/x.app" "absoluter Pfad" --output "$app_root/x.app" --command relativ
  p15 "--output <tmp>/x.app" "Icon nicht gefunden" --output "$app_root/x.app" --icon "$app_root/gibt-es-nicht.icns"
  p15 "--bogus" "unbekannte Option" --bogus
}

# ------------------------------------------------------------------ G: opening Goose with the right profile (fake Goose processes)
open_calls() { if [ -f "$OPEN_LOG" ]; then trim "$(cat "$OPEN_LOG")"; fi; }

run_G() {
  local gp calls
  local -a env_g
  setup_goose
  env_g=("AVERY_GOOSE_APP=$FAKE_APP" "AVERY_GOOSE_ROOT=$PROFILE" "AVERY_MODEL_URL=$DEAD_URL" "PATH=$GOOSE_DIR/stubs:$PATH")
  rm -f "$OPEN_LOG"

  fake_goose "$FAKE_APP" "$PROFILE"
  gp="$FAKE_PID"
  avery "${env_g[@]}" app
  calls="$(open_calls)"
  [ "$RC" = 0 ] && has "$OUT" "laeuft bereits mit dem Avery-Profil" && [ "$calls" = "-a $FAKE_APP" ]
  check "G1 Goose laeuft schon mit dem Avery-Profil: nur in den Vordergrund holen" $? "stdout=$OUT stderr=$ERR open=$calls"
  stop_process "$gp"
  rm -f "$OPEN_LOG"
  fake_goose "$FAKE_APP" "$GOOSE_DIR/anderes-profil"
  gp="$FAKE_PID"
  avery "${env_g[@]}" app
  calls="$(open_calls)"
  [ "$RC" = 1 ] && has "$ERR" "anderen Profil" && [ -z "$calls" ]
  check "G2 Goose laeuft mit einem anderen Profil: klare Meldung, nichts gestartet" $? "stdout=$OUT stderr=$ERR open=$calls"
  stop_process "$gp"
  avery "${env_g[@]}" app
  calls="$(open_calls)"
  [ "$RC" = 0 ] && [ "$calls" = "-n -a $FAKE_APP --env GOOSE_PATH_ROOT=$PROFILE --env GOOSE_MOIM_MESSAGE_FILE=$PROFILE/top-of-mind.md" ]
  check "G3 Goose laeuft nicht: Start mit dem Avery-Profil" $? "stdout=$OUT stderr=$ERR open=$calls"
}

# ------------------------------------------------------------------ O: the setup dialogs (the parts that need no screen)
# The dialogs themselves are never shown here: every run below is in a state in which "onboard --if-needed" is silent
# (something is set up, dismissed, switched off, demo mode) or fails before it could ask.
# line_of TEXT NEEDLE: number of the first line of TEXT that contains NEEDLE (empty if none).
line_of() { printf '%s\n' "$1" | grep -n -F -- "$2" | head -n 1 | cut -d: -f1; }
# before TEXT FIRST SECOND: 0 if both occur and FIRST comes on an earlier line than SECOND.
before() {
  local a b
  a="$(line_of "$1" "$2")"
  b="$(line_of "$1" "$3")"
  [ -n "$a" ] && [ -n "$b" ] && [ "$a" -lt "$b" ]
}

run_O() {
  local want home dismissed_home broken_home mailstub mailstub_log failing quiet missing calls apps source
  local -a env_o
  ensure_mail_build
  setup_health
  setup_goose
  want="$MAIL_BIN onboard --if-needed"

  avery AVERY_DRY_RUN=1 AVERY_NO_ONBOARD= "AVERY_MODEL_URL=$DEAD_URL" up
  [ "$(count "$OUT" "$want")" = 1 ] && before "$OUT" "services run avery" "$want" && before "$OUT" "$want" "open -n -a"
  check "O1 up (Modell aus): die Einrichtung in Dialogen kommt nach dem Start des Dienstes und vor Goose" $? "$OUT"
  avery AVERY_DRY_RUN=1 AVERY_NO_ONBOARD= "AVERY_MODEL_URL=$ALIVE_URL" up
  [ "$(count "$OUT" "$want")" = 1 ] && before "$OUT" "laeuft bereits" "$want" && before "$OUT" "$want" "open -n -a"
  check "O2 up (Modell laeuft): die Einrichtung kommt einmal, vor Goose" $? "$OUT"
  avery AVERY_DRY_RUN=1 AVERY_NO_ONBOARD= app
  [ "$(printf '%s\n' "$OUT" | wc -l | tr -d ' ')" = 2 ] && [ "$(printf '%s\n' "$OUT" | head -n 1)" = "$want" ] && begins "$(last_line "$OUT")" "open -n -a"
  check "O3 app: erst 'onboard --if-needed', dann Goose" $? "$OUT"
  avery AVERY_DRY_RUN=1 AVERY_NO_ONBOARD=1 "AVERY_MODEL_URL=$DEAD_URL" up
  lacks "$OUT" "onboard --if-needed" && has "$OUT" "services run avery" && has "$OUT" "open -n -a"
  check "O4 AVERY_NO_ONBOARD=1: up startet ohne Einrichtung" $? "$OUT"
  avery AVERY_DRY_RUN=1 AVERY_NO_ONBOARD=1 app
  lacks "$OUT" "onboard --if-needed" && begins "$(trim "$OUT")" "open -n -a"
  check "O5 AVERY_NO_ONBOARD=1: app startet ohne Einrichtung" $? "$OUT"
  home="$(mkd home)"
  mkdir -p "$home/.avery"
  printf 'AVERY_NO_ONBOARD=1\n' > "$home/.avery/avery.env"
  avery HOME="$home" AVERY_DRY_RUN=1 AVERY_NO_ONBOARD= app
  lacks "$OUT" "onboard --if-needed"
  check "O6 AVERY_NO_ONBOARD=1 in avery.env gilt wie in der Umgebung" $? "$OUT"
  avery HOME="$home" AVERY_DRY_RUN=1 AVERY_NO_ONBOARD=0 app
  has "$OUT" "onboard --if-needed"
  check "O7 die Umgebung hat Vorrang vor avery.env (AVERY_NO_ONBOARD=0)" $? "$OUT"

  # The start is never blocked: a failing, a succeeding and a missing mail server (stand-ins, no dialog).
  env_o=("AVERY_GOOSE_APP=$FAKE_APP" "AVERY_GOOSE_ROOT=$PROFILE" "AVERY_MODEL_URL=$DEAD_URL" "PATH=$GOOSE_DIR/stubs:$PATH")
  mailstub="$(mkd mailstub)"
  mailstub_log="$mailstub/calls.log"
  failing="$mailstub/failing"
  write_exec "$failing" <<STUB
#!/bin/sh
echo "\$@" >> "$mailstub_log"
echo "boom" >&2
exit 1
STUB
  quiet="$mailstub/quiet"
  write_exec "$quiet" <<STUB
#!/bin/sh
echo "\$@" >> "$mailstub_log"
exit 0
STUB
  missing="$mailstub/nirgends"
  rm -f "$OPEN_LOG" "$mailstub_log"
  avery "${env_o[@]}" AVERY_NO_ONBOARD= "AVERY_MAIL_BIN=$failing" app
  calls="$(open_calls)"
  [ "$RC" = 0 ] && [ "$(trim "$(cat "$mailstub_log" 2>/dev/null)")" = "onboard --if-needed" ] \
    && [ "$calls" = "-n -a $FAKE_APP --env GOOSE_PATH_ROOT=$PROFILE --env GOOSE_MOIM_MESSAGE_FILE=$PROFILE/top-of-mind.md" ] \
    && has "$ERR" "Hinweis: Die Einrichtung in Dialogen ist nicht abgeschlossen"
  check "O8 eine fehlgeschlagene Einrichtung blockiert den Start nicht: Hinweis, Goose startet" $? "rc=$RC open=$calls stderr=$ERR"
  rm -f "$OPEN_LOG" "$mailstub_log"
  avery "${env_o[@]}" AVERY_NO_ONBOARD= "AVERY_MAIL_BIN=$quiet" app
  calls="$(open_calls)"
  [ "$RC" = 0 ] && [ "$(trim "$(cat "$mailstub_log" 2>/dev/null)")" = "onboard --if-needed" ] && lacks "$ERR" "Einrichtung in Dialogen" \
    && begins "$calls" "-n -a $FAKE_APP"
  check "O9 eine abgeschlossene oder uebersprungene Einrichtung (Exit 0): Goose startet ohne Hinweis" $? "rc=$RC open=$calls stderr=$ERR"
  rm -f "$OPEN_LOG" "$mailstub_log"
  avery "${env_o[@]}" AVERY_NO_ONBOARD= "AVERY_MAIL_BIN=$missing" app
  calls="$(open_calls)"
  [ "$RC" = 0 ] && [ ! -e "$mailstub_log" ] && begins "$calls" "-n -a $FAKE_APP"
  check "O10 fehlt das Mail-Programm, startet Goose trotzdem (die Einrichtung wird uebersprungen)" $? "rc=$RC open=$calls stderr=$ERR"
  rm -f "$OPEN_LOG" "$mailstub_log"
  avery "${env_o[@]}" AVERY_NO_ONBOARD=1 "AVERY_MAIL_BIN=$failing" app
  calls="$(open_calls)"
  [ "$RC" = 0 ] && [ ! -e "$mailstub_log" ] && begins "$calls" "-n -a $FAKE_APP"
  check "O11 AVERY_NO_ONBOARD=1 ruft das Mail-Programm gar nicht erst auf" $? "rc=$RC open=$calls stderr=$ERR"

  # The real mail server, only in states without any question.
  home="$(mkd home)"
  mkdir -p "$home/.avery/mail"
  printf '{"version": 3, "apple": {"calendars": [{"id": "C1", "name": "Privat", "level": "read"}]}}\n' > "$home/.avery/mail/config.json"
  avery HOME="$home" AVERY_NO_ONBOARD= mail onboard --if-needed
  [ "$RC" = 0 ] && [ -z "$OUT" ] && [ -z "$ERR" ]
  check "O12 onboard --if-needed schweigt, wenn schon etwas eingerichtet ist (Apple-Eintrag)" $? "rc=$RC $OUT$ERR"
  home="$(mkd home)"
  mkdir -p "$home/.avery/mail"
  printf '{"version": 3, "accounts": [{"provider": "microsoft", "address": "bob@outlook.example"}]}\n' > "$home/.avery/mail/config.json"
  avery HOME="$home" AVERY_NO_ONBOARD= mail onboard --if-needed
  [ "$RC" = 0 ] && [ -z "$OUT" ] && [ -z "$ERR" ]
  check "O13 onboard --if-needed schweigt, wenn ein Konto verbunden ist" $? "rc=$RC $OUT$ERR"
  dismissed_home="$(mkd home)"
  mkdir -p "$dismissed_home/.avery/mail"
  : > "$dismissed_home/.avery/mail/onboard.dismissed"
  avery HOME="$dismissed_home" AVERY_NO_ONBOARD= mail onboard --if-needed
  [ "$RC" = 0 ] && [ -z "$OUT" ] && [ -z "$ERR" ]
  check "O14 onboard --if-needed schweigt nach 'Nicht mehr fragen' (onboard.dismissed)" $? "rc=$RC $OUT$ERR"
  avery HOME="$(mkd home)" AVERY_NO_ONBOARD=1 mail onboard --if-needed
  [ "$RC" = 0 ] && [ -z "$OUT" ] && [ -z "$ERR" ]
  check "O15 onboard --if-needed schweigt mit AVERY_NO_ONBOARD=1" $? "rc=$RC $OUT$ERR"
  home="$(mkd home)"
  avery HOME="$home" AVERY_NO_ONBOARD= MAIL_MCP_FAKE=1 mail onboard --if-needed
  [ "$RC" = 0 ] && [ -z "$OUT" ] && [ -z "$ERR" ] && [ ! -e "$home/.avery" ]
  check "O16 onboard --if-needed schweigt im Demo-Modus und legt nichts an" $? "rc=$RC $OUT$ERR"
  avery HOME="$home" MAIL_MCP_FAKE=1 mail onboard
  [ "$RC" = 1 ] && has "$OUT" "Demo-Modus" && [ ! -e "$home/.avery" ]
  check "O17 onboard ohne Option verweigert im Demo-Modus, bevor ein Dialog erscheint" $? "rc=$RC $OUT$ERR"
  avery mail onboard --bogus
  [ "$RC" = 2 ] && has "$OUT" "Benutzung: avery mail onboard [--if-needed]"
  check "O18 onboard mit unbekannter Option: Benutzungshinweis, Exit 2" $? "rc=$RC $OUT"
  broken_home="$(mkd home)"
  mkdir -p "$broken_home/.avery/mail"
  printf '{"version": 2}\n' > "$broken_home/.avery/mail/config.json"
  avery HOME="$broken_home" AVERY_NO_ONBOARD= mail onboard --if-needed
  [ "$RC" = 1 ] && begins "$OUT" "Fehler: " && [ "$(cat "$broken_home/.avery/mail/config.json")" = '{"version": 2}' ]
  check "O19 onboard --if-needed meldet eine unlesbare Konfiguration, statt Dialoge zu zeigen, und aendert sie nicht" $? "rc=$RC $OUT"
  avery mail help
  has "$OUT" "avery mail onboard [--if-needed]" && has "$OUT" "onboard and revoke refuse in demo mode"
  check "O20 die Hilfe des Mail-Programms nennt onboard" $? "$OUT"
  avery help
  has "$OUT" "onboard, link, unlink" && has "$OUT" "AVERY_NO_ONBOARD=1" && has "$OUT" "avery mail onboard"
  check "O21 die Hilfe von avery nennt die Einrichtung in Dialogen und den Schalter" $? "$OUT"

  if have osacompile; then
    setup_stub
    apps="$(mkd app)"
    avery "AVERY_APP_DIR=$apps" "AVERY_APP_COMMAND=$STUB" AVERY_MACOS_VERSION=26.0 install-app
    source="$(osadecompile "$apps/Avery.app" 2>/dev/null)"
    [ "$RC" = 0 ] && has "$source" "set cliPath to \"$STUB\"" && has "$source" 'quoted form of cliPath & " up 2>&1"'
    check "O22 Avery.app startet 'avery up', den Weg mit der Einrichtung (O1, O2)" $? "$source"
  else
    skip "O22 Avery.app startet 'avery up', den Weg mit der Einrichtung (O1, O2)" "osacompile fehlt"
  fi
  has "$(cat "$ROOT/Casks/avery.rb")" "avery mail onboard" && has "$(cat "$ROOT/Casks/avery.rb")" "AVERY_NO_ONBOARD=1" \
    && has "$(cat "$ROOT/Casks/avery.rb")" "macOS may ask for a permission" && has "$(cat "$ROOT/Casks/avery.rb")" "If no dialog appears" \
    && lacks "$(cat "$ROOT/Casks/avery.rb")" "System Events" && has "$(cat "$ROOT/Casks/avery.rb")" "Nicht mehr fragen"
  check "O23 Cask: die Hinweise nennen die Dialoge, den Schalter, die moegliche Berechtigung und den Weg, wenn kein Dialog erscheint" $?
  has "$(cat "$ROOT/Formula/avery.rb")" "avery mail onboard" && has "$(cat "$ROOT/Formula/avery.rb")" "AVERY_NO_ONBOARD=1" \
    && has "$(cat "$ROOT/Formula/avery.rb")" "macOS may ask for a permission" && has "$(cat "$ROOT/Formula/avery.rb")" "If no dialog appears" \
    && lacks "$(cat "$ROOT/Formula/avery.rb")" "System Events"
  check "O24 Formel: die Hinweise nennen die Dialoge, den Schalter, die moegliche Berechtigung und den Weg, wenn kein Dialog erscheint" $?
  has "$(cat "$SHARE/avery.env")" "AVERY_NO_ONBOARD"
  check "O25 die Vorlage avery.env nennt AVERY_NO_ONBOARD" $?
  has "$(cat "$ROOT/README.md")" "avery mail onboard --if-needed" && has "$(cat "$ROOT/README.md")" "onboard.dismissed" \
    && has "$(cat "$ROOT/README.md")" "macOS may still ask for a permission" && has "$(cat "$ROOT/README.md")" "If no dialog appears" \
    && lacks "$(cat "$ROOT/README.md")" "System Events" && has "$(cat "$ROOT/README.md")" "AVERY_NO_ONBOARD=1"
  check "O26 README: die Dialoge, der Schalter, die Markierung und die moegliche Berechtigung sind beschrieben, ohne System Events" $?
}

# ------------------------------------------------------------------ W: the watcher stops the model after Goose was closed
run_W() {
  local gp wpid w1home w3home output still ended
  local -a env_w
  setup_goose
  setup_health
  env_w=(AVERY_DRY_RUN=1 AVERY_WATCH_INTERVAL=1 AVERY_IDLE_SECONDS=3 "AVERY_GOOSE_APP=$FAKE_APP" "AVERY_MODEL_URL=$ALIVE_URL")

  avery "${env_w[@]}" watch
  w1home="$AHOME"
  [ "$RC" = 0 ] && has "$OUT" "services stop avery" && has "$OUT" "Modell wird gestoppt" && [ ! -e "$w1home/.avery/watch.pid" ]
  check "W1 ohne Goose: das Modell wird nach der Wartezeit gestoppt" $? "$OUT$ERR"
  fake_goose "$FAKE_APP"
  gp="$FAKE_PID"
  avery_bg "$TT/w2.out" "${env_w[@]}" watch
  wpid="$BG_PID"
  sleep 5
  still=1
  kill -0 "$wpid" 2>/dev/null || still=0
  stop_process "$gp"
  wait_gone "$wpid" 20
  output="$(cat "$TT/w2.out")"
  [ "$still" = 1 ] && has "$output" "services stop avery"
  check "W2 solange Goose laeuft, bleibt das Modell; nach dem Schliessen wird es gestoppt" $? "laeuft=$still $output"
  avery "${env_w[@]}" "AVERY_MODEL_URL=$DEAD_URL" watch
  [ "$RC" = 0 ] && has "$OUT" "laeuft nicht mehr" && lacks "$OUT" "services stop"
  check "W3 laeuft das Modell nicht mehr, endet der Waechter ohne Stopp-Befehl" $? "$OUT"
  fake_goose "$FAKE_APP"
  gp="$FAKE_PID"
  avery_bg "$TT/w4.out" "${env_w[@]}" watch
  wpid="$BG_PID"
  w3home="$AHOME"
  sleep 2
  avery HOME="$w3home" "${env_w[@]}" watch
  [ "$RC" = 0 ] && has "$OUT" "laeuft bereits"
  check "W4 es gibt nur einen Waechter" $? "$OUT"
  avery HOME="$w3home" "${env_w[@]}" down
  ended=0
  wait_gone "$wpid" 10 && ended=1
  stop_process "$gp"
  [ "$ended" = 1 ] && has "$OUT" "services stop avery" && [ ! -e "$w3home/.avery/watch.pid" ]
  check "W5 'avery down' beendet auch den Waechter und stoppt das Modell" $? "beendet=$ended $OUT"
  avery AVERY_DRY_RUN=1 "AVERY_MODEL_URL=$DEAD_URL" up
  has "$OUT" "Waechter im Hintergrund"
  check "W6 up startet den Waechter nur, wenn es das Modell selbst gestartet hat" $? "$OUT"
  avery AVERY_DRY_RUN=1 "AVERY_MODEL_URL=$ALIVE_URL" up
  lacks "$OUT" "Waechter" && has "$OUT" "laeuft bereits"
  check "W7 ein schon laufendes Modell bleibt unter deiner Kontrolle (kein Waechter)" $? "$OUT"
}

# ------------------------------------------------------------------ Q: logo, disk image and cask
# publish_env COMMAND...: runs COMMAND with a clean git environment (no user or system config, fixed identity).
publish_env() {
  env -i "PATH=$PATH" "HOME=$PUB_HOME" GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null \
    GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.org GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.org "$@"
}
pgit() { publish_env git -C "$PUB_PROJECT" "$@"; }

run_Q() {
  local icon d n f name tiff_info script make_dmg verify_dmg nums geom_w geom_h expected_layout offset_a offset_b
  local cask_text cask_url brew_path loader rb pub remote image publish revision files parents pinned new_revision
  local -a geometry

  for icon in Avery.icns AveryClassic.icns; do
    d="$(mkd icon)"
    capture 60 iconutil -c iconset "$SHARE/$icon" -o "$d/x.iconset"
    n=0
    if [ "$RC" = 0 ]; then n="$(ls "$d/x.iconset" | wc -l | tr -d ' ')"; fi
    [ "$n" = 10 ]
    check "Q1 $icon ist ein gueltiges icns mit allen 10 Groessen" $? "${ERR:-$n}"
  done
  for name in avery-icon.svg avery-app-icon.svg avery-mark.svg; do
    f="$ROOT/assets/$name"
    [ "$(head -c 4 "$f")" = "<svg" ] && ! grep -q '<image' "$f" && ! grep -q 'base64' "$f"
    check "Q2 assets/$name ist ein SVG ohne eingebettete Bilder" $?
  done
  tiff_info="$(tiffutil -info "$ROOT/assets/dmg/background.tiff" 2>/dev/null)"
  [ "$(count "$tiff_info" "Directory at")" = 2 ] && has "$tiff_info" "Image Width: 640 Image Length: 448" \
    && has "$tiff_info" "Image Width: 1280 Image Length: 896"
  check "Q3 das DMG-Hintergrundbild hat 1x und 2x (640x448 und 1280x896)" $? "$tiff_info"
  for script in tools/render-assets.sh tools/make-dmg.sh tools/verify-dmg.sh tools/publish-dmg.sh libexec/avery-make-app bin/avery; do
    [ -x "$ROOT/$script" ] && bash -n "$ROOT/$script" >/dev/null 2>&1
    check "Q4 $script ist ausfuehrbar und hat gueltige Syntax" $?
  done

  make_dmg="$ROOT/tools/make-dmg.sh"
  verify_dmg="$(cat "$ROOT/tools/verify-dmg.sh")"
  nums="$(awk '
    /^win_left=[0-9]+ win_top=[0-9]+ win_width=[0-9]+ win_height=[0-9]+$/ {
      first = $0
      getline
      if ($0 ~ /^app_x=[0-9]+ app_y=[0-9]+ apps_x=[0-9]+ apps_y=[0-9]+$/) { print first " " $0; exit }
    }' "$make_dmg" | tr -c '0-9\n' ' ')"
  geometry=($nums)
  expected_layout=""
  geom_w=""
  geom_h=""
  if [ "${#geometry[@]}" = 8 ]; then
    geom_w="${geometry[2]}"
    geom_h="${geometry[3]}"
    expected_layout="icon view|${geometry[0]},${geometry[1]},$((geometry[0] + geometry[2])),$((geometry[1] + geometry[3]))|${geometry[4]},${geometry[5]}|${geometry[6]},${geometry[7]}|128|13|false"
  fi
  [ -n "$expected_layout" ] && has "$verify_dmg" "$expected_layout"
  check "Q5 make-dmg.sh und verify-dmg.sh meinen dasselbe Fensterlayout" $? "$expected_layout"
  [ "$geom_w" = 640 ] && [ "$geom_h" = 448 ]
  check "Q6 das Fenster passt zum Hintergrundbild (640 x 448)" $? "${geom_w}x${geom_h}"
  offset_a="$(grep -b -o -F -m 1 'update without registering applications' "$make_dmg" | head -n 1 | cut -d: -f1)"
  offset_b="$(grep -b -o -F -m 1 '.VolumeIcon.icns' "$make_dmg" | head -n 1 | cut -d: -f1)"
  [ -n "$offset_a" ] && [ -n "$offset_b" ] && [ "$offset_a" -lt "$offset_b" ]
  check "Q7 das Volume-Icon kommt erst nach Finders Update ins Image (Finder loescht es sonst)" $? "update=$offset_a icon=$offset_b"

  cask_text="$(cat "$ROOT/Casks/avery.rb")"
  has "$cask_text" 'app "Avery.app"' && has "$cask_text" 'container nested: "Avery-#{version}.dmg"' \
    && has "$cask_text" 'depends_on formula: "flavioaiello/avery/avery"' && has "$cask_text" "depends_on macos: :tahoe"
  check "Q8 Cask: App aus dem verschachtelten DMG, Abhaengigkeit von der Formel, Tahoe" $? "$cask_text"
  cask_url="$(sed -n 's/^.*url "\([^"]*\)".*$/\1/p' "$ROOT/Casks/avery.rb" | head -n 1)"
  [ "$cask_url" = "ssh://git@github.com/flavioaiello/avery.git" ] && has "$cask_text" "using:    :git" \
    && has "$cask_text" 'tag:      "dmg-v#{version}"' && grep -q -E 'revision: "[0-9a-f]{40}"' "$ROOT/Casks/avery.rb"
  check "Q9 Cask: Image per git ueber SSH, Tag dmg-v<Version> und feste Revision" $? "$cask_url"
  if have ruby && [ -n "$cask_url" ]; then
    ruby -ruri -e 'URI(ARGV[0])' "$cask_url" >/dev/null 2>&1
    check "Q9b die Cask-URL ist ein gueltiger URI (die scp-Form git@host:pfad ist es nicht und laesst den Cask nicht laden)" $?
  else
    skip "Q9b die Cask-URL ist ein gueltiger URI (die scp-Form git@host:pfad ist es nicht und laesst den Cask nicht laden)" "ruby fehlt oder keine url im Cask"
  fi
  has "$cask_text" "postflight_steps do" && lacks "$cask_text" "postflight do" \
    && has "$cask_text" 'run "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "{{appdir}}/Avery.app"]'
  check "Q10 Cask: Quarantaene wird entfernt (Ad-hoc-Signatur, nicht notarisiert), mit den aktuellen Schritten statt dem veralteten postflight" $? "$cask_text"

  brew_path="$(command -v brew 2>/dev/null)"
  if [ -z "$brew_path" ] && [ -x /opt/homebrew/bin/brew ]; then brew_path=/opt/homebrew/bin/brew; fi
  if [ -n "$brew_path" ]; then
    loader='c = Cask::CaskLoader::FromContentLoader.new(File.read(ARGV[0]), tap: Tap.fetch("flavioaiello/avery")).load(config: nil); puts "geladen #{c.token} #{c.version}"'
    capture 120 env HOMEBREW_DEVELOPER=1 HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_ENV_HINTS=1 \
      "$brew_path" ruby -e "$loader" "$ROOT/Casks/avery.rb"
    has "$OUT" "geladen avery $CARGO_VERSION" && ! has_i "$OUT$ERR" "deprecated"
    check "Q11a Homebrew laedt den Cask im strengen Entwicklermodus ohne Fehler und ohne Veraltet-Warnungen" $? "$OUT$ERR"
  else
    skip "Q11a Homebrew laedt den Cask im strengen Entwicklermodus ohne Fehler und ohne Veraltet-Warnungen" "brew fehlt"
  fi
  for rb in Casks/avery.rb Formula/avery.rb; do
    if have ruby; then
      ruby -c "$ROOT/$rb" >/dev/null 2>&1
      check "Q11 $rb hat gueltige Ruby-Syntax" $?
    else
      skip "Q11 $rb hat gueltige Ruby-Syntax" "ruby fehlt"
    fi
  done

  # publish-dmg.sh against a throw-away project and a bare "origin"
  if have git && have ruby; then
    pub="$(mkd pub)"
    PUB_HOME="$pub/home"
    PUB_PROJECT="$pub/project"
    remote="$pub/origin.git"
    mkdir -p "$PUB_HOME" "$PUB_PROJECT/bin" "$PUB_PROJECT/tools" "$PUB_PROJECT/Casks" "$PUB_PROJECT/dist"
    cp "$ROOT/tools/publish-dmg.sh" "$PUB_PROJECT/tools/"
    cp "$ROOT/Casks/avery.rb" "$PUB_PROJECT/Casks/"
    printf 'AVERY_VERSION="9.9.9"\n' > "$PUB_PROJECT/bin/avery"
    image="$PUB_PROJECT/dist/Avery-9.9.9.dmg"
    printf 'disk image one' > "$image"
    publish_env git init -q --bare "$remote" >/dev/null 2>&1
    pgit init -q . >/dev/null 2>&1
    pgit remote add origin "$remote" >/dev/null 2>&1
    publish="$PUB_PROJECT/tools/publish-dmg.sh"
    capture 120 publish_env "$publish"
    revision="$(pgit rev-parse 'dmg-v9.9.9^{commit}' 2>/dev/null)"
    files="$(pgit ls-tree -r --name-only "$revision" 2>/dev/null)"
    parents="$(pgit rev-list --parents -n 1 "$revision" 2>/dev/null)"
    pinned="$(cat "$PUB_PROJECT/Casks/avery.rb")"
    [ "$RC" = 0 ] && [ -n "$revision" ] && [ "$files" = "Avery-9.9.9.dmg" ] && [ "$parents" = "$revision" ]
    check "Q12 publish-dmg: ein Tag mit genau einem Commit, der nur das Image enthaelt (ohne Elternteil)" $? "rc=$RC files=$files parents=$parents $OUT$ERR"
    [ "$(publish_env git --git-dir "$remote" rev-parse 'dmg-v9.9.9^{commit}' 2>/dev/null)" = "$revision" ] \
      && has "$pinned" 'version "9.9.9"' && has "$pinned" "revision: \"$revision\""
    check "Q13 publish-dmg: Tag liegt auf dem Server, Cask nennt Version und Revision" $? "$revision"
    capture 120 publish_env "$publish"
    [ "$RC" = 0 ] && has "$OUT" "existiert schon"
    check "Q14 publish-dmg ist wiederholbar, solange das Image gleich ist" $? "$OUT$ERR"
    printf 'disk image two' > "$image"
    capture 120 publish_env "$publish"
    [ "$RC" = 1 ] && has "$ERR" "nicht verschoben"
    check "Q15 publish-dmg verschiebt kein veroeffentlichtes Tag" $? "$ERR"
    publish_env git --git-dir "$remote" tag -d dmg-v9.9.9 >/dev/null 2>&1
    capture 120 publish_env "$publish"
    new_revision="$(pgit rev-parse 'dmg-v9.9.9^{commit}' 2>/dev/null)"
    [ "$RC" = 0 ] && [ -n "$new_revision" ] && [ "$new_revision" != "$revision" ] \
      && [ "$(publish_env git --git-dir "$remote" rev-parse 'dmg-v9.9.9^{commit}' 2>/dev/null)" = "$new_revision" ]
    check "Q16 ein Tag, das nie auf dem Server ankam, darf mit neuem Image wiederholt werden" $? "$OUT$ERR"
  else
    for name in Q12 Q13 Q14 Q15 Q16; do skip "$name publish-dmg gegen ein Wegwerf-Projekt" "git oder ruby fehlt"; done
  fi
}

# ------------------------------------------------------------------ M: versions agree
run_M() {
  local formula tag cask_text cask_version lock_version zeros
  ensure_mail_build
  [ -n "$AVERY_VERSION" ] && [ "$AVERY_VERSION" = "$CARGO_VERSION" ]
  check "M1 Versionen in bin/avery und avery-mail/Cargo.toml stimmen ueberein" $? "bin/avery=$AVERY_VERSION Cargo.toml=$CARGO_VERSION"
  formula="$ROOT/Formula/avery.rb"
  if [ -e "$formula" ]; then
    tag="$(sed -n 's/^.*tag:[[:space:]][[:space:]]*"v\([0-9.][0-9.]*\)".*$/\1/p' "$formula" | head -n 1)"
    [ -n "$tag" ] && [ "$tag" = "$CARGO_VERSION" ]
    check "M2 Formel verweist auf das Tag dieser Version" $? "$tag"
  fi
  cask_text="$(cat "$ROOT/Casks/avery.rb")"
  cask_version="$(sed -n 's/^  version "\([0-9.][0-9.]*\)".*$/\1/p' "$ROOT/Casks/avery.rb" | head -n 1)"
  [ -n "$cask_version" ] && [ "$cask_version" = "$CARGO_VERSION" ]
  check "M3 Cask nennt die Version dieses Stands" $? "$cask_version"
  zeros="$(printf '%040d' 0)"
  lacks "$cask_text" "$zeros"
  check "M4 die Revision im Cask ist gesetzt (nicht der Platzhalter)" $?
  capture 30 "$MAIL_BIN" --version
  [ "$RC" = 0 ] && [ "$(trim "$OUT")" = "$CARGO_VERSION" ]
  check "M5 avery-mail --version meldet die Version aus avery-mail/Cargo.toml" $? "rc=$RC $OUT$ERR"
  lock_version="$(awk '/^name = "avery-mail"$/ { getline; if ($1 == "version") { gsub(/"/, "", $3); print $3; exit } }' "$ROOT/avery-mail/Cargo.lock")"
  [ -n "$lock_version" ] && [ "$lock_version" = "$CARGO_VERSION" ]
  check "M6 Cargo.lock nennt dieselbe Version (cargo install --locked braucht das)" $? "$lock_version"
}

# ------------------------------------------------------------------ main
want() {
  [ -n "$SELECTED" ] || return 0
  case " $SELECTED " in *" $1 "*) return 0 ;; esac
  return 1
}

want J && run_J
want L && run_L
want N && run_N
want P && run_P
want G && run_G
want O && run_O
want W && run_W
want Q && run_Q
want M && run_M

[ "$SKIP" -eq 0 ] || printf '\n%d uebersprungen (siehe SKIP-Zeilen)' "$SKIP"
printf '\n%d bestanden, %d fehlgeschlagen\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
