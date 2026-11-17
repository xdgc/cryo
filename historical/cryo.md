# CRYO

## Overview

Cryo is an integrated zfs snapshot lifecycle maintenance and replication
framework called `cryo`. It has three core purposes that converge around
the zfs snapshot:

1. It preserves data by making periodic and ad-hoc snapshots of
   zfs datasets (filesystems and volumes). It manages lifecycle by
   permitting policies for snapshot retention and expiry. (This behavior
   broadly is inherited from `zest`.)

2. It provides for managed remote replication.

3. As a consequence of these, it can be used for wholesale reproduction
   of a pool from one machine to another, or for incremental sync of
   live datasets.

Cryo is a fully automated system. There is no concept of selecting
individual datasets on the command line for routine operation: cryo
discovers everything it needs to act on by walking zfs properties across
every imported pool on the host, and acts only where `cryo:*` properties
resolve (by local setting or inheritance). An unconfigured dataset is
simply untouched.


## Snapshot behaviors

a. There are no configuration files. Configuration and state are kept in
ZFS user properties with the namespace `cryo`. Policy is set and changed
with `zfs set`/`zfs inherit` directly; cryo itself has no subcommand for
editing policy.

b. `cryo:snapshot` properties specify snapshot frequency and
   retention. This is a _prefix_, not an exact property name.

   Frequency is given as number-letter pairs describing an interval. For
   example, 15m is every 15 minutes. m=minutes, h=hours, d=days,
   w=weeks. No other units are accepted because they're either too short
   or they're ambiguous.

   Retention is given either as a bare number (keep N of these) or as a
   time period like the frequency. For example, 1h/8h means to keep 8
   hours' worth of 1-hour-increment snapshots.

   A retention period can also be the literal word 'indefinite'. '*' is
   synonymous. See below.

c. `cryo` properties are inherited and operate recursively. Exceptions
   are met by inheritance overrides ("local properties").

d. A snapshot property describes only one policy. A dataset
   can have multiple snapshot properties, each for a separate
   policy. `cryo:snapshot` could be a global baseline, while
   `cryo:snapshot-monthly` specifies an additional retention policy
   for some subset.

   For claim-tracking purposes (see below), a policy's _name_ is the
   suffix after `cryo:snapshot-`, or the empty string for the bare
   `cryo:snapshot` policy.

e. `cryo:last` (and `cryo:last-<name>`, mirroring the same prefix
   pattern) records, per dataset and per policy, the last time that
   policy fired. This is the scheduling state used to compute when a
   policy next comes due; it is separate from any individual snapshot.

f. Snapshot names carry no policy identity, only a timestamp:
   `dataset@cryo-YYYYMMDD.HHMMSS`. There is no reason to encode which
   policy's cadence produced a snapshot in its name, because what
   matters is not "is this an hourly or a monthly snapshot" but "does
   anything still need this snapshot to exist." A manual/instant
   snapshot is named `dataset@cryo-instant-YYYYMMDD.HHMMSS` and never
   auto-expires; it must be deleted by hand.

   If more than one policy on a dataset comes due at the same moment,
   cryo takes a single snapshot that satisfies all of them, rather than
   one redundant snapshot per policy.

g. Snapshot lifecycle is tracked by a "claims" model. Every cryo-managed
   snapshot carries a `cryo:claimed-by` property: a comma-separated list
   of the names (per (d) above) of every policy currently requiring that
   snapshot to exist. A snapshot is destroyed exactly when its claim
   list becomes empty — there is no other destruction path.

   - When a snapshot is taken, every policy that was due at that moment
     adds its name to that snapshot's `claimed-by`.
   - Expiry sweep, per policy, per dataset: for each cryo-tagged
     snapshot claimed by a given policy, once that policy's time or
     counter-based retention window has elapsed for it, that policy's
     name is dropped from `claimed-by`. If the list is now empty, the
     snapshot is destroyed.
   - A policy with `indefinite`/`*` retention never drops its own claim
     on its own initiative — only `cryo:limit`/`cryo:reserve` pressure
     (below) can strip an indefinite claim, and it is the *only* kind of
     claim that pressure-driven cleanup is permitted to strip. A claim
     from a time- or counter-based policy is never touched by space
     pressure, so a snapshot shared between an indefinite policy and a
     still-active time-based policy survives pressure cleanup (only the
     indefinite claim is stripped; the snapshot persists as long as any
     claim remains).

h. You can also define retention policy with a reservation or a limit.
   This allows indefinite retention provided that utilization remains
   modest.

   If `cryo:limit` is set and the size of the dataset plus all snapshots
   exceeds that limit, snapshots will be deleted oldest-first until
   under the limit. This is a dataset property but can be inherited from
   a parent or root dataset.

   If the _pool_ has a `cryo:reserve` property, it's a minimum amount
   of space to keep free on the pool. If pool utilization goes above
   that, cryo snapshots will be deleted oldest-first until under
   threshold. This can be specified as a `high:low` pair. If utilization
   goes below `high`, snapshots are deleted until below `low`.

   `cryo:limit` cleanup is scoped to its own dataset. `cryo:reserve`
   cleanup is pool-wide: when it trips, the oldest eligible (i.e.
   indefinitely-claimed-only, per (g)) snapshot *anywhere in the pool*
   is the next one cleaned up, not the oldest within any one dataset.

   These sizes can be specified with any conventional size
   suffix. Lowercase suffices have an order of 10^3 (e.g. g=gigabytes);
   uppercase have order 2^10 (e.g. G=gibibytes).


## Replication behaviors

a. `cryo:target` properties describe a replication target. All cryo
   snapshots of a dataset are replicated to that target. (Snapshots that
   were not created by cryo are ignored.) This also is a prefix. You
   can set two replication targets as `cryo:target-local` and
   `cryo:target-cloud`, for example.

b. The format of a replication target is `[endpoint:]path
   [transport:detail]`. "Detail" is transport-dependent.

c. Local-local replication is permitted. If endpoint is absent from a
   target identifier, transport degrades to `local`.

d. If path is absent, it's the same as the origin. A path that begins
   with `.` or `/` is a VFS filesystem location and snapshot streams are
   files on disk. (These can be replayed on a ZFS-capable system.) A
   target path that begins with a letter is a zfs path, beginning with a
   pool and optionally including a parent dataset.

   A VFS target has no peer to negotiate a baseline with, so the stream
   files themselves stand in for that: they're written to
   `<path>/<dataset>/<snapshot-name>.zfs`, mirroring the dataset's own
   path as nested directories. Before sending anything, cryo looks at
   what's already in that directory the same way it would read a
   `cryo server`'s reported snapshot list, to find the newest shared
   snapshot to send incrementally from (or sends a full stream if
   there's no overlap). Each file is written first under a `.partial`
   sibling name and renamed into place only once `zfs send` has
   actually finished successfully, so an interrupted write never
   leaves a truncated file sitting under a name that would later look
   like a valid baseline. There's no `cryo server` on this path either,
   so none of the origin/GUID bookkeeping in (g) applies to a VFS
   target -- replaying the file elsewhere is an ordinary `zfs receive`,
   outside cryo's involvement.

e. In the target spec, transport is optional. If not present, we assume
   `ssh:endpoint`.

   The ssh transport will attempt to ssh to the named endpoint
   (a hostname or an identifier in `~/.ssh/config`) and invoke
   `cryo server`. Tuning ssh behavior should be done through
   `~/.ssh/config`, or by using the `pipe:command` transport.

   The pipe transport interprets `detail` as a command to run which will
   connect to `cryo server` at the other end.

   Local transport runs `cryo server` locally.

   No other transports are currently defined.

f. Snapshots on a replication target do not automatically match the
   origin's retention policy. The replication server runs its own copy
   of cryo to prune its replicas. (A target dataset does receive the
   origin's other, non-cryo-retention properties — see (i) below —
   but cryo's own lifecycle logic on that side ignores any inherited
   `cryo:snapshot*` policy because of (h): it only snapshots/claims on
   datasets of local origin.)

g. A cryo snapshot is assumed to have a local origin. But if
   `cryo:origin` is present, that identifies the origin of the
   snapshot. It should include a hostname part, a path part, the source
   pool's GUID, and the source dataset's GUID. These are stored as
   `cryo:origin = <hostname>|<dataset-path>|<pool-guid>|<dataset-guid>`.
   This property is set on a replication target dataset and its
   snapshots by the replication procedure. "Hostname" here refers to the
   actual system name, not an IP address or DNS name.

   Hostname alone can't detect a pool or dataset having been destroyed
   and recreated under the same name; the GUIDs can. If a later
   replication session's handshake (hostname, pool GUID, dataset GUID)
   doesn't match what's already recorded in `cryo:origin` for a dataset,
   that's treated as a hard error — most likely a misconfiguration, a
   rebuilt source, or a name collision between two unrelated lineages —
   and requires an explicit override flag to proceed. It is never
   silently overwritten.

h. `Cryo` doesn't snapshot datasets of nonlocal origin. It does do
   lifecycle management on the dataset's snapshots, though.

i. Replication propagates the origin's dataset properties to the target
   (`zfs send -p`/`-R` semantics), so the replica mirrors the source's
   general configuration (compression, quota, and so on, including
   origin's `cryo:snapshot*` values, which are inert there per (f)).
   Cryo then overlays what it needs on top (`cryo:origin`,
   `cryo:claimed-by` as snapshots are replicated in, etc.).

j. An encrypted source dataset is always replicated with a raw send
   (`zfs send -w`); there is no reason not to for a replication target,
   since it means the target never needs the encryption key for
   anything cryo does to it (not even its own snapshot lifecycle
   management — claim bookkeeping and destroy operate on metadata, not
   decrypted content). Two things fall out of this that are expected
   behavior, not bugs:

   - A raw incremental send requires both snapshots to share the same
     encryption key/root. After a `zfs change-key` on the source, the
     next replication of that dataset has to be a fresh full raw send.
   - The replica is not independently readable without the key being
     delivered/loaded there separately — raw replication deliberately
     never puts the replication target in possession of it.

k. Resumable transfers are supported: an interrupted `zfs send`/`receive`
   can resume via `zfs receive -s` resume tokens rather than restarting
   from scratch. The server always receives with `-s`; if a destination
   already has a pending resume token when a new session's hello
   arrives, that token rides along in the ack in place of (and takes
   priority over) the usual existing-snapshot list — zfs won't accept
   any other receive onto that dataset until the pending one is either
   resumed or explicitly aborted, so there's nothing to negotiate.
   The client then sends with `zfs send -t <token>` instead of the
   usual `-i baseline latest`; the token alone is enough to pick up
   exactly where the interrupted attempt left off. This only applies
   to wire-protocol targets — a VFS target's `.partial`-then-rename
   write (d) is how that path avoids ever trusting a truncated file,
   without needing zfs's own resume mechanism at all.

l. Replication can be one-shot by specifying an origin and a target on
   the command line. One-shot operations are also recursive, unless
   otherwise specified.


## Wire protocol

A `cryo` instance connected to a `cryo server` instance speaks a framed,
binary protocol over a single persistent connection (ssh, pipe, or
local). The connection stays open for the whole session, and one session
may carry any number of operations in sequence — there is no need to
reconnect per dataset or per sync. Each operation is some number of
`CONTROL` frames (synchronous request/response negotiation) optionally
followed by a run of `DATA` frames carrying a `zfs send` stream and
terminated by an `END-OF-STREAM` frame — not by closing the connection.
The connection closes only when the whole session ends.

Every frame has the same fixed 18-byte header, followed by an optional
extension block, followed by the payload:

    MAGIC        2 bytes   sanity marker, catches stream desync/garbage
    VERSION      1 byte    protocol version
    TYPE         2 bytes   frame type (u16)
    FLAGS        2 bytes   reserved bitfield, currently unused
    RESERVED     1 byte    padding/future use
    HEADER_LEN   2 bytes   length of the extension block that follows
    PAYLOAD_LEN  8 bytes   payload length (u64 — no artificial per-frame
                           size ceiling, even though DATA frames will in
                           practice be chunked to something on the order
                           of 128KB-1MB)

followed by `HEADER_LEN` bytes of extension data, then `PAYLOAD_LEN`
bytes of payload.

Frame types (not exhaustive — new ones can be added; a peer on an
incompatible `VERSION` refuses the session rather than misparsing):

- `CONTROL` — a synchronous request or response. Its payload is
  TLV-encoded (tag, length, value, repeated) so that new fields can be
  added later without breaking old parsers: an unrecognized tag can
  still be skipped correctly because its length is self-declared.
- `DATA` — a chunk of `zfs send` stream payload.
- `END-OF-STREAM` — marks the end of the current streaming operation.
  Does not close the connection.
- `HEARTBEAT` — emitted periodically during a long-running DATA stream,
  so a stalled or dead peer can be detected within a bounded time
  without relying solely on ssh/TCP-level timeouts.
- `ABORT` — either side can cancel an in-progress operation without
  severing the connection.

A session opens with a `CONTROL` "hello" exchange whose TLV fields
include at least the sender's real hostname (per the `cryo:origin`
definition above), the relevant pool GUID, and dataset GUID.

All snapshot sends are incremental if both ends have a shared baseline
snapshot. If not, this is revealed during the `CONTROL` negotiation to
characterize the type of send and receive needed.


## Execution model

Cryo normally runs as a long-running daemon (`cryo daemon`, say), not
from cron. It maintains an internal scheduler: since it already knows,
for every discovered policy, when it last fired (`cryo:last`/
`cryo:last-<name>`) and how often it's due, it computes the single
earliest next action across everything it manages and sleeps until
then, rather than polling on a fixed interval.

Optionally, the daemon also watches `zpool events -f` (the same kernel
event stream `zed` consumes, read directly rather than through zed
itself, to avoid any dependency on zed being installed/running or any
need to touch system-level `zed.d` configuration) so that a `zfs set
cryo:...` or a dataset creation is picked up promptly instead of waiting
out the sleep. This is purely a responsiveness optimization, never a
correctness dependency — the timer-driven sweep still has to exist and
fire on its own regardless of whether any event was observed, since a
policy's cadence is fundamentally time-based.

A `cron`-style fallback mode (`cryo run`, say) exists for anyone who
would rather not run a persistent process: it evaluates whatever is due
right now across everything it discovers, and exits. This mirrors
zest's own `-SP --all` cron invocation.

The daemon keeps its full discovered state in memory — every policy,
every cryo-managed snapshot and its claims — rather than re-querying
zfs for each decision made along the way. It resyncs that state in two
ways: periodically, on a bounded safety-net interval, independent of
any event (covering drift from missed events, manual `zfs`
edits, or anything else); and early, on a wake from `zpool events -f`
as described above.

A running daemon exposes a small local control channel (a Unix domain
socket) that every other `cryo` invocation checks first. If a daemon is
reachable there, a subcommand like `run` or `status` becomes a small
request over that socket, answered from the daemon's cached state
rather than a fresh scan; if no daemon is reachable, it falls back to
scanning zfs and doing the work inline itself, exactly as it would with
no daemon running at all. Two things operate only through this channel:
an explicit resync request (forces an immediate full re-scan, bypassing
the periodic interval) and a stop request (asks the daemon to shut down
cleanly). This local control protocol is deliberately separate from,
and much simpler than, the remote replication wire protocol above — it
only ever carries small control messages between a CLI invocation and
its own local daemon, never a streaming payload, so there's no reason
for its framing to solve the same problems.


## Command-line interface

There is no verb/flag surface for managing policy — that's `zfs
set`/`zfs inherit` directly, on the documented `cryo:*` properties.
Cryo's own CLI is subcommand-based for the actions that remain its own
responsibility: running the daemon, running a one-shot sweep, acting as
the replication server side of a session, and read-only introspection
into what's scheduled/claimed/retained. `cryo --server` from early
drafts of this design becomes the subcommand `cryo server`.

The exact subcommand set is worked out alongside implementation, not
fixed here.


## Implementation notes

Language: Rust. Use the guidelines from
~/.claude-default/modes/dev/init.md and the `__template_rust` starter
from the same folder.

Platforms: Linux and FreeBSD (both current OpenZFS), using portable
Rust `std` only. Interaction with ZFS is via shelling out to the
`zfs`/`zpool` binaries using their machine-readable output modes (`-H
-p`), not via `libzfs_core` FFI bindings — the CLI surface is
essentially identical between Linux and FreeBSD OpenZFS releases, so
this buys FreeBSD support for free, and avoids tying the build to a
specific native library ABI per platform/version. illumos/Solaris
descendants and the macOS OpenZFS port are not design targets; they may
happen to work, but their CLI/feature divergence (older property
semantics, inconsistent raw-send/resume support) isn't worth designing
or testing for at this scale.

Privilege/authorization: cryo has no authorization layer of its own.
The entire trust boundary is external to the tool — whoever can ssh in
as the configured user, and whatever that user's `zfs allow`/sudo
grants permit, is implicitly authorized to do whatever `cryo server`
does on their behalf. Keeping this boundary outside the tool is a
deliberate choice, not an oversight: cryo should not be in the business
of reimplementing access control that ssh and `zfs allow` already
provide.

Docs: Embed the man page in the program, as specified in
~/.claude-default/modes/dev/init.md.

History: Herein are also docs for zest. Zest is the ancestor of the
snapshot LCM portion of cryo. This document is for reference only.
There's no obligation to repeat any of it. Migrating an existing zest
deployment (its `zest:*` properties and `zest-`-tagged snapshots) to
cryo is explicitly out of scope for cryo itself; that's a separate,
standalone converter tool, to be written later.
