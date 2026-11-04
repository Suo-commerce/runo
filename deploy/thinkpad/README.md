# The ThinkPad as a voice

The laptop's node runs as a systemd **user** service: as you, with your key home, never as root.
A small sleep hook tells the chorus when the lid closes and opens.

## Install

After `deploy/goliath/fleet.sh setup` (which enrolled `thinkpad` and `owner` in your key home and
signed the roster):

```sh
cargo build --release -p runo-cli
cd deploy/thinkpad
./install.sh ../goliath/state/chorus tcp:127.0.0.1:7070 "$(cat ../goliath/state/owner-key)"
```

It installs:

| What | Where |
| --- | --- |
| `runo` | `~/.local/bin/runo` |
| the manifest and profiles, the chorus files (public), `node.env` (store address and owner key, no secrets) | `~/.config/runo/` |
| the user unit | `~/.config/systemd/user/runo-node.service`, enabled and started |
| the sleep hook (asks for sudo) | `/usr/lib/systemd/system-sleep/runo` |

If `./fleet.sh thinkpad` is still running in a terminal, stop it first: the service would refuse
to take over its control socket.

## What it does

- **Start (login, boot):** claims a new boot session and sings `online`; refrains every 15 minutes.
- **Lid closed:** the hook sings `sleep` before suspend. Nobody is called for a day.
- **Lid opened:** the hook sings `wake`, then `online`.
- **Clean stop** (`systemctl --user stop runo-node`, shutdown): sings `state-saved`, then
  `going-offline`. The view shows "offline, as it said".
- **Crash or power cut:** nothing is sung, so the silence is called after 30 minutes.

## Check it

```sh
systemctl --user status runo-node
journalctl --user -u runo-node -f
runo sing --singer thinkpad docked          # docking is manual for now
```

## Remove it

```sh
systemctl --user disable --now runo-node
rm ~/.config/systemd/user/runo-node.service
sudo rm /usr/lib/systemd/system-sleep/runo
```

Your key stays in your key home (`~/.local/share/runo/keys`) until you delete it.
