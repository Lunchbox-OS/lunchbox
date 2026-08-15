#!/bin/bash
# Drive the Shepherd Companion pairing flow on a USB-attached Android
# phone, unattended. See SKILL.md for the whole procedure and the
# gotchas this script exists to avoid.
#
#   pair.sh ui              dump every labelled node with tap coordinates
#   pair.sh tap <text>      tap a node by label (exact match wins)
#   pair.sh run             run the full flow from the "Pair a device" list
#
# Env: PKG (package id), SWAYLOG (daemon log), SHOTDIR (screenshot output).
set -uo pipefail

PKG=${PKG:-com.armeafamily.shepherd.companion}
REPO=${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
SWAYLOG=${SWAYLOG:-$REPO/dev-runtime/headless/sway.log}
SHOTDIR=${SHOTDIR:-${TMPDIR:-/tmp}/shepherd-pairing}
UIXML=$SHOTDIR/ui.xml
mkdir -p "$SHOTDIR"

require_adb() {
  command -v adb >/dev/null || { echo "adb not on PATH (try /opt/android-sdk/platform-tools)"; exit 1; }
  adb get-state >/dev/null 2>&1 || { echo "no adb device (check 'adb devices' — 'unauthorized' needs a prompt accepted on the phone)"; exit 1; }
}

log() { echo "[$(date +%H:%M:%S)] $*"; }
shot() { adb exec-out screencap -p > "$SHOTDIR/$1" 2>/dev/null && echo "$SHOTDIR/$1"; }

# Every on-screen label with the centre point to tap it.
texts() {
  adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1
  adb shell cat /sdcard/ui.xml > "$UIXML" 2>/dev/null
  python3 - "$UIXML" <<'PY'
import re, sys, html
xml = open(sys.argv[1], encoding="utf-8", errors="replace").read()
for m in re.finditer(r"<node[^>]*>", xml):
    tag = m.group(0)
    t = re.search(r'text="([^"]*)"', tag)
    d = re.search(r'content-desc="([^"]*)"', tag)
    b = re.search(r'bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"', tag)
    lab = html.unescape((t.group(1) if t else "") or (d.group(1) if d else "")).strip()
    if lab and b:
        x = (int(b.group(1)) + int(b.group(3))) // 2
        y = (int(b.group(2)) + int(b.group(4))) // 2
        print(f"{x},{y}\t{lab}")
PY
}

# Exact label match wins over substring, so "shepherd" picks the device
# row and not the "Make sure the shepherd device's TV is on…" blurb.
find_text() {
  texts | python3 -c '
import sys
needle = sys.argv[1].lower(); exact = part = None
for line in sys.stdin:
    line = line.rstrip("\n")
    if "\t" not in line: continue
    xy, lab = line.split("\t", 1)
    l = lab.lower()
    if l == needle and exact is None: exact = (xy, lab)
    elif needle in l and (part is None or len(lab) < len(part[1])): part = (xy, lab)
hit = exact or part
if hit: print(hit[0] + "\t" + hit[1])
' "$1"
}

tap_hit() { local xy; xy=$(echo "$1" | cut -f1); adb shell input tap ${xy/,/ }; }

cmd_ui() { texts; }

cmd_tap() {
  local hit; hit=$(find_text "$1")
  [ -z "$hit" ] && { echo "not found: $1"; return 1; }
  tap_hit "$hit"; echo "tapped '$(echo "$hit" | cut -f2)' @ $(echo "$hit" | cut -f1)"
}

cmd_run() {
  [ -r "$SWAYLOG" ] || { log "daemon log not readable at $SWAYLOG — is the headless session up?"; exit 1; }
  local baseline; baseline=$(grep -ac "Numeric Comparison pairing requested" "$SWAYLOG" 2>/dev/null)

  log "waiting for the scan to list the device"
  local hit=""
  for _ in $(seq 1 25); do
    local cand; cand=$(find_text "shepherd")
    if [ -n "$cand" ] && ! echo "$cand" | grep -qiE "scanning|make sure"; then hit="$cand"; break; fi
    sleep 1
  done
  [ -z "$hit" ] && { log "device row never appeared — is shepherdd advertising?"; exit 1; }
  log "tapping row: $(echo "$hit" | cut -f2)"
  tap_hit "$hit"

  # Only ever confirm a prompt this attempt caused. A leftover notification
  # from an earlier attempt is indistinguishable on screen.
  log "waiting for a fresh device-side passkey request"
  local devcode=""
  for _ in $(seq 1 20); do
    sleep 1
    local count; count=$(grep -ac "Numeric Comparison pairing requested" "$SWAYLOG" 2>/dev/null)
    if [ "$count" -gt "$baseline" ]; then
      devcode=$(sed -r 's/\x1b\[[0-9;]*m//g' "$SWAYLOG" | grep -a "Numeric Comparison pairing requested" \
                | tail -1 | grep -oP 'passkey=\K[0-9]+')
      devcode=$(printf '%06d' "$devcode")   # the daemon logs it unpadded
      log "device passkey: $devcode"
      break
    fi
  done
  [ -z "$devcode" ] && { log "device never requested numeric comparison"; shot no-passkey.png; exit 1; }

  # The heads-up auto-dismisses; the shade keeps it. This action only
  # OPENS the comparison dialog, it does not confirm the pairing.
  log "opening the pairing prompt"
  for _ in $(seq 1 12); do
    adb shell cmd statusbar expand-notifications >/dev/null 2>&1
    sleep 0.6
    local h; h=$(texts | python3 -c '
import sys
for line in sys.stdin:
    line = line.rstrip("\n")
    if "\t" not in line: continue
    xy, lab = line.split("\t", 1)
    if lab.strip().lower() in ("pair & connect", "pair and connect"):
        print(xy + "\t" + lab); break
')
    if [ -n "$h" ]; then tap_hit "$h"; log "opened via '$(echo "$h" | cut -f2)'"; break; fi
  done

  # THE confirmation. Collapse the shade first: expanded, a "Pair" label up
  # there shadows the dialog's button, the tap lands on the notification,
  # and the phone silently sits out the 30s SMP timeout.
  adb shell cmd statusbar collapse >/dev/null 2>&1
  sleep 1
  local confirmed=0
  for _ in $(seq 1 10); do
    local all; all=$(texts)
    local phonecode; phonecode=$(echo "$all" | grep -oE '\b[0-9]{6}\b' | head -1)
    local h; h=$(echo "$all" | python3 -c '
import sys
for line in sys.stdin:
    line = line.rstrip("\n")
    if "\t" not in line: continue
    xy, lab = line.split("\t", 1)
    if lab.strip().lower() == "pair":
        print(xy + "\t" + lab); break
')
    if [ -n "$h" ]; then
      shot prompt.png >/dev/null
      if [ -n "$phonecode" ] && [ "$phonecode" = "$devcode" ]; then
        log "MATCH: phone $phonecode == device $devcode"
      elif [ -n "$phonecode" ]; then
        log "MISMATCH: phone $phonecode != device $devcode — do NOT confirm this in a real check"
      else
        log "no 6-digit code visible in the dialog"
      fi
      tap_hit "$h"; log "confirmed"
      confirmed=1
      break
    fi
    sleep 1
  done
  [ "$confirmed" = 0 ] && log "comparison dialog never appeared"

  sleep 10
  log "result: $(shot result.png)"
  texts | sed 's/^/    /' | head -8
  log "daemon:"; sed -r 's/\x1b\[[0-9;]*m//g' "$SWAYLOG" | grep -aiE "pairing complete|claim|passkey" | tail -4
  log "phone bond:"; adb shell dumpsys bluetooth_manager 2>/dev/null | grep -A6 "Bonded devices" | grep -E "=>" | tail -2
  log "admin record:"; ls "$REPO/dev-runtime/data/" 2>/dev/null | grep -i admin || echo "    (none — still unclaimed)"
}

case "${1:-}" in
  ui)  require_adb; cmd_ui ;;
  tap) require_adb; shift; cmd_tap "$@" ;;
  run) require_adb; cmd_run ;;
  *)   sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' ;;
esac
