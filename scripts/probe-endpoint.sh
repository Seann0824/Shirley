#!/usr/bin/env bash
#
# probe-endpoint.sh -- probe the capability boundary of an "OpenAI / Anthropic compatible" model endpoint.
#
# Usage:
#   scripts/probe-endpoint.sh <base_url> [api_key] [model...]
#
# Examples:
#   # With auth, auto-fetch the first 6 models from /v1/models and probe them
#   scripts/probe-endpoint.sh https://api.example.com "$MY_KEY"
#
#   # Probe the specified models
#   scripts/probe-endpoint.sh https://anyrouter.top "$MY_KEY" \
#       claude-haiku-4-5-20251001 claude-sonnet-4-5-20250929
#
#   # Local endpoint without auth
#   scripts/probe-endpoint.sh http://127.0.0.1:11434/v1
#
# Why test this way (methodology):
#   1. "Is the endpoint alive" and "is the model usable" are two different things -- first GET /v1/models to get the list,
#      but a model name in the list is **not necessarily actually usable** (common with relays: the list is complete, but only a few actually work).
#   2. "Which protocol works" must be compared by hitting both protocols with the **same model**.
#      Hitting only one protocol cannot distinguish "protocol unsupported" from "model unsupported".
#   3. **The status code is the primary information, the error body is secondary**. 404 / 400 / 429 / 500 mean completely different things:
#        404 -> this protocol does not recognize this model (switch protocol or model)
#        400 -> a parameter is missing (e.g. Anthropic's 1M beta header, thinking's budget_tokens)
#        429/503 -> either the parameters are invalid, or upstream is flaky -- distinguish with "A/B comparison + retry"
#        500 -> upstream is down (retry / switch model)
#   4. Only the last step is "does the actual payload Shirley sends get through" -- copy the adapter's real body
#      (stream + thinking + tools), because a minimal curl payload getting through != the app being able to run.

set -uo pipefail

BASE="${1:-}"
KEY="${2:-}"
shift 2 2>/dev/null || true

if [[ -z "$BASE" ]]; then
  sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
fi
BASE="${BASE%/}"

# Local endpoints bypass the proxy (otherwise probing 127.0.0.1 gets hijacked by http_proxy into a 502);
# remote endpoints go through the proxy (often required in sandboxed environments). Under bash 3.2 avoid expanding empty arrays.
USE_NOPROXY=0
case "$BASE" in
  *127.0.0.1*|*localhost*|*0.0.0.0*) USE_NOPROXY=1 ;;
esac

PY="${PYTHON:-python3}"

# do_curl <curl args...> -- unified curl entry point (with proxy policy)
do_curl() {
  if [[ "$USE_NOPROXY" -eq 1 ]]; then
    curl -s --noproxy '*' "$@"
  else
    curl -s "$@"
  fi
}

# probe <label> <curl args...>
probe() {
  local label="$1"; shift
  local body code
  body=$(mktemp)
  code=$(do_curl -m 60 -o "$body" -w '%{http_code}' "$@" 2>/dev/null)
  printf '%-48s HTTP %-4s %s\n' "$label" "$code" "$(head -c 150 "$body" | LC_ALL=C tr -d '\n')"
  rm -f "$body"
}

echo "=== 1. Endpoint alive & model list  GET $BASE/v1/models ==="
if [[ -n "$KEY" ]]; then
  probe "GET /v1/models" "$BASE/v1/models" -H "Authorization: Bearer $KEY"
else
  probe "GET /v1/models" "$BASE/v1/models"
fi

MODELS=("$@")
if [[ ${#MODELS[@]} -eq 0 ]]; then
  echo
  echo "(no model specified, auto-fetching the first 6 from /v1/models)"
  if [[ -n "$KEY" ]]; then
    LIST=$(do_curl -m 60 "$BASE/v1/models" -H "Authorization: Bearer $KEY")
  else
    LIST=$(do_curl -m 60 "$BASE/v1/models")
  fi
  while IFS= read -r m; do
    [[ -n "$m" ]] && MODELS+=("$m")
  done < <(printf '%s' "$LIST" | "$PY" -c '
import sys, json
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
ids = [m.get("id") for m in d.get("data", []) if m.get("id")]
for i in ids[:6]:
    print(i)
' 2>/dev/null)
fi

if [[ ${#MODELS[@]} -eq 0 ]]; then
  echo "Could not fetch the model list, cannot continue. You can pass model names manually."
  exit 1
fi

echo
echo "=== 2. Protocol x model cross probe ==="
for m in "${MODELS[@]}"; do
  probe "  $m | chat_completions" "$BASE/v1/chat/completions" \
    -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
    -d "{\"model\":\"$m\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"max_tokens\":16}"

  probe "  $m | anthropic_messages" "$BASE/v1/messages" \
    -H "x-api-key: $KEY" -H 'anthropic-version: 2023-06-01' -H 'Content-Type: application/json' \
    -d "{\"model\":\"$m\",\"max_tokens\":16,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
done

echo
echo "=== 3. How to read ==="
cat <<'TIPS'
  200            this combination works
  404 "model not supported"  this protocol does not recognize this model -> switch protocol or model
  400 "please enable 1m context"  model works, but the anthropic-beta header is missing -> needs a client that can add custom headers
  429 / 503      first rule out parameter issues (A/B comparison), then retry 2-3 times to tell whether upstream is flaky
  500            upstream error -> retry or switch model
  401 / 403      wrong auth method: chat uses Authorization: Bearer, anthropic uses x-api-key
TIPS
