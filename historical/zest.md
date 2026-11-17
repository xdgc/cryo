ZFS Snapshots with Zest
=======================

Zest is a python tool for managing a zoo of zyclical ZFS znapshots on
a FreeBSD (e.g. FreeNAS) or Linux zystem. It is a viable substitute for
the snapshot manager built into the FreeNAS GUI. On Linux (or Solaris or
OSX) it provides capabilities not built into the platform.

Wait but why replace the FreeNAS GUI snapshot manager?
------------------------------------------------------

Two primary reasons:

* It’s integrated into the GUI, and cannot be managed from the command
  line. The configuration for the snapshot manager is stored in the
  Django ORM database that the GUI is built on, and there’s no way to
  manually configure or update anything.

* Its inheritance model is bad. If you set 5 snapshot cycles (one for
  hourly, one for daily, etc) on pool1, it’s inherited by every dataset
  below pool1. Great! But if you want an exception for pool1/junk,
  you’re out of luck. There’s no way to do that. You have to remove
  the snapshot policies for the pool, and put in policies for each
  sub-dataset. This takes you from 5 policies, which is already a bit
  unwieldy in the crummy web UI, to 5*(number of datasets). That’s 30,
  35, 40 separate items to click, configure in a dialog, and save.

There are other benefits too, but those are the main motivators.

So what does Zest do differently?
---------------------------------

Zest runs from cron. It runs on whatever schedule you want. Currently
on bagofholdine it runs every five minutes, but that doesn’t mean it
takes snapshots every five minutes. The cron period should be no less
than the minimum interval of your snapshot policies, but it can be more
often. (Strictly speaking, by the Nyquist rule, it should run twice as
often as your most frequent snapshot — but that’s probably a little
anal-retentive.)

Zest takes snapshots when they’re due. If it runs and no snapshot is
needed yet, it doesn’t make one. When it runs again, if enough time has
elapsed to make a snapshot due, it takes it.

You can run zest manually to take an “instant” snapshot if you’re
preparing for some risky work.

Zest stores its configuration inside ZFS itself. ZFS supports arbitrary
user-defined properties on pools and on datasets. (N.B. “Dataset” is an
all-encompassing term for “filesystem or volume”, but in practice we
often call a filesystem a volume too.) Zest takes advantage of this to
store snapshot policy for pool1/foo in a property on pool1/foo named
zest:policies. This means that snapshot policies are inherited (like
all ZFS properties) by default, but can be overridden on a per-dataset
basis. It also means that there is no state on local disk. A snapshot
policy is imported and exported with the pool itself. If you upgrade
the OS, all you need to do to resume the previous snapshot policy is to
start running zest again.

State is also stored in ZFS. The zest:last property records when a
dataset was last snapshotted. This determines when the next snapshot
should occur. Snapshots themselves use the zest:created and zest:expires
properties to track the lifecycle of a single snapshot; when the time
indicated by zest:expires elapses, the snapshot is automatically deleted
by the cron job as a new one is created.

How is it used?
---------------

Zest runs ZFS commands; as such, it must be run as root. (If you have
used zfs allow to grant access on specific volumes or datasets to an
unprivileged user, it can run as that user.)

Automation
----------

Here is a cron job that does all the automated management:

*/5 * * * * /mnt/volume1/system/zest -SP --all

This means: every five minutes, perform all automatic snapshots (-S)
and all automatic purges/expirations (-P). This is the only realistic
use case for the -S or -P options, but you can run them solo at a
command prompt if you come up with a reason.

Policy notation
---------------

A policy is a colon-delimited expression of an interval and an
expiration. For example, 5m:2h means to take a snapshot every 5 minutes,
and leave each one for 2 hours. Put another way, you’ll have 2 hours’
worth of historical snapshots taken at 5-minute intervals. 1d:1y means
you’ll have a year’s worth of daily snapshots. 1d:365 is another way of
saying the same thing: keep 365 iterations of daily snapshots.

All time values are a single letter (minute, hour, day, week, year), except for month.

You’ll find that zest normalizes the value you set, always expressing
it using the largest unit(s) that fit. For example, 7d will turn into
1w if you set it on a policy. Likewise, 8d normalizes to 1w1d, since
8 days is a week plus a day and there’s no single larger unit that
divides 8 days evenly. (1w1d itself is already in this normalized
form, so it’s unchanged if you set it directly.)

Policy review
-------------

To see what policies are in place, use the -L/--list option. (sudo is
not required for listing.)

    $ zest -L
    volume1 | 5m:2h, 15m:12h, 1h:3d, 1d:1m, 1w:1y, 1mo:3y
    volume1/.system | 1h:3d, 1d:1m, 1w:1y
    volume1/.system/configs-0dc2ca1e7fa9464d8c4d7c4fd81f6855 | 1h:3d, 1d:1m, 1w:1y
    volume1/.system/cores | 1h:3d, 1d:1m, 1w:1y
    volume1/.system/rrd-0dc2ca1e7fa9464d8c4d7c4fd81f6855 | 1h:3d, 1d:1m, 1w:1y
    volume1/.system/samba4 | 1h:3d, 1d:1m, 1w:1y
    volume1/.system/syslog-0dc2ca1e7fa9464d8c4d7c4fd81f6855 | 1h:3d, 1d:1m, 1w:1y
    volume1/backup | 5m:2h, 15m:12h, 1h:3d, 1d:1m, 1w:1y, 1mo:3y
    volume1/boneyard | 1h:3d, 1d:1m, 1w:1y, 1mo:3y
    volume1/dgctest | 5m:3h, 15m:6h
    volume1/general | 5m:2h, 15m:12h, 1h:3d, 1d:1m, 1w:1y, 1mo:3y
    volume1/glue | 1h:3d, 1d:1m, 1w:1y, 1mo:3y
    volume1/homes | 5m:2h, 15m:12h, 1h:3d, 1d:1m, 1w:1y, 1mo:3y
    volume1/jails/generic | 5m:2h, 15m:12h, 1h:3d, 1d:1m, 1w:1y, 1mo:3y
    volume1/mmedia | 5m:2h, 15m:12h, 1h:3d, 1d:1m, 1w:1y, 1mo:3y
    volume1/private | 5m:2h, 15m:12h, 1h:3d, 1d:1m, 1w:1y, 1mo:3y
    volume1/system | 5m:2h, 15m:12h, 1h:3d, 1d:1m, 1w:1y, 1mo:3y
    volume1/vmds01 | 5m:2h, 15m:12h, 1h:3d, 1d:1m


This lists datasets that have active zest policies (that is, datasets
with a zest:policies property set) and the list of policies in that
property. Note that most of these are simply inherited — they were never
set explicitly.

Selecting datasets
-------------------

Wherever a `dataset` argument is accepted, it's matched by substring by
default: `zest -A 1d:3d kvmpool` matches `kvmpool` *and* `kvmpool/vms`
*and* anything else with "kvmpool" anywhere in its name. This is handy
for quick interactive use, but it means a parent dataset's name can
accidentally also match a child's, which can clobber the child's own
policy if you apply changes to the parent first.

Prefix the argument with `=` to force an exact match instead:
`zest -A 1d:3d =kvmpool` matches only `kvmpool`. Globs (`?`, `*`, `[`,
`{`) and `/regex/` syntax are also recognized, with or without the `=`
prefix (an exact-match glob/regex still needs the whole name to match,
not just part of it). Scripted/automated usage — like
zest_set_defaults, which sets policy on a list of datasets that may
include both a parent and a child — should always use `=` to avoid
this trap.

Modifying policy
----------------

Use the -A and -R options (or --add/--remove) to alter policy on a
dataset. Note that only one policy can exist for a given internal. For
example, to remove the 1d:1m policy on vmds01 and replace it with a
1d:2w policy, just run

    $ sudo zest -A 1d:2w volume1/vmds01 

The old 1d policy will be replaced. To replace 1d:1m with 1w:3m, remover first then add:

    $ sudo zest -R 1d:1m volume1/vmds01
    $ sudo zest -A 1w:3m volume1/vmds01

Changing a dataset’s snapshot policy will not affect existing snapshot
expiration. Once a snapshot is made, it will always expire at the same
time as when it was created. (It might be desirable to implement an
option to update expiration times according to current policy.)

Listing snapshots
-----------------

The -L/--list option can be used in conjunction with -v/--verbose to
show all the snapshots managed by zest. Here’s a severely truncated (in
the interest of space) example:

    $ zest -Lv
    volume1 | 5m:2h, 15m:12h, 1h:3d, 1d:1m, 1w:1y, 1mo:3y (149 snapshots)
    - 20161026.0000-1mo
    - 20161117.0000-1w
    - 20161124.0000-1w
    ...
    
    volume1/.system | 1h:3d, 1d:1m, 1w:1y (73 snapshots)
    - 20161124.0000-1w
    - 20161127.0300-1h
    - 20161127.0400-1h
    ...
    
    volume1/boneyard | 1h:3d, 1d:1m, 1w:1y, 1mo:3y (75 snapshots)
    - 20161026.0000-1mo
    - 20161124.0000-1w
    - 20161125.0000-1mo
    ...


These snapshot names are abbreviated. If you were to interact with them
using the zfs command directly, YYYYMMDD.HHMM-NX would be, for example,
volume1/boneyard@zest-YYYYMMDD.HHMM-NX.

Expiring old snapshots
----------------------

As above, a cron job typically manages old snapshot expiration, but you
can force it to happen by running sudo zest -P. That’s not useful if
you’re running the cron job, but it could be useful to run standalone
with the --older/--newer options. This lets you force-expire snapshots
older (or newer) than some age. For example, to expire all vmds01
snapshots older than 10 days:

    $ sudo zest -P --older 10d volume1/vmds01 

or:

    $ sudo zest -P --older 1w3d volume1/vmds01

Deleting snapshots
------------------

Sometimes it’s useful to manually delete a specific snapshot. Do
that with -D/--delete. Use the -n/--no/--dry-run option to prevent
actually deleting anything, if you just want to see what’s going to
happen. Here’s part of the output of an example run:

    $ sudo zest -n -D vmds01
    ignoring volume1/vmds01@auto-20161123.1445-6h - not a managed snapshot
    ignoring volume1/vmds01@auto-20161123.1451-1m - not a managed snapshot
    deleting volume1/vmds01@zest-20161127.0300-1h
    deleting volume1/vmds01@zest-20161127.0400-1h
    deleting volume1/vmds01@zest-20161127.0500-1h


Note that the deletion skips over snapshots whose name does not contain
“zest” as the snapshot name prefix. (This can be changed using the
-t/--tag option; indeed you could manage two wholly distinct sets of
snapshot policies by using that option cleverly.) If you really want to
delete the other snapshots too, add -f/--force:

    $ sudo zest -n -D vmds01
    deleting volume1/vmds01@auto-20161123.1445-6h
    deleting volume1/vmds01@auto-20161123.1451-1m
    deleting volume1/vmds01@zest-20161127.0300-1h
    deleting volume1/vmds01@zest-20161127.0400-1h
    deleting volume1/vmds01@zest-20161127.0500-1h

Instant snapshots
-----------------

Running zest -S will only take whatever snapshots are (over)due at the
moment. If more than one is due, it will take only the most recent,
since they would be the same. And if your policy says to keep 10
iterations and one gets missed for some reason, then you’ll keep only
9. The number of iterations to keep really just informs how long a
single snapshot is kept.

(Note that snapshots include the time at which they were taken — more
or less. Snapshot times are rounded down to a multiple of the previous
interval. In other words, they’re marked as taken at the time at which
they came due, not at the time they were actually made. The zest:created
stamp on the snapshot has the actual time.)

Sometimes, though, you want a snapshot now, regardless of what’s
expected in the normal scheme of things. For that, use zest -I. This
takes a snapshot right now of any datasets that you name on the command
line, or of all datasets if you pass -a/--all. (Unlike some other
operations, -I requires one or the other — it won’t default to all
datasets just because none were named.)

Instant snapshots are named differently — for example,
volume1/vmds01@zest-instant-YYYYMMDD.HHMM instead of
volume1/vmds01@zest-YYYYMMDD.HHMM. And, significantly, instant snapshots
will never auto-expire. You’ll need to delete them manually (with
-D/--delete) or with -P --older to get rid of them.

Usage
-----

Here is the live usage statement for zest. Note that all actions
(verbs) use capital letters for short options, while options that modify
actions use lowercase.

    usage: zest [-h] [-v] [-n] [-a] [-t TAG] [-f] [--older age] [--newer age]
                [-A ADD] [-R REMOVE] [-S] [-I] [-P] [-D] [-L]
                [dataset [dataset ...]]
    
    positional arguments:
      dataset
    
    optional arguments:
      -h, --help            show this help message and exit
      -v, --verbose         verbose
      -n, --no, --dry-run   don't make changes
      -a, --all             select all datasets
      -t TAG, --tag TAG     zfs property tag prefix (default "zest")
      -f, --force           force deletion even if a snapshot is not managed
      --older age           age older than which to purge
      --newer age           age newer than which to purge
    
    Operations:
      -A ADD, --add ADD     add snapshot policy to selected datasets
      -R REMOVE, --remove REMOVE
                            remove snapshot policy from selected datasets
      -S, --snapshot        perform routine snapshots on selected datasets
      -I, --instant         do an immediate snapshot of all selected datasets
      -P, --purge           purge expired snapshots
      -D, --delete          delete named snapshots
      -L, --list            list eligible datasets


Where does it come from?
------------------------

I wrote zest here at ExtraHop, based on a prior implementation called
zfstool which I developed during my first ZFS experiences around
2008. It uses the same concepts for snapshot definition and state
storage, but improves on the UI and operational resilience. So now it
comes from ehit/projects/zfs, unless we want to open-source it.
