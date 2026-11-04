# Goliath's fleet on one laptop

Three emulated machines and one real one, singing presence:

| Voice | Where | Profile |
| --- | --- | --- |
| `thinkpad` | your laptop, its own node (`deploy/thinkpad`) | `profiles/goliath.toml` |
| `desk` | a container | `profiles/goliath.toml` |
| `sentinel` | a container, refrains every 5 minutes | `profiles/sentinel.toml` |
| the store | a container, port 7070 on 127.0.0.1 only | |

## Run it

```sh
cargo build --release -p runo-cli          # the laptop's runo
cd deploy/goliath
./fleet.sh setup      # builds the image, enrolls every voice, signs roster v1
./fleet.sh up         # store, desk, sentinel
./fleet.sh thinkpad   # this laptop's node, in the foreground (or install it: deploy/thinkpad)
./fleet.sh view       # in another terminal
```

Messages (one per incident: quiet, still quiet, may be down, fine again) appear in the
`./fleet.sh thinkpad` terminal; `deploy/thinkpad/README.md` shows how to get them on Telegram.

Then try:

```sh
./fleet.sh sing desk sleep     # desk: "sleep since …, as it said"
./fleet.sh stop sentinel       # gone without a word: QUIET after 10 minutes
./fleet.sh start sentinel      # back, with a new boot session
```

Everything is written under `deploy/goliath/state` (git ignores it), except the ThinkPad's and
the owner's keys, which go to your own key home. Delete `state` to start over.

## What this cannot show

The containers share the laptop, so they share its failure domain. When the laptop sleeps, so do
they. The nodes' own clocks don't count suspend, so nobody is called for it. But `runo view` reads
the machines' own timestamps: right after a long sleep, `desk` can show QUIET until its next
refrain (at most 15 minutes). A genuinely separate second host (the Scaleway VM, milestone 6) is
what makes "the others noticed" mean something.

The store service has no authentication of its own. Every reader admits records against the signed
roster, so a forged verse is refused and shown as an incident. Anyone who can reach the port could
still fill the disk, which is why it listens on 127.0.0.1 only.
