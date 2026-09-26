#!/bin/sh
# Runs INSIDE the g4 build chroot as csillag. job.sh <sha> <job>...
# Jobs: build lint libtest itest web. One shared worktree and target dir, so
# consecutive shas build incrementally; run-job serializes callers.
set -u
SHA=$1; shift
umask 0022
export LIBGIT2_NO_VENDOR=1 CARGO_TARGET_DIR=$HOME/target PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
J=$(nproc)
SRC=$HOME/src
[ -d "$SRC/.git" ] || git clone -q "$HOME/aoe.git" "$SRC" || exit 1
cd "$SRC" || exit 1
git fetch -q "$HOME/aoe.git" "refs/jobs/$SHA" && git checkout -q -f --detach "$SHA" && git clean -q -ffdx -e /web/dist || exit 1
# build.rs regenerates web/dist only when web sources change, so keep it
# through the clean and force a rerun if it is missing anyway.
[ -d web/dist ] || touch web/index.html
echo "=== source $(git rev-parse HEAD) ($(git log -1 --format=%s))"

run() { # run <label> <cmd...>
    echo "=== STEP $1: $2"
    start=$(date +%s)
    sh -c "$2" 2>&1
    rc=$?
    echo "=== STEP $1 EXIT=$rc ($(( $(date +%s) - start ))s)"
    return $rc
}

overall=0
for job in "$@"; do
    case $job in
      build)   run build "cargo build --profile dev-release --features web -j $J" &&
               mkdir -p "$HOME/out" && cp "$CARGO_TARGET_DIR/dev-release/aoe" "$HOME/out/aoe-$SHA" &&
               ls -t "$HOME"/out/aoe-* | tail -n +6 | xargs -r rm -f ;;
      lint)    run fmt "cargo fmt --check"; r1=$?
               run clippy "cargo clippy --all-targets -j $J"; [ $? -eq 0 ] && [ $r1 -eq 0 ] ;;
      libtest) run libtest "cargo test --lib -j $J" ;;
      itest)   run shim "cd acp-worker/test-shim && npm ci --no-audit --no-fund" &&
               run itest "cargo test --features web --test integration -j $J" ;;
      web)     run web-ci "cd web && npm ci --ignore-scripts --no-audit --no-fund" || { overall=1; echo "=== JOB web EXIT=1"; continue; }
               r=0
               for c in "npm run format:check" "npm run lint" "npx tsc -b" "npx vitest run" "node tests/validate-coverage-matrix.mjs"; do
                   run "web: $c" "cd web && $c" || r=1
               done
               [ $r -eq 0 ] ;;
      *)       echo "unknown job $job"; false ;;
    esac
    rc=$?
    echo "=== JOB $job EXIT=$rc"
    [ $rc -eq 0 ] || overall=1
done
exit $overall
