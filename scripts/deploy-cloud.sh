#!/usr/bin/env bash
#
# deploy-cloud.sh -- build and deploy Shirley (server / cloud-web mode) to a Linux host.
#
# Usage:
#   scripts/deploy-cloud.sh [command] [options]
#
# Commands:
#   deploy     (default) build frontend + cross-compile + upload + atomic switch + restart + health check
#   build      build only (frontend dist + static musl binary); no access to the server
#   rollback   switch `current` back to a previous release and restart (default: the one before current)
#   releases   list releases on the server, marking the active one
#
# Options:
#   --host USER@HOST        ssh target (required; or set $SHIRLEY_DEPLOY_HOST)
#   --port PORT             ssh port (default: $SHIRLEY_DEPLOY_PORT, else 22)
#   --skip-web              do not run `npm run build` (reuse the existing dist)
#   --skip-build            do not compile; upload the existing target binary
#   --dry-run               print every remote action without executing it
#   --no-rollback           do not auto-rollback when the post-restart health check fails
#   --keep N                keep only the N newest releases after a successful deploy (default: keep all)
#   --release NAME          (rollback only) switch to this release instead of "current - 1"
#   -h, --help              this help
#
# Environment:
#   SHIRLEY_DEPLOY_HOST     ssh target (required unless --host is given)
#   SHIRLEY_DEPLOY_PORT     default ssh port
#   SHIRLEY_DEPLOY_ROOT     remote install root (default /opt/shirley)
#   SHIRLEY_DEPLOY_USER     remote owner of the release tree (default shirley)
#   SHIRLEY_DEPLOY_SERVICE  systemd unit name (default shirley)
#   SHIRLEY_DEPLOY_HEALTH   URL probed from the server after restart (default http://127.0.0.1:8787/)
#   SHIRLEY_DEPLOY_TOKEN    token for the authenticated probe (default: read from
#                           <root>/cloud.env on the server, if present)
#   SHIRLEY_BUILD_HOME      HOME used for the zig/cargo build (zig cannot resolve its cache dir
#                           when HOME is unset; defaults to the invoking user's HOME)
#   ZIG_LOCAL_CACHE_DIR / ZIG_GLOBAL_CACHE_DIR  default /tmp/zig-cache and /tmp/zig-global
#
# Why it is built this way (read before changing it):
#
#   1. **Static musl, not glibc.** The target hosts are old (CentOS 7 / glibc 2.17). A glibc-linked
#      binary either refuses to start or drags in a newer libc than the host has. `cargo-zigbuild`
#      with `x86_64-unknown-linux-musl` produces a fully static, stripped ELF; verify with `file`
#      (this script does, and aborts if the result is not "statically linked").
#   2. **Releases are immutable and timestamped (UTC).** Every deploy writes a brand-new
#      `<root>/releases/<UTC timestamp>/`; nothing is ever edited in place. That makes rollback a
#      pure symlink flip and makes "what was running on day X" answerable.
#   3. **`current` is swapped atomically.** `ln -sfn ... current.tmp && mv -Tf current.tmp current`
#      replaces the symlink in one rename(2). Never `ln -sfn <new> current` directly: that unlinks
#      and recreates the link, leaving a window where `current` does not exist and the unit's
#      ExecStart path is missing.
#   4. **The web dist ships with the binary.** The server serves the SPA from `SHIRLEY_WEB_DIST`
#      (= `<root>/current/web`), so a stale dist is a stale UI. Both go into the same release dir,
#      so they can never skew.
#   5. **Health checks are bounded and must not use `/api/status`.** That endpoint is an SSE stream
#      (`Sse<impl Stream>`): with no pushes the body stays empty and a plain `curl` hangs until the
#      timeout. Probe `GET /` (expect 200 + the asset names referenced by index.html) and an
#      unauthenticated `POST /api/cmd/<cmd>` (expect 401). Every remote call carries a timeout.
#   6. **Failure after the switch rolls back.** If the unit does not become active, or the health
#      probe fails, the script re-points `current` at the previous release and restarts, so a bad
#      build does not leave production down.
#
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# defaults
# ---------------------------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

TARGET="x86_64-unknown-linux-musl"
FEATURES="server"
BIN_NAME="Shirley"
WEB_DIR="${REPO_ROOT}/crates/shirley/src/interface/desktop/web"

DEPLOY_HOST="${SHIRLEY_DEPLOY_HOST:-}"
DEPLOY_PORT="${SHIRLEY_DEPLOY_PORT:-22}"
REMOTE_ROOT="${SHIRLEY_DEPLOY_ROOT:-/opt/shirley}"
REMOTE_USER="${SHIRLEY_DEPLOY_USER:-shirley}"
SERVICE="${SHIRLEY_DEPLOY_SERVICE:-shirley}"
# Empty means "derive it from the unit" (see resolve_health_url): hard-coding 127.0.0.1:8787 would
# silently fail on any host that binds elsewhere, and a failed health check triggers an auto-rollback.
HEALTH_URL="${SHIRLEY_DEPLOY_HEALTH:-}"
HEALTH_TOKEN="${SHIRLEY_DEPLOY_TOKEN:-}"

COMMAND="deploy"
SKIP_WEB=0
SKIP_BUILD=0
DRY_RUN=0
AUTO_ROLLBACK=1
KEEP_RELEASES=""
ROLLBACK_TO=""

# ---------------------------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------------------------
log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  ok\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarn\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mfail\033[0m %s\n' "$*" >&2; exit 1; }

usage() { awk 'NR>1 && /^set -euo/{exit} NR>1{sub(/^# ?/,""); print}' "$0"; exit 0; }

# Every remote invocation goes through here: one place for the connect timeout, the batch flag,
# and the dry-run short circuit.
ssh_run() {
  if (( DRY_RUN )); then
    printf '  \033[2m[dry-run] ssh %s -- %s\033[0m\n' "$DEPLOY_HOST" "$*"
    return 0
  fi
  ssh -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=15 -p "$DEPLOY_PORT" \
      "$DEPLOY_HOST" "$@"
}

# Retry a remote-facing command a couple of times. Long uploads over a flaky link can be cut by the
# server ("Connection closed by remote host"); a single retry turns that into a non-event.
retry() {
  local tries="${1:?retry needs a count}"; shift
  local n=1
  until "$@"; do
    if (( n >= tries )); then
      warn "gave up after ${tries} attempt(s): $*"
      return 1
    fi
    warn "attempt ${n}/${tries} failed, retrying: $*"
    n=$((n + 1))
    sleep 2
  done
}

scp_put() {
  local src="$1" dst="$2"
  if (( DRY_RUN )); then
    printf '  \033[2m[dry-run] scp -r %s -> %s:%s\033[0m\n' "$src" "$DEPLOY_HOST" "$dst"
    return 0
  fi
  # NOTE: scp's port flag is capital -P (lowercase -p means "preserve times" and would make the
  # port number be parsed as a source file -> "scp: stat local \"22\": No such file or directory").
  retry 3 scp -o BatchMode=yes -o ConnectTimeout=10 -P "$DEPLOY_PORT" -q "$src" "${DEPLOY_HOST}:${dst}"
}

# Upload a directory as a single tar stream over one ssh connection. Per-file scp of the web dist
# (~450 small files) is slow and gets cut by the server mid-transfer; a streamed tar is one
# connection, much faster, and restarts cleanly via `retry`.
put_dir_tar() {
  local src="$1" dst="$2"
  if (( DRY_RUN )); then
    printf '  \033[2m[dry-run] tar | ssh %s -- tar -x -C %s (from %s)\033[0m\n' "$DEPLOY_HOST" "$dst" "$src"
    return 0
  fi
  retry 3 tar_stream "$src" "$dst"
}

tar_stream() {
  local src="$1" dst="$2"
  # `tar -C src -cf - .` keeps relative paths; the remote extracts into an already-existing dir.
  # If the stream dies halfway, retry re-extracts over the partial tree (tar overwrites files).
  tar -C "$src" -cf - . \
    | ssh -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=15 -p "$DEPLOY_PORT" \
        "$DEPLOY_HOST" "tar -xf - -C '$dst'"
}

# ---------------------------------------------------------------------------------------------
# argument parsing
# ---------------------------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
  case "$1" in
    deploy|build|rollback|releases) COMMAND="$1"; shift ;;
    --host)         DEPLOY_HOST="${2:?--host needs a value}"; shift 2 ;;
    --port)         DEPLOY_PORT="${2:?--port needs a value}"; shift 2 ;;
    --skip-web)     SKIP_WEB=1; shift ;;
    --skip-build)   SKIP_BUILD=1; shift ;;
    --dry-run)      DRY_RUN=1; shift ;;
    --no-rollback)  AUTO_ROLLBACK=0; shift ;;
    --keep)         KEEP_RELEASES="${2:?--keep needs a number}"; shift 2 ;;
    --release)      ROLLBACK_TO="${2:?--release needs a name}"; shift 2 ;;
    -h|--help)      usage ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

# A host is required for anything that touches the server (build-only does not need one).
if [[ "$COMMAND" != "build" && -z "$DEPLOY_HOST" ]]; then
  die "no target host: pass --host user@host or set SHIRLEY_DEPLOY_HOST"
fi

# ---------------------------------------------------------------------------------------------
# build: frontend
# ---------------------------------------------------------------------------------------------
build_web() {
  if (( SKIP_WEB )); then
    log "web: skipped (--skip-web)"
    [[ -f "${WEB_DIR}/dist/index.html" ]] || die "no dist at ${WEB_DIR}/dist -- drop --skip-web"
    return
  fi
  log "web: npm run build"
  command -v npm >/dev/null || die "npm not found"
  [[ -d "${WEB_DIR}/node_modules" ]] || die "run 'npm install' in ${WEB_DIR} first"
  ( cd "$WEB_DIR" && npm run build )
  [[ -f "${WEB_DIR}/dist/index.html" ]] || die "build finished but dist/index.html is missing"
  ok "web dist built ($(du -sh "${WEB_DIR}/dist" | cut -f1))"
}

# ---------------------------------------------------------------------------------------------
# build: static musl binary
# ---------------------------------------------------------------------------------------------
build_binary() {
  if (( SKIP_BUILD )); then
    log "binary: skipped (--skip-build)"
    [[ -f "${REPO_ROOT}/target/${TARGET}/release/${BIN_NAME}" ]] \
      || die "no binary at target/${TARGET}/release/${BIN_NAME} -- drop --skip-build"
    return
  fi

  command -v cargo-zigbuild >/dev/null || die "cargo-zigbuild not found: cargo install cargo-zigbuild"
  command -v zig >/dev/null || die "zig not found (cargo-zigbuild needs it on PATH)"
  rustup target list --installed 2>/dev/null | grep -qx "$TARGET" \
    || die "rust target not installed: rustup target add $TARGET"

  # zig cannot resolve its cache dir when HOME is unset (AppDataDirUnavailable), and rustup cannot
  # find the toolchain without a real HOME (~/.rustup) -- both happen in sandboxed shells where HOME
  # is empty. So: honour SHIRLEY_BUILD_HOME, else keep a non-empty HOME, else derive the real home
  # from the passwd entry (do NOT fall back to /tmp: rustup then says "no default toolchain").
  if [[ -z "${SHIRLEY_BUILD_HOME:-}" && -z "${HOME:-}" ]]; then
    local real_home
    real_home="$(python3 -c 'import pwd, os; print(pwd.getpwuid(os.getuid()).pw_dir)' 2>/dev/null || true)"
    [[ -n "$real_home" ]] || die "HOME is unset and cannot be derived; set SHIRLEY_BUILD_HOME to your home directory"
    export HOME="$real_home"
  else
    export HOME="${SHIRLEY_BUILD_HOME:-$HOME}"
  fi
  export ZIG_LOCAL_CACHE_DIR="${ZIG_LOCAL_CACHE_DIR:-/tmp/zig-cache}"
  export ZIG_GLOBAL_CACHE_DIR="${ZIG_GLOBAL_CACHE_DIR:-/tmp/zig-global}"
  mkdir -p "$ZIG_LOCAL_CACHE_DIR" "$ZIG_GLOBAL_CACHE_DIR"

  log "binary: cargo zigbuild --release --target ${TARGET} --features ${FEATURES}"
  ( cd "$REPO_ROOT" && cargo zigbuild --release --target "$TARGET" --features "$FEATURES" )

  local bin="${REPO_ROOT}/target/${TARGET}/release/${BIN_NAME}"
  [[ -f "$bin" ]] || die "build finished but $bin is missing"

  # Hard gate: a dynamically linked artifact would not run on the old CentOS hosts.
  if ! file "$bin" | grep -q 'statically linked'; then
    file "$bin" >&2
    die "binary is not statically linked -- do not deploy it to a glibc 2.17 host"
  fi
  ok "binary built ($(du -h "$bin" | cut -f1), static)"
}

# ---------------------------------------------------------------------------------------------
# deploy
# ---------------------------------------------------------------------------------------------
deploy() {
  log "target ${DEPLOY_HOST}:${DEPLOY_PORT}  root ${REMOTE_ROOT}"

  if ! (( DRY_RUN )); then
    ssh_run "true" >/dev/null 2>&1 || die "cannot ssh to ${DEPLOY_HOST} (check key / host / port)"
    ok "ssh reachable"
  fi

  build_web
  build_binary

  local ts bin dist release_dir previous
  ts="$(date -u +%Y%m%dT%H%M%SZ)"
  bin="${REPO_ROOT}/target/${TARGET}/release/${BIN_NAME}"
  dist="${WEB_DIR}/dist"
  release_dir="${REMOTE_ROOT}/releases/${ts}"

  log "release ${ts}"
  ssh_run "mkdir -p '${release_dir}/web'"

  log "upload binary + web dist"
  scp_put "$bin" "${release_dir}/shirley"
  put_dir_tar "$dist" "${release_dir}/web"

  log "fix ownership"
  ssh_run "set -e; chown -R '${REMOTE_USER}:${REMOTE_USER}' '${release_dir}'; chmod 755 '${release_dir}/shirley'"

  # Remember what we are replacing so a failed health check can be undone. (Dry-run never mutates
  # anything, so there is no previous release to report or roll back to.)
  previous=""
  if (( ! DRY_RUN )); then
    previous="$(ssh_run "readlink -f '${REMOTE_ROOT}/current' 2>/dev/null || true" | tr -d '\r' | head -n1)"
    if [[ -n "$previous" ]]; then log "previous: ${previous}"; else warn "no previous release (first deploy?)"; fi
  fi

  log "switch current -> ${ts}"
  ssh_run "set -e; ln -sfn '${release_dir}' '${REMOTE_ROOT}/current.tmp' && mv -Tf '${REMOTE_ROOT}/current.tmp' '${REMOTE_ROOT}/current'; readlink -f '${REMOTE_ROOT}/current'"

  log "restart ${SERVICE}"
  ssh_run "systemctl restart '${SERVICE}'"

  if health_check; then
    ok "deploy ${ts} healthy"
    prune_releases
    if [[ -n "$previous" ]]; then
      log "rollback command if needed:"
      printf '  %s rollback --release %s\n' "$0" "$(basename "$previous")"
    fi
    return 0
  fi

  if (( AUTO_ROLLBACK )) && [[ -n "$previous" ]]; then
    warn "health check failed -- rolling back to $(basename "$previous")"
    ssh_run "set -e; ln -sfn '${previous}' '${REMOTE_ROOT}/current.tmp' && mv -Tf '${REMOTE_ROOT}/current.tmp' '${REMOTE_ROOT}/current'; systemctl restart '${SERVICE}'"
    sleep 3
    health_check && warn "rolled back; previous release is serving" || warn "rollback also unhealthy -- manual attention required"
  else
    warn "health check failed and auto-rollback is off"
  fi
  exit 1
}

# ---------------------------------------------------------------------------------------------
# health check (bounded; never touches /api/status -- it is an SSE stream and would hang)
# ---------------------------------------------------------------------------------------------
# Resolve the URL to probe. Order: explicit SHIRLEY_DEPLOY_HEALTH > the unit's SHIRLEY_SERVER_ADDR
# (that is where the server actually listens) > 127.0.0.1:8787 as a last resort. Deriving it from the
# unit means the script works on a fresh host without the operator having to pass a private address.
resolve_health_url() {
  if [[ -n "$HEALTH_URL" ]]; then return 0; fi
  local addr=""
  if (( ! DRY_RUN )); then
    addr="$(ssh_run "systemctl show -p Environment '${SERVICE}' 2>/dev/null" 2>/dev/null \
      | tr ' ' '\n' | sed -n 's/^SHIRLEY_SERVER_ADDR=//p' | head -n1 | tr -d '\r')"
  fi
  if [[ -z "$addr" ]]; then
    warn "cannot derive SHIRLEY_SERVER_ADDR from unit ${SERVICE}; set SHIRLEY_DEPLOY_HEALTH"
    HEALTH_URL="http://127.0.0.1:8787/"
    return 0
  fi
  # A wildcard bind is not directly curl-able; probe loopback on the same port instead.
  local host="${addr%:*}" port="${addr##*:}"
  case "$host" in 0.0.0.0|'*'|'') host="127.0.0.1" ;; esac
  HEALTH_URL="http://${host}:${port}/"
}

health_check() {
  log "health check"
  if (( DRY_RUN )); then printf '  \033[2m[dry-run] health check skipped\033[0m\n'; return 0; fi
  resolve_health_url
  log "probe ${HEALTH_URL}"

  # 1) unit is active (give it a moment to come up)
  local state="" code refs missing="" base
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    state="$(ssh_run "systemctl is-active '${SERVICE}'" 2>/dev/null | tr -d '\r' || true)"
    [[ "$state" == "active" ]] && break
    sleep 1
  done
  if [[ "$state" != "active" ]]; then
    warn "unit ${SERVICE} is '${state}'"
    ssh_run "journalctl -u '${SERVICE}' -n 30 --no-pager" >&2 || true
    return 1
  fi
  ok "unit active"

  # 2) GET / -> 200, and index.html must reference assets that actually exist in this release
  code="$(ssh_run "curl -s --max-time 8 -o /dev/null -w '%{http_code}' '${HEALTH_URL}'" | tr -d '\r')"
  if [[ "$code" != "200" ]]; then warn "GET ${HEALTH_URL} -> ${code}"; return 1; fi
  ok "GET ${HEALTH_URL} -> 200"

  refs="$(ssh_run "curl -s --max-time 8 '${HEALTH_URL}' | grep -o 'assets/[^\"]*' | sort -u" | tr -d '\r' || true)"
  if [[ -z "$refs" ]]; then
    warn "index.html references no assets/ files -- is SHIRLEY_WEB_DIST pointing at the right dir?"
    return 1
  fi
  while IFS= read -r ref; do
    [[ -z "$ref" ]] && continue
    if ! ssh_run "test -f '${REMOTE_ROOT}/current/web/${ref}'" >/dev/null 2>&1; then
      missing="${missing} ${ref}"
    fi
  done <<< "$refs"
  if [[ -n "$missing" ]]; then warn "referenced assets missing on disk:${missing}"; return 1; fi
  ok "asset references resolve ($(printf '%s' "$refs" | tr '\n' ' '))"

  # 3) auth still enforced: an unauthenticated command must be rejected
  base="${HEALTH_URL%/}"
  code="$(ssh_run "curl -s --max-time 8 -o /dev/null -w '%{http_code}' -X POST '${base}/api/cmd/agent_list_projects'" | tr -d '\r')"
  case "$code" in
    401|403) ok "unauthenticated API call -> ${code} (auth enforced)" ;;
    200)     warn "unauthenticated API call -> 200: the server is exposed without a token!"; return 1 ;;
    *)       warn "unauthenticated API call -> ${code} (unexpected)"; return 1 ;;
  esac

  # 4) authenticated round trip, if a token is available. Look in the usual places: an explicit
  #    SHIRLEY_DEPLOY_TOKEN wins; otherwise try <root>/cloud.env, then the conventional system
  #    env file /etc/<service>/cloud.env. (No path is hard-coded to one host.)
  if [[ -z "$HEALTH_TOKEN" ]]; then
    HEALTH_TOKEN="$(ssh_run "for f in '${REMOTE_ROOT}/cloud.env' '/etc/${SERVICE}/cloud.env'; do [ -r \"\$f\" ] && sed -n 's/^SHIRLEY_SERVER_TOKEN=//p' \"\$f\" | head -n1 && break; done" | tr -d '\r' | head -n1 || true)"
  fi
  if [[ -n "$HEALTH_TOKEN" ]]; then
    code="$(ssh_run "curl -s --max-time 8 -o /dev/null -w '%{http_code}' -X POST -H 'Authorization: Bearer ${HEALTH_TOKEN}' '${base}/api/cmd/agent_list_projects'" | tr -d '\r')"
    [[ "$code" == "200" ]] && ok "authenticated API call -> 200" || { warn "authenticated API call -> ${code}"; return 1; }
  else
    warn "no SHIRLEY_SERVER_TOKEN found -- skipping the authenticated probe"
  fi

  return 0
}

# ---------------------------------------------------------------------------------------------
# housekeeping / rollback / listing
# ---------------------------------------------------------------------------------------------
prune_releases() {
  [[ -n "$KEEP_RELEASES" ]] || return 0
  [[ "$KEEP_RELEASES" =~ ^[0-9]+$ ]] || die "--keep needs a number"
  log "prune: keep ${KEEP_RELEASES} newest releases"
  ssh_run "set -e; cd '${REMOTE_ROOT}/releases'; cur=\$(basename \"\$(readlink -f '${REMOTE_ROOT}/current')\"); ls -1d */ 2>/dev/null | sed 's#/\$##' | sort -r | tail -n +$((KEEP_RELEASES + 1)) | while read -r r; do [ \"\$r\" = \"\$cur\" ] && continue; echo \"  removing \$r\"; find \"\$r\" -depth -delete; done"
}

rollback() {
  local target="$ROLLBACK_TO"
  if [[ -z "$target" ]]; then
    # "current - 1": the newest release that is not the one currently pointed at.
    target="$(ssh_run "cur=\$(basename \"\$(readlink -f '${REMOTE_ROOT}/current')\"); ls -1d '${REMOTE_ROOT}/releases/'*/ | sed 's#.*/releases/##;s#/\$##' | sort -r | grep -vx \"\$cur\" | head -n1" | tr -d '\r')"
  fi
  [[ -n "$target" ]] || die "no rollback target found"
  log "rollback -> ${target}"
  ssh_run "set -e; test -d '${REMOTE_ROOT}/releases/${target}' || { echo 'no such release: ${target}' >&2; exit 1; }; ln -sfn '${REMOTE_ROOT}/releases/${target}' '${REMOTE_ROOT}/current.tmp' && mv -Tf '${REMOTE_ROOT}/current.tmp' '${REMOTE_ROOT}/current'; systemctl restart '${SERVICE}'"
  health_check || die "rolled back to ${target} but it is unhealthy"
  ok "now serving ${target}"
}

list_releases() {
  log "releases on ${DEPLOY_HOST}"
  ssh_run "cur=\$(basename \"\$(readlink -f '${REMOTE_ROOT}/current' 2>/dev/null)\"); for d in \$(ls -1d '${REMOTE_ROOT}/releases/'*/ 2>/dev/null | sort -r); do n=\$(basename \"\$d\"); sz=\$(du -sh \"\$d\" | cut -f1); if [ \"\$n\" = \"\$cur\" ]; then printf '  * %s  %s  (active)\n' \"\$n\" \"\$sz\"; else printf '    %s  %s\n' \"\$n\" \"\$sz\"; fi; done"
}

# ---------------------------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------------------------
case "$COMMAND" in
  build)    build_web; build_binary ;;
  deploy)   deploy ;;
  rollback) rollback ;;
  releases) list_releases ;;
  *)        die "unknown command: $COMMAND" ;;
esac
