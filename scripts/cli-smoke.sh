#!/usr/bin/env bash
# The whole mod lifecycle through the `mm` CLI, against a fake CrossOver
# bottle in a scratch folder: install, deploy, verify, preflight, deps,
# update, rollback, profiles, archives, adopt, purge. The game folder must end
# byte-identical to how it started. Never touches the real library or bottle.
#
#   scripts/cli-smoke.sh [path/to/mm]
set -euo pipefail

MM=${1:-target/debug/mm}
WORK=$(mktemp -d /tmp/mm-smoke-XXXXXX)
export MM_DATA_DIR="$WORK/data"
BOTTLES="$WORK/Bottles"
GAME="$BOTTLES/Cyberpunk 2077/drive_c/GOG Games/Cyberpunk 2077"
mm() { "$MM" --bottles-dir "$BOTTLES" "$@"; }
step() { printf '\n== %s\n' "$*"; }
fail() { echo "FAILED: $*" >&2; exit 1; }

step "fake bottle in $WORK"
mkdir -p "$BOTTLES/Cyberpunk 2077/drive_c/users/crossover" "$GAME/bin/x64" "$GAME/engine/config/base"
printf '[Bottle]\n"Template" = "win10_64"\n' > "$BOTTLES/Cyberpunk 2077/cxbottle.conf"
printf 'WINE REGISTRY Version 2\n' > "$BOTTLES/Cyberpunk 2077/system.reg"
printf 'WINE REGISTRY Version 2\n\n[Software\\\\Wine\\\\DllOverrides] 1\n"version"="native,builtin"\n"winmm"="native,builtin"\n' > "$BOTTLES/Cyberpunk 2077/user.reg"
printf 'MZ game' > "$GAME/bin/x64/Cyberpunk2077.exe"
printf '[General]\nOriginal=1\n' > "$GAME/engine/config/base/general.ini"
snapshot() { (cd "$GAME" && find . -type f -print0 | sort -z | xargs -0 shasum -a 256); }
BEFORE=$(snapshot)

step "archives"
make_mod() { # name version archive
  local dir="$WORK/src/$1-$2"
  mkdir -p "$dir/archive/pc/mod" "$dir/r6/scripts/$1" "$dir/engine/config/base"
  printf 'RDAR %s %s' "$1" "$2" > "$dir/archive/pc/mod/$1.archive"
  printf '// %s %s\n' "$1" "$2" > "$dir/r6/scripts/$1/main.reds"
  printf '[General]\nModded=%s\n' "$2" > "$dir/engine/config/base/general.ini"
  (cd "$dir" && zip -qr "$WORK/$3" .)
}
make_mod Smoke 1 Smoke.zip
make_mod Smoke 2 Smoke-v2.zip
mkdir -p "$WORK/src/Helper/archive/pc/mod" && printf 'helper' > "$WORK/src/Helper/archive/pc/mod/helper.archive"
(cd "$WORK/src/Helper" && zip -qr "$WORK/Helper.zip" .)

step "detect, install, deploy"
out=$(mm detect); grep -q "Cyberpunk 2077 (KnownPath)" <<< "$out" || fail "fake bottle not detected"
mm install "$WORK/Smoke.zip" "$WORK/Helper.zip"
mm deploy > /dev/null
grep -q "Modded=1" "$GAME/engine/config/base/general.ini" || fail "mod not deployed"
out=$(mm --json verify); grep -q '"drifted": \[\]' <<< "$out" || fail "drift right after deploy"

step "deps and preflight"
out=$(mm deps); grep -q "redscript.*MISSING" <<< "$out" || fail "missing redscript not reported"
set +e; mm preflight > "$WORK/preflight.txt"; code=$?; set -e
[ "$code" = 2 ] || fail "preflight should exit 2 with errors, got $code"
grep -q "Smoke needs redscript" "$WORK/preflight.txt" || fail "preflight misses the requirement"

step "profiles, enable, disable"
mm profiles create Vanilla > /dev/null
out=$(mm --json profiles); grep -q '"name": "Vanilla"' <<< "$out" || fail "profile not created"
mm profiles switch Default > /dev/null
mm disable --mod Helper > /dev/null
out=$(mm --json mods); grep -q '"enabled": false' <<< "$out" || fail "disable didn't stick"
mm enable --mod Helper > /dev/null

step "update and rollback"
mm update --mod Smoke --archive "$WORK/Smoke-v2.zip"
mm deploy > /dev/null
grep -q "Modded=2" "$GAME/engine/config/base/general.ini" || fail "update not deployed"
mm rollback --mod Smoke
mm deploy > /dev/null
grep -q "Modded=1" "$GAME/engine/config/base/general.ini" || fail "rollback not deployed"
out=$(mm --json archives); grep -q '"unused": false' <<< "$out" || fail "archives not listed"
mm archives clean > /dev/null

step "adopt a hand-installed mod"
mkdir -p "$GAME/r6/scripts/ByHand"
printf '// by hand' > "$GAME/r6/scripts/ByHand/main.reds"
out=$(mm adopt); grep -q "ByHand" <<< "$out" || fail "hand-installed mod not found"
mm adopt --name ByHand > /dev/null
rm "$GAME/r6/scripts/ByHand/main.reds"; rmdir "$GAME/r6/scripts/ByHand"
out=$(mm --json verify); grep -q '"Missing"' <<< "$out" || fail "removed adopted file not reported missing"
mm disable --mod ByHand > /dev/null

step "purge"
mm purge > /dev/null
# Folders mods are dropped into stay on purpose; files must match exactly.
AFTER=$(snapshot)
[ "$BEFORE" = "$AFTER" ] || { diff <(echo "$BEFORE") <(echo "$AFTER"); fail "game folder differs after purge"; }

echo
echo "CLI smoke test passed ($WORK)"
