#!/usr/bin/env bash
# Previous-release compatibility (#761): the previous release's `capsule` against the server built
# from the commit under test, and the commit's `capsule` against the previous release's
# `capsule-server`, each driven through one fixed flow.
#
#   scripts/prev-release-compat.sh NEW_BIN_DIR [--prev-dir DIR --prev-protocol YYYY-MM-DD]
#   scripts/prev-release-compat.sh --resolve-only
#
# `--resolve-only` resolves the previous release as below and prints its tag, or nothing when no
# release older than HEAD exists, and exits 0 either way; a failed listing still exits 1. CI runs
# it first, so the flow's job is skipped, not red, until the first release exists, and runs on
# its own from the first release on.
#
# NEW_BIN_DIR holds `capsule` and `capsule-server` built from the commit under test. Without
# `--prev-dir`, the previous release is resolved and downloaded: the newest published, non-draft,
# non-prerelease GitHub Release whose tag is an ancestor of HEAD and does not point at HEAD, and its
# `capsule-v<version>-x86_64-unknown-linux-gnu.tar.gz` archive (the layout release.yml publishes).
# Its protocol version is read from `capsule-core/src/crypto/primitives.rs` at that tag. With
# `--prev-dir`, DIR stands in for the release (a local run, or a self-test against the new build)
# and `--prev-protocol` names its protocol version. Both servers inherit PROTOCOL_MIN and
# PROTOCOL_MAX, so a self-test can move the window off the client's version to drive the refused
# arm: `PROTOCOL_MIN=2027-01-01 PROTOCOL_MAX=2027-12-31 scripts/prev-release-compat.sh target/debug
# --prev-dir target/debug --prev-protocol 2026-05-31`.
#
# There is no vacuous pass: run without `--resolve-only`, no release older than HEAD is a failure,
# loudly, as is a release that lacks the archive or either binary.
#
# The flow, per direction: register, login, `library init`, `auth escrow`, `auth recover` (the one
# CLI path that lists a device in the account's directory, which push requires), import one
# fixture, `push`, `status`, `restore`. Which outcome is required is decided before the flow runs, from the server's
# advertised window (`GET /v1/version`, the `X-Capsule-Protocol-Min`/`-Max` headers) and the
# client's protocol version:
#
# - inside the window, the push must reach verified custody for the one asset and the restore
#   must return the fixture byte for byte;
# - outside it, every write must be refused with `error.protocol.version_unsupported` (the `426`
#   gate), and nothing may be lost: the fixture is untouched and the local library still holds the
#   imported asset (versioning invariants 3 and 5).
#
# Every wait is bounded: the server's readiness probe retries for 60 s, and every CLI run is cut
# at CLI_TIMEOUT seconds. Each process's output is kept under the work directory, which is
# printed, and every failure quotes the output of the step that failed.
set -euo pipefail

readonly TARGET=x86_64-unknown-linux-gnu
readonly PRIMITIVES=capsule-core/src/crypto/primitives.rs
readonly REFUSAL_CODE=error.protocol.version_unsupported
readonly PASSWORD='prev-release-compat password 1'
readonly PASSPHRASE='prev-release-compat passphrase 1'
# The published example key from capsule-server/.env.example, as `mise run serve-memory` uses:
# the server listens on loopback only and lives for one flow.
readonly JWT_KEY=MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF
CLI_TIMEOUT=${CLI_TIMEOUT:-300}

log() { printf '[prev-release-compat] %s\n' "$*" >&2; }
die() {
	printf '[prev-release-compat] FAIL: %s\n' "$*" >&2
	exit 1
}

usage() {
	sed -n "2,26p" "$0" >&2
	exit 2
}

# The `PROTOCOL_VERSION` date a copy of primitives.rs declares, read from stdin.
protocol_of() {
	sed -n 's/^pub const PROTOCOL_VERSION: &str = "\([0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}\)";$/\1/p' | head -n 1
}

# ── Arguments ────────────────────────────────────────────────────────────────
new_dir=''
prev_dir=''
prev_protocol=''
resolve_only=''
while [ $# -gt 0 ]; do
	case "$1" in
	--resolve-only)
		resolve_only=1
		shift
		;;
	--prev-dir)
		prev_dir=${2:?--prev-dir needs a directory}
		shift 2
		;;
	--prev-protocol)
		prev_protocol=${2:?--prev-protocol needs a date}
		shift 2
		;;
	-h | --help) usage ;;
	-*) die "unknown option $1" ;;
	*)
		[ -z "$new_dir" ] || die "one NEW_BIN_DIR only"
		new_dir=$1
		shift
		;;
	esac
done
if [ -n "$resolve_only" ]; then
	[ -z "$new_dir$prev_dir$prev_protocol" ] || die "--resolve-only takes no other argument"
else
	[ -n "$new_dir" ] || usage
fi
if [ -n "$prev_dir" ] && [ -z "$prev_protocol" ]; then
	die "--prev-dir needs --prev-protocol"
fi

# ── Resolve the previous release ─────────────────────────────────────────────
# `resolve_release TAGS`: the first of TAGS (newest first, one per line) that is an ancestor of
# HEAD and does not point at it.
resolve_release() {
	local tags=$1 head tag tag_commit
	head=$(git rev-parse HEAD)
	while IFS= read -r tag; do
		[ -n "$tag" ] || continue
		git fetch --quiet --no-tags origin "refs/tags/$tag:refs/tags/$tag" 2>/dev/null || true
		tag_commit=$(git rev-parse --verify --quiet "refs/tags/$tag^{commit}") || {
			log "release $tag: its tag is not fetchable; skipped"
			continue
		}
		if [ "$tag_commit" = "$head" ]; then
			log "release $tag: is the commit under test; skipped"
			continue
		fi
		if git merge-base --is-ancestor "$tag_commit" "$head"; then
			printf '%s\n' "$tag"
			return 0
		fi
		log "release $tag: not an ancestor of HEAD; skipped"
	done <<<"$tags"
	return 1
}

# `list_release_tags`: the published, non-draft, non-prerelease release tags, newest first:
# `gh release list` sorts by creation date, the order releases are cut. A listing that fails is a
# failure of its own, never "no release".
list_release_tags() {
	command -v gh >/dev/null || die "gh is required to resolve the previous release"
	gh release list --exclude-drafts --exclude-pre-releases --limit 100 \
		--json tagName --jq '.[].tagName' || die "listing the repository's releases failed"
}

if [ -n "$resolve_only" ]; then
	release_tags=$(list_release_tags)
	if prev_tag=$(resolve_release "$release_tags"); then
		log "previous release: $prev_tag"
		printf '%s\n' "$prev_tag"
	else
		log "no published release is older than $(git rev-parse --short HEAD); nothing to check against yet (#761)"
	fi
	exit 0
fi

repo_root=$(git rev-parse --show-toplevel)
# The interpreter itself, not a version-manager shim: the escrow helper runs under the flow's
# isolated HOME, where a shim finds none of its configuration.
python=$(python3 -I -c 'import sys; print(sys.executable)')
new_protocol=$(protocol_of <"$repo_root/$PRIMITIVES")
[ -n "$new_protocol" ] || die "no PROTOCOL_VERSION in $PRIMITIVES"

work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/prev-release-compat.XXXXXX")
log "work directory: $work"

pids=()
cleanup() {
	local pid
	for pid in "${pids[@]}"; do
		kill "$pid" 2>/dev/null || true
		wait "$pid" 2>/dev/null || true
	done
}
trap cleanup EXIT

if [ -z "$prev_dir" ]; then
	release_tags=$(list_release_tags)
	if ! prev_tag=$(resolve_release "$release_tags"); then
		die "no published release is older than $(git rev-parse --short HEAD). This flow cannot pass before the first release exists; CI resolves the release with --resolve-only first and skips the flow until one does (#761)."
	fi
	version=${prev_tag#v}
	asset="capsule-v${version}-${TARGET}.tar.gz"
	log "previous release: $prev_tag (asset $asset)"
	mkdir -p "$work/download" "$work/release"
	gh release download "$prev_tag" --pattern "$asset" --dir "$work/download" ||
		die "release $prev_tag has no asset $asset"
	tar -xzf "$work/download/$asset" -C "$work/release"
	prev_dir="$work/release/capsule-v${version}-${TARGET}"
	prev_protocol=$(git show "refs/tags/$prev_tag:$PRIMITIVES" | protocol_of)
	[ -n "$prev_protocol" ] || die "no PROTOCOL_VERSION in $PRIMITIVES at $prev_tag"
	prev_catalog="$work/release-en.json"
	git show "refs/tags/$prev_tag:locales/en.json" >"$prev_catalog" ||
		die "no locales/en.json at $prev_tag"
else
	# A stand-in for the release speaks this tree's catalog.
	prev_catalog="$repo_root/locales/en.json"
fi

for bin in "$new_dir/capsule" "$new_dir/capsule-server" "$prev_dir/capsule" "$prev_dir/capsule-server"; do
	[ -x "$bin" ] || die "$bin is not an executable"
done
log "new protocol: $new_protocol; previous protocol: $prev_protocol"

# ── The fixture ──────────────────────────────────────────────────────────────
# A 16x16 PNG, generated so no binary sits in the repository and every run imports the same bytes.
fixture_dir="$work/fixture"
mkdir -p "$fixture_dir"
python3 -I - "$fixture_dir/compat.png" <<'PY'
import struct, sys, zlib

def chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

width = height = 16
rows = b"".join(
    b"\0" + b"".join(bytes((x * 16, y * 16, (x ^ y) * 16)) for x in range(width))
    for y in range(height)
)
png = (
    b"\x89PNG\r\n\x1a\n"
    + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(rows, 9))
    + chunk(b"IEND", b"")
)
with open(sys.argv[1], "wb") as out:
    out.write(png)
PY
fixture="$fixture_dir/compat.png"
fixture_sum=$(sha256sum "$fixture" | cut -d' ' -f1)

# ── One flow ─────────────────────────────────────────────────────────────────
# `cli_run DIR NAME STDIN ARGS...`: run the flow's CLI with its isolated home, writing
# DIR/NAME.{stdout,stderr,status}. Returns the CLI's exit status.
cli_run() {
	local dir=$1 name=$2 input=$3
	shift 3
	local status=0
	printf '%s' "$input" | env -u CAPSULE_AUTH_ENDPOINT -u CAPSULE_SYNC_ENDPOINT \
		-u CAPSULE_UPLOAD_ENDPOINT -u CAPSULE_ALBUMS_ENDPOINT -u CAPSULE_PROTOCOL \
		CAPSULE_ENDPOINT="$endpoint" HOME="$dir/home" XDG_CONFIG_HOME="$dir/home/config" \
		XDG_DATA_HOME="$dir/home/data" XDG_CACHE_HOME="$dir/home/cache" NO_COLOR=1 RUST_LOG=off \
		timeout "$CLI_TIMEOUT" "$cli" "$@" >"$dir/$name.stdout" 2>"$dir/$name.stderr" || status=$?
	printf '%s\n' "$status" >"$dir/$name.status"
	log "  capsule $* -> exit $status"
	return "$status"
}

# `cli_escrow DIR LIBRARY`: `capsule auth escrow`, which binds the library to the signed-in
# account (push refuses an unbound library's uploads) and confirms the minted recovery secret on a
# terminal prompt. A pseudo-terminal answers it: the passphrase goes in as the `--passphrase-stdin`
# line, and the 64-hex secret the command prints is typed back once its prompt appears. Writes
# DIR/escrow.{stdout,stderr,status} like `cli_run`; the run is cut at CLI_TIMEOUT seconds.
cli_escrow() {
	local dir=$1 library=$2
	local status=0
	: >"$dir/escrow.stderr"
	env -u CAPSULE_AUTH_ENDPOINT -u CAPSULE_SYNC_ENDPOINT -u CAPSULE_UPLOAD_ENDPOINT \
		-u CAPSULE_ALBUMS_ENDPOINT -u CAPSULE_PROTOCOL \
		CAPSULE_ENDPOINT="$endpoint" HOME="$dir/home" XDG_CONFIG_HOME="$dir/home/config" \
		XDG_DATA_HOME="$dir/home/data" XDG_CACHE_HOME="$dir/home/cache" NO_COLOR=1 RUST_LOG=off \
		COMPAT_PASSPHRASE="$PASSPHRASE" \
		"$python" -I - "$CLI_TIMEOUT" "$cli" auth escrow --library "$library" --passphrase-stdin \
		>"$dir/escrow.stdout" 2>"$dir/escrow.stderr" <<'PY' || status=$?
import os, pty, re, select, sys, time

timeout, argv = float(sys.argv[1]), sys.argv[2:]
pid, fd = pty.fork()
if pid == 0:
    os.execvp(argv[0], argv)
os.write(fd, os.environ["COMPAT_PASSPHRASE"].encode() + b"\n")
deadline, seen, answered = time.monotonic() + timeout, b"", False
while True:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        os.kill(pid, 9)
        sys.stderr.write(f"auth escrow did not exit within {timeout:.0f}s\n")
        break
    ready, _, _ = select.select([fd], [], [], min(remaining, 1.0))
    if not ready:
        continue
    try:
        chunk = os.read(fd, 4096)
    except OSError:
        break
    if not chunk:
        break
    sys.stdout.buffer.write(chunk)
    sys.stdout.flush()
    seen += chunk
    secret = re.search(rb"\b([0-9a-f]{64})\b", seen)
    if not answered and secret and b"Type the recovery secret back" in seen:
        os.write(fd, secret.group(1) + b"\r")
        answered = True
_, raw = os.waitpid(pid, 0)
sys.exit(os.waitstatus_to_exitcode(raw) if os.WIFEXITED(raw) else 1)
PY
	printf '%s\n' "$status" >"$dir/escrow.status"
	log "  capsule auth escrow --library $library --passphrase-stdin -> exit $status"
	return "$status"
}

# Print a step's output and fail the flow.
step_failed() {
	local dir=$1 name=$2 why=$3
	{
		printf -- '--- %s stdout\n' "$name"
		cat "$dir/$name.stdout"
		printf -- '--- %s stderr\n' "$name"
		cat "$dir/$name.stderr"
		printf -- '--- server log\n'
		tail -n 50 "$dir/server.log"
	} >&2
	die "$label: $name: $why"
}

# Whether a refused step carried the protocol gate's refusal: its stable code (a `--json`
# document), or the English catalog message the CLI renders that code as (a person's line).
refused_by_gate() {
	grep -qF -e "$REFUSAL_CODE" -e "$refusal_message" "$1/$2.stdout" "$1/$2.stderr"
}

# The English message a client renders the refusal code as, from that client's catalog on stdin.
refusal_message_of() {
	jq -er --arg code "$REFUSAL_CODE" '.[$code].message'
}

# `run_flow LABEL CLI SERVER CLIENT_PROTOCOL PORT CLIENT_CATALOG`
run_flow() {
	label=$1 cli=$2
	local server=$3 client_protocol=$4 port=$5
	refusal_message=$(refusal_message_of <"$6") || die "$label: no $REFUSAL_CODE message in $6"
	local dir="$work/$label"
	mkdir -p "$dir/home" "$dir/blobs" "$dir/out"
	log "── $label: capsule ($cli) against capsule-server ($server)"

	endpoint="http://127.0.0.1:$port"
	env -u DATABASE_URL -u VALKEY_URL -u SERVER_HOST -u SERVER_PORT -u CAPSULE_PROFILE \
		JWT_ED25519_DER="$JWT_KEY" RUST_LOG="${CAPSULE_COMPAT_SERVER_LOG:-warn}" \
		"$server" serve --memory --listen "127.0.0.1:$port" --blob-root "$dir/blobs" \
		>"$dir/server.log" 2>&1 &
	local server_pid=$!
	pids+=("$server_pid")
	if ! curl --silent --show-error --fail --retry 60 --retry-delay 1 --retry-connrefused \
		--dump-header "$dir/version.headers" --output "$dir/version.json" "$endpoint/v1/version"; then
		tail -n 50 "$dir/server.log" >&2
		die "$label: the server did not answer /v1/version within 60 s"
	fi
	local window_min window_max expected
	window_min=$(sed -n 's/^[Xx]-[Cc]apsule-[Pp]rotocol-[Mm]in: *\([0-9-]*\).*/\1/p' "$dir/version.headers")
	window_max=$(sed -n 's/^[Xx]-[Cc]apsule-[Pp]rotocol-[Mm]ax: *\([0-9-]*\).*/\1/p' "$dir/version.headers")
	[ -n "$window_min" ] && [ -n "$window_max" ] || die "$label: /v1/version advertised no protocol window"
	# ISO dates order as strings.
	if [[ ! "$client_protocol" < "$window_min" ]] && [[ ! "$client_protocol" > "$window_max" ]]; then
		expected=accepted
	else
		expected=refused
	fi
	log "  server window $window_min..$window_max, client $client_protocol: expect $expected"

	local email="compat-$label-$RANDOM$RANDOM@example.test"
	local library="$dir/library"

	if [ "$expected" = refused ]; then
		# Every write is refused by the gate. Registration is the flow's first write.
		if cli_run "$dir" register "$PASSWORD"$'\n' auth register --email "$email" --password-stdin; then
			step_failed "$dir" register "a client outside the window registered"
		fi
		refused_by_gate "$dir" register || step_failed "$dir" register "refused, but not with $REFUSAL_CODE"
		if cli_run "$dir" login "$PASSWORD"$'\n' auth login --email "$email" --password-stdin; then
			step_failed "$dir" login "a client outside the window signed in"
		fi
		refused_by_gate "$dir" login || step_failed "$dir" login "refused, but not with $REFUSAL_CODE"
		# The local half still works, and the refused push loses nothing: the imported asset
		# still opens from the library afterwards.
		cli_run "$dir" init '' library init "$library" --name compat ||
			step_failed "$dir" init "library init failed"
		cli_run "$dir" import "$PASSPHRASE"$'\n' import "$fixture_dir" --library "$library" \
			--passphrase-stdin --json || step_failed "$dir" import "import failed"
		local asset
		asset=$(jq -er '.outcomes[0].asset_id' "$dir/import.stdout") ||
			step_failed "$dir" import "the report names no imported asset"
		if cli_run "$dir" push "$PASSPHRASE"$'\n' push --library "$library" --passphrase-stdin --json; then
			step_failed "$dir" push "a client outside the window pushed"
		fi
		cli_run "$dir" show "$PASSPHRASE"$'\n' show "$asset" --library "$library" --passphrase-stdin ||
			step_failed "$dir" show "the imported asset no longer opens after the refused push"
		[ "$(sha256sum "$fixture" | cut -d' ' -f1)" = "$fixture_sum" ] ||
			die "$label: the fixture changed"
		log "  $label: every write refused with $REFUSAL_CODE; nothing lost"
	else
		cli_run "$dir" register "$PASSWORD"$'\n' auth register --email "$email" --password-stdin ||
			step_failed "$dir" register "register failed"
		cli_run "$dir" login "$PASSWORD"$'\n' auth login --email "$email" --password-stdin ||
			step_failed "$dir" login "login failed"
		# A library created by `library init` is bound to the account by `auth escrow`, but only
		# `auth recover` publishes a device into the account's directory, and push refuses an
		# upload from a device the directory does not list. So the flow escrows a first library
		# and recovers the account into the one it imports into: both are CLI commands a release
		# ships, and both cross the wire under test.
		local escrowed="$dir/escrowed-library"
		cli_run "$dir" init '' library init "$escrowed" --name compat ||
			step_failed "$dir" init "library init failed"
		cli_escrow "$dir" "$escrowed" || step_failed "$dir" escrow "auth escrow failed"
		local secret
		secret=$(grep -oE '[0-9a-f]{64}' "$dir/escrow.stdout" | head -n 1)
		[ -n "$secret" ] || step_failed "$dir" escrow "printed no recovery secret"
		cli_run "$dir" recover "$secret"$'\n'"$PASSPHRASE"$'\n' auth recover --library "$library" \
			--recovery-secret-stdin --passphrase-stdin || step_failed "$dir" recover "auth recover failed"
		cli_run "$dir" import "$PASSPHRASE"$'\n' import "$fixture_dir" --library "$library" \
			--passphrase-stdin --json || step_failed "$dir" import "import failed"
		cli_run "$dir" push "$PASSPHRASE"$'\n' push --library "$library" --passphrase-stdin --json ||
			step_failed "$dir" push "push failed"
		local verified failed
		verified=$(jq -r '.verified_assets' "$dir/push.stdout") || step_failed "$dir" push "push printed no JSON"
		failed=$(jq -r '.failed_assets' "$dir/push.stdout")
		[ "$verified" = 1 ] && [ "$failed" = 0 ] ||
			step_failed "$dir" push "expected 1 verified and 0 failed assets, got $verified verified and $failed failed"
		cli_run "$dir" status '' status || step_failed "$dir" status "status failed"
		cli_run "$dir" restore "$PASSPHRASE"$'\n' restore --library "$library" --out "$dir/out" \
			--passphrase-stdin --json || step_failed "$dir" restore "restore failed"
		[ "$(jq -r '.failed' "$dir/restore.stdout")" = 0 ] || step_failed "$dir" restore "an asset failed to restore"
		local restored
		restored=$(find "$dir/out" -type f -name '*.png' | head -n 1)
		[ -n "$restored" ] || step_failed "$dir" restore "no PNG under the restore directory"
		cmp -s "$fixture" "$restored" || step_failed "$dir" restore "$restored differs from the fixture"
		log "  $label: verified custody, and the restore matches the fixture byte for byte"
	fi

	kill "$server_pid" 2>/dev/null || true
	wait "$server_pid" 2>/dev/null || true
}

run_flow old-cli-new-server "$prev_dir/capsule" "$new_dir/capsule-server" "$prev_protocol" 3901 \
	"$prev_catalog"
run_flow new-cli-old-server "$new_dir/capsule" "$prev_dir/capsule-server" "$new_protocol" 3902 \
	"$repo_root/locales/en.json"
log "both directions passed"
