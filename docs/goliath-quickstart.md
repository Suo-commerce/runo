# Goliath quickstart: presence on one machine

Goliath's first use is presence: each machine says when it is online, asleep, awake, docked or
going offline, and the others notice when one goes quiet without saying so. This page runs the
whole chorus on one machine, so you can watch it work before it goes on the ThinkPad and a
second host.

Everything below writes only into the directory you choose (`$G`). Keys stay in `$G/keys`;
the chorus directory holds public files only.

## 1. Keys and the roster

```sh
G=~/goliath-try; mkdir -p $G
export RUNO_HOME=$G/keys
for v in thinkpad desk sentinel owner; do runo enroll --singer $v --chorus $G/chorus; done
runo init examples/goliath/chorus.toml --chorus $G/chorus --sign
```

`init` prints `pin this owner key on every host: <64 hex>`. Keep it as `OWNER`; each node
refuses a roster signed by any other key.

## 2. The store and two machines

Each in its own terminal (or tmux window):

```sh
runo store serve --dir $G/store
runo node --singer thinkpad --profile profiles/goliath.toml --chorus $G/chorus --store $G/store/store.sock --owner-key $OWNER
runo node --singer desk     --profile profiles/goliath.toml --chorus $G/chorus --store $G/store/store.sock --owner-key $OWNER
```

Each node claims a boot session, sings `online`, refrains every 15 minutes and listens every 5.

## 3. Declare a state

```sh
runo sing --singer desk sleep      # desk is silent by design now; nobody is called for a day
runo sing --singer desk wake
runo sing --singer desk docked     # refused: the profile allows docked only from online
```

## 4. Look at the fleet

```sh
runo view --manifest examples/goliath/chorus.toml --chorus $G/chorus --store $G/store/store.sock --owner-key $OWNER
```

One line per machine, problems first: `QUIET` (silent past its expectation), never heard,
waiting, up, asleep as it said, offline as it said. Refused records are counted under
`incidents`. `--once` prints it once.

For the Docker fleet (desk and sentinel in containers) and the ThinkPad as a systemd service, see
`deploy/goliath/README.md` and `deploy/thinkpad/README.md`.

## What you should see

- Stop the desk node with Ctrl-C without singing anything. About 30 minutes later (twice the
  refrain interval) the thinkpad node prints `Crossed desk Reachability …`, then `Called`.
- Sing `sleep` first and stop it: nothing is printed until a day has passed.
- Start desk again: it claims a new boot session, and its old one ends without a call.

Every verse is a line in `$G/store/song.jsonl`; the latest refrain per machine is in
`$G/store/refrains/`.
