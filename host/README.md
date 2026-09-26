# Host build of this AoE fork

This directory is the copy that matters. It is local to this fork and is **not
for an upstream pull request**. `local/host-ops` carries it, cut from the
release tag `v1.16.1`, and `kitchensink` merges that branch last so a rebuild
does not drop the scripts.

An older, longer copy still sits in `~/brain/aoe-build-cookbook.md` and
`~/brain/aoe-g4-build-environment.md`, with the scripts under
`~/brain/scripts/aoe-remote-build/`. That tree is read-only. Its branch list
stops at `feat/reasoning-thought-display` and its refresh base notes still
say `v1.16.0`. Follow this file. The brain copy still has the per-branch
incident notes (why a patch exists, what to re-check). Read those before
rebasing a branch you do not remember.

**Never run `aoe update`.** It replaces the installed binary and discards
every local patch.

| what | where |
|---|---|
| checkout | the repo that contains this file |
| upstream | `origin` = `git@github.com:agent-of-empires/agent-of-empires.git` |
| fork | `fork` = `git@github.com:csillag/agent-of-empires.git` |
| build host repo | `g4` = `ssh://csillag@brain-cell-g4.lan/srv/aoe-build/excalibur-arm64/home/csillag/aoe.git` |
| installed binary | `~/local/libexec/aoe`, reached through `~/local/bin/aoe` |
| remote build | `./host/aoe-remote` (`~/local/bin/aoe-remote` points here) |
| built binaries and logs | `~/aoe-build/aoe-<sha8>`, `~/aoe-build/remote-logs/` |
| where the compile happens | `host/g4.md` |

The base is the latest release tag we have chosen to track, **not**
`origin/main`. That tag is `v1.16.1`. Tags `v1.17.0` and `v1.17.1` exist
upstream. Stay on `v1.16.1` until the owner says the infrastructure is ready
to move.

## Branch layout

`main` tracks `origin/main` and never carries our commits. One topic branch
per change, cut from `v1.16.1` or from the branch it builds on. Stacking is
normal. Record the parent in `host/branches.txt`.

`kitchensink` is the only branch we build. It is upstream's tag plus a merge
of `host/branches.txt`, in that order. History is disposable and is recreated
every refresh. It is pushed to `fork` as a backup, with `--force-with-lease`
after a refresh. It is never pushed to `origin`.

`devendor-c-deps` is local-only. It drops vendored OpenSSL and bundled
sqlite and links the distro libraries, so the build needs
`LIBGIT2_NO_VENDOR=1`. Upstream wants a portable vendored binary. Do not
open a pull request for it.

The fork is public. Nothing secret goes on these branches.

## Refresh, when upstream has a new tag we mean to take

```sh
cd ~/local/src/agent-of-empires
git fetch origin --tags
# BASE is the new tag. OLD is the tag kitchensink is on now.
branches() { sed -e 's/#.*//' -e '/^[[:space:]]*$/d' host/branches.txt; }
```

Snapshot every tip first (`git branch -f prerefresh/$b $b`), including
`kitchensink`. Rebase each topic branch onto `BASE`, parent before child.
The stacks are commented in `host/branches.txt`. A conflict belongs on that
topic branch. Resolving it on `kitchensink` makes the next cycle worse.

A clean rebase is not a correct one. `devendor-c-deps` in particular: audit
new dependencies for `vendored`, `bundled`, and `*-sys` crates. Expect
`openssl-src` absent, `libsqlite3-sys` not bundled, and `zstd-sys` using
pkg-config.

Then rebuild kitchensink. Merges are not guaranteed clean. Past kitchensink
merges hold hand resolutions (SpawnConfig fields, SessionViewHost) that live
in the merge commits, not in the topic branches. Expect to redo those.

```sh
git checkout -B kitchensink "$BASE"
for b in $(branches); do
    git merge --no-ff -m "kitchensink: merge $b" "$b" || break
done
git push --force-with-lease fork $(branches) kitchensink
```

Do not force-push `main` or anything to `origin`.

## Build

Remote, on a brain cell, is the way to compile. superego takes about 18
minutes and starves every other agent. g4 takes about 5.5 minutes when the
target dir is warm. The full gate, and the only one before a deploy:

```sh
./host/aoe-remote <kitchensink-sha> build lint libtest itest web
```

That tests the commit that will be installed. g4 runs one job at a time, so
a topic-branch run ahead of it only delays the deploy. Jobs: `build` writes
`~/aoe-build/aoe-<sha8>`, `lint` is fmt and clippy, `libtest` is `cargo test
--lib`, `itest` is the integration suite, `web` is the dashboard. Logs are
`~/aoe-build/remote-logs/<sha8>-<jobs>.log`. Exit 0 only if every job passed.

`./host/aoe-remote --host g3 <sha> build` picks another cell.
`./host/aoe-remote --hosts g4,g3,g2 <sha> build lint libtest itest web`
spreads the jobs. How the chroot is built and kept matched to superego's
libraries is `host/g4.md`.

A local build, when a remote one is the wrong tool:

```sh
LIBGIT2_NO_VENDOR=1 nice -n 19 cargo build --profile dev-release --features web -j 6
```

Give it its own `CARGO_TARGET_DIR` (`/var/tmp/aoe-target/<branch>`). Two
worktrees must not share one. `nice -n 19` is the rule, not a courtesy.

## Install

`aoe-install` is not on this machine. The swap it used to do:

```sh
install -m 0755 ~/aoe-build/aoe-<sha8> ~/local/libexec/aoe.new
cp -a ~/local/libexec/aoe ~/local/libexec/aoe.prev
mv -f ~/local/libexec/aoe.new ~/local/libexec/aoe
~/local/bin/stop-aoe-server
~/local/bin/start-aoe-server
```

The installed file's name must be `aoe`. The wrapper execs
`~/local/libexec/aoe` and never `target/<profile>/aoe`, which is empty
mid-build. Copy, then `mv`. Do not overwrite the running inode. Stop and
start back to back: between them a new runner would spawn from a path the
daemon no longer holds.

Before the restart, nothing may be waiting on the owner. An open question
or approval dies with the daemon and does not come back. Runners that were
already up keep the old binary until they are respawned (`aoe acp restart
<full-session-id>`). A short id from `aoe ps` is not enough.

Rollback: `mv -f ~/local/libexec/aoe.prev ~/local/libexec/aoe` and restart
the same way.

Check the binary, not the build log. `ldd ~/local/libexec/aoe` must resolve
`libgit2.so.1.9`, `libsqlite3`, `libssl`, `libcrypto`, and `libzstd`, with
no `not found`. `readlink -f /proc/<daemon-pid>/exe` must be that file, and
`cmp` against `~/aoe-build/aoe-<sha8>` must agree. `/proc/<pid>/exe` and the
path can show different inode numbers; compare the bytes.
