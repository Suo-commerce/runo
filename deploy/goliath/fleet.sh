#!/usr/bin/env bash
# Goliath's presence chorus on one laptop: the ThinkPad's own node plus `desk` and `sentinel` in
# Docker. Everything this script writes is under ./state, except the ThinkPad's and the owner's
# keys, which go to the laptop's own key home (as `runo enroll` does by default).
#
#   ./fleet.sh setup          build the image, enroll every voice, sign roster v1
#   ./fleet.sh up             start the store, desk and sentinel
#   ./fleet.sh thinkpad       run this laptop's node in the foreground (Ctrl-C stops it silently)
#   ./fleet.sh view           the fleet view, refreshing (add --once to print once)
#   ./fleet.sh sing desk sleep   declare a state on a container's node
#   ./fleet.sh stop desk      stop a container without a word: it should go QUIET in the view
#   ./fleet.sh start desk     bring it back: a new boot session
#   ./fleet.sh down           stop everything (state is kept; delete ./state to start over)
set -euo pipefail
cd "$(dirname "$0")"
REPO=$(cd ../.. && pwd)
RUNO=${RUNO:-$REPO/target/release/runo}
MANIFEST=$REPO/examples/goliath/chorus.toml
STATE=$PWD/state
PORT=${RUNO_STORE_PORT:-7070}

compose() {
  env UID="$(id -u)" GID="$(id -g)" RUNO_OWNER_KEY="$(cat "$STATE/owner-key" 2>/dev/null || true)" \
    docker compose "$@"
}

enrolled() { [ -e "$STATE/chorus/enroll/$1.cbor" ]; }

need_runo() {
  [ -x "$RUNO" ] || { echo "no runo at $RUNO: run 'cargo build --release -p runo-cli' in $REPO, or set RUNO" >&2; exit 1; }
}

case "${1:-}" in
  setup)
    need_runo
    if ! docker image inspect runo:local >/dev/null 2>&1; then
      docker build -f Dockerfile -t runo:local "$REPO"
    fi
    mkdir -p "$STATE/chorus" "$STATE/song" "$STATE/keys/desk" "$STATE/keys/sentinel"
    for v in thinkpad owner; do
      if ! enrolled "$v"; then "$RUNO" enroll --singer "$v" --chorus "$STATE/chorus"; fi
    done
    for v in desk sentinel; do
      if ! enrolled "$v"; then compose run --rm --no-deps "$v" enroll --singer "$v" --chorus /chorus; fi
    done
    "$RUNO" init "$MANIFEST" --chorus "$STATE/chorus" --sign | tee "$STATE/init.txt"
    sed -n 's/.*owner key on every host: //p' "$STATE/init.txt" > "$STATE/owner-key"
    echo "owner key pinned in $STATE/owner-key (public)"
    ;;
  up)    compose up -d store desk sentinel ;;
  down)  compose down ;;
  stop)  compose stop "${2:?which machine}" ;;
  start) compose start "${2:?which machine}" ;;
  sing)
    m=${2:?which machine}; s=${3:?which state}
    compose exec "$m" runo sing --singer "$m" --control "/tmp/$m.sock" "$s"
    ;;
  thinkpad)
    need_runo
    exec "$RUNO" node --singer thinkpad --manifest "$MANIFEST" \
      --chorus "$STATE/chorus" --store "tcp:127.0.0.1:$PORT" --owner-key "$(cat "$STATE/owner-key")"
    ;;
  view)
    need_runo
    shift
    exec "$RUNO" view --manifest "$MANIFEST" --chorus "$STATE/chorus" \
      --store "tcp:127.0.0.1:$PORT" --owner-key "$(cat "$STATE/owner-key")" "$@"
    ;;
  logs)  compose logs "${@:2}" ;;
  *) sed -n '2,15p' "$0"; exit 2 ;;
esac
