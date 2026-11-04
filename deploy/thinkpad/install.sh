#!/usr/bin/env bash
# Install the ThinkPad's voice: the binary, its profile and chorus files, the user unit, and the
# sleep hook (the hook needs sudo; everything else stays in your home directory).
#   ./install.sh <chorus-dir> <store> <owner-key>
# e.g. ./install.sh ../goliath/state/chorus tcp:127.0.0.1:7070 "$(cat ../goliath/state/owner-key)"
set -euo pipefail
cd "$(dirname "$0")"
REPO=$(cd ../.. && pwd)
CHORUS=${1:?chorus directory}; STORE=${2:?store, e.g. tcp:127.0.0.1:7070}; OWNER=${3:?owner key}
[ ${#OWNER} -eq 64 ] || { echo "the owner key is 64 hex characters" >&2; exit 1; }

install -D -m 0755 "$REPO/target/release/runo" "$HOME/.local/bin/runo"
# the manifest and every profile, in the repository's layout so the manifest's paths resolve
install -D -m 0644 "$REPO/examples/goliath/chorus.toml" "$HOME/.config/runo/goliath/examples/goliath/chorus.toml"
for p in "$REPO"/profiles/*.toml; do install -D -m 0644 "$p" "$HOME/.config/runo/goliath/profiles/$(basename "$p")"; done
mkdir -p "$HOME/.config/runo/chorus"
cp -r "$CHORUS"/. "$HOME/.config/runo/chorus/"
printf 'RUNO_STORE=%s\nRUNO_OWNER_KEY=%s\n' "$STORE" "$OWNER" > "$HOME/.config/runo/node.env"
install -D -m 0644 runo-node.service "$HOME/.config/systemd/user/runo-node.service"
systemctl --user daemon-reload
systemctl --user enable --now runo-node.service
echo "node running: systemctl --user status runo-node"

echo "the sleep hook needs root (it is a 20-line shell script; read it first: $PWD/runo-sleep)"
sudo install -m 0755 -o root -g root runo-sleep /usr/lib/systemd/system-sleep/runo
sudo sed -i "s/^RUNO_USER=\${RUNO_USER:-samwain}/RUNO_USER=\${RUNO_USER:-$(id -un)}/" /usr/lib/systemd/system-sleep/runo
echo "sleep hook installed: close the lid, open it, and runo view shows sleep then online"
