# justfile for cryo -- zfs snapshot lifecycle and replication.
#
# Install paths follow the usual PREFIX/DESTDIR convention, so either
# works: `just prefix=/usr install`, or `DESTDIR=/pkg/stage just install`
# for a staged/packaging build. Plain string concatenation, not path
# joining, on purpose -- `prefix` is already absolute, and a path-join
# operator would (correctly, per POSIX join semantics, but not what we
# want here) discard `destdir` entirely whenever it meets an absolute
# path.
prefix := env_var_or_default("PREFIX", "/usr/local")
destdir := env_var_or_default("DESTDIR", "")

bin := "target/release/cryo"
bindir := destdir + prefix + "/bin"
mandir := destdir + prefix + "/share/man/man1"
docdir := destdir + prefix + "/share/doc/cryo"
unitdir := destdir + "/etc/systemd/system"

# Elevate only when actually installing onto the real system -- a
# staged (`DESTDIR`) build writes into a scratch directory the
# invoking user already owns, and should neither need nor want a
# password prompt. Keeping this conditional inside the recipe (rather
# than making the caller run `sudo just install`) also means `release`
# -- a dependency of `install`, so it always runs as part of it --
# never gets rebuilt as root by accident.
sudo := if destdir == "" { "sudo" } else { "" }

# Bare `just` lists what's available rather than silently doing
# anything.
default:
    @just --list

# Debug build.
build:
    cargo build

# Optimized build -- what `install` actually ships.
release:
    cargo build --release

check:
    cargo clippy --all-targets

test:
    cargo test

# Same sequence we run by hand after every change: lint, test, build.
verify: check test build

clean:
    cargo clean

# Copies the release binary, man page, and design doc under
# $DESTDIR$PREFIX, and the systemd unit under $DESTDIR/etc/systemd --
# but doesn't reload/enable/start it when DESTDIR is set, since a
# staged install has no business touching the build host's real
# systemd. See `enable`/`start` for actually running it on a real
# target host, kept as separate, explicit steps on purpose.
install: release
    {{ sudo }} install -D -m 755 {{ bin }} {{ bindir }}/cryo
    {{ sudo }} install -D -m 644 man/cryo.1 {{ mandir }}/cryo.1
    {{ sudo }} install -D -m 644 cryo.md {{ docdir }}/cryo.md
    {{ sudo }} install -D -m 644 systemd/cryo.service {{ unitdir }}/cryo.service
    [ -n "{{ destdir }}" ] || {{ sudo }} systemctl daemon-reload

uninstall:
    {{ sudo }} rm -f {{ bindir }}/cryo
    {{ sudo }} rm -f {{ mandir }}/cryo.1
    {{ sudo }} rm -f {{ docdir }}/cryo.md
    {{ sudo }} rm -f {{ unitdir }}/cryo.service
    [ -n "{{ destdir }}" ] || {{ sudo }} systemctl daemon-reload

# Enables the service at boot without starting it now. Always the
# real system, so always elevated -- there's no staged equivalent of
# "enabled at boot".
enable:
    sudo systemctl enable cryo.service

# Starts the service now without changing its boot-enablement.
start:
    sudo systemctl start cryo.service
