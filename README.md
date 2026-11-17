# cryo

zfs snapshot lifecycle maintenance and replication, driven entirely by
zfs properties.

## Why

FreeNAS's built-in snapshot manager has a bad inheritance model (one
exception on one dataset means abandoning the shared policy and
re-entering it on every dataset by hand) and keeps its configuration
in a GUI database that doesn't travel with the pool. [zest][], cryo's
ancestor, fixed that for snapshot lifecycle by storing policy as zfs
user properties instead — inherited like any other zfs property, and
exported/imported with the pool itself.

cryo carries that same idea the rest of the way: a replication target
is just another zfs property. Policy, schedule, and replication target
all travel with the pool across machines, with nothing separate to
reinstall, reconfigure, or keep in sync when it moves.

[zest]: zest.md

## Model

- **No configuration files.** All policy and the handful of state cryo
  needs live in `cryo:*` zfs user properties, inherited like any other
  zfs property — one policy set on a pool root covers everything below
  it until something overrides it locally.
- **Fully automated.** cryo discovers what to manage by walking those
  properties across every imported pool on the host. A dataset with
  none set, directly or by inheritance, is simply left alone — there's
  no flag anywhere for naming a dataset to act on.
- **Claims-based snapshot lifecycle.** Every policy due when a
  snapshot is taken "claims" it; the snapshot survives as long as any
  policy still claims it, and is destroyed the moment none do. That's
  what lets an `indefinite`-retention policy and a time-based one
  share the same snapshot safely, and what two independent, optional
  space-pressure valves (`cryo:limit`, `cryo:reserve`) hook into when
  "indefinite" needs to stop meaning "fill the disk."
- **A long-running daemon is the normal way to run it.** It keeps
  every policy and snapshot claim in memory, computes exactly when the
  next action is due instead of polling blindly, and answers
  `status`/`run` from that cache over a local control socket —
  falling back to a direct scan (or a cron-driven one-shot `run`) when
  no daemon is present.
- **Replication speaks a small framed protocol of its own**, over
  ssh, a user-supplied pipe command, or locally — three ways to reach
  another `cryo server`, none of which cryo authenticates on its own:
  whoever can reach it, and whatever that user's `zfs allow`/sudo
  permits, is the entire trust boundary, by design. A target can also
  just be a directory, with no peer involved at all: cryo writes
  plain, independently-replayable `zfs send` stream files instead.

See `man cryo` (or `cryo --man`) for the complete property reference
and command-line usage, and [`cryo.md`](cryo.md) for the full design —
the wire protocol, the claims model in detail, and the reasoning
behind every choice above.

## Installing

Needs Rust to build, and a `zfs`/`zpool` toolchain to run — Linux or
FreeBSD, both current OpenZFS. [`just`][just] drives the rest:

```sh
just release   # cargo build --release
just install   # binary, man page, cryo.md, and the systemd unit -- sudo only where actually needed
just enable    # enable at boot (systemd; Linux only)
just start     # start it now
```

`just install` follows the usual `PREFIX`/`DESTDIR` convention:
`just prefix=/usr install`, or `DESTDIR=/pkg/stage just install` for a
staged/packaging build with no elevation at all. `just --list` shows
everything else (`check`, `test`, `verify`, `clean`, `uninstall`).

The shipped systemd unit (`systemd/cryo.service`) runs the daemon as
root by default — cryo's privilege model has no layer of its own, and
a daemon meant to discover and manage every pool on the host needs
that breadth. The unit file documents exactly what to `zfs allow`
instead, if you'd rather delegate to a dedicated user on specific
pools than run as root.

[just]: https://github.com/casey/just

## Status

Young. Built and tested interactively against a real multi-pool host —
including live replication over all three transports, resumable
transfers, VFS file targets, and real `cryo:limit`/`cryo:reserve`
space pressure — but new, single-maintainer software, not yet hardened
for anything beyond personal or small-scale use.
