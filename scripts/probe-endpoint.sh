#!/usr/bin/env bash
#
# probe-endpoint.sh —— 探测一个「OpenAI / Anthropic 兼容」模型端点的能力边界。
#
# 用法:
#   scripts/probe-endpoint.sh <base_url> [api_key] [model...]
#
# 示例:
#   # 有鉴权，自动从 /v1/models 取前 6 个模型探测
#   scripts/probe-endpoint.sh https://api.example.com "$MY_KEY"
#
#   # 指定要探测的模型
#   scripts/probe-endpoint.sh https://anyrouter.top "$MY_KEY" \
#       claude-haiku-4-5-20251001 claude-sonnet-4-5-20250929
#
#   # 本地无鉴权端点
#   scripts/probe-endpoint.sh http://127.0.0.1:11434/v1
#
# 为什么这么测（方法论）:
#   1. 「端点活着吗」和「模型能用吗」是两件事 —— 先 GET /v1/models 拿清单，
#      但清单里的模型名**未必真能用**（中转站常见：列表很全，实际只有少数几个可用）。
#   2. 「哪个协议能用」必须用**同一个模型**分别打两种协议才能对比出来。
#      只打一个协议，无法区分「协议不支持」和「模型不支持」。
#   3. **状态码是主要信息，错误体是次要信息**。404 / 400 / 429 / 500 含义完全不同：
#        404 → 该协议不认这个模型（换协议或换模型）
#        400 → 参数缺东西（如 Anthropic 的 1M beta 头、thinking 的 budget_tokens）
#        429/503 → 要么参数非法、要么上游抖动 —— 用「A/B 对照 + 重试」区分
#        500 → 上游挂了（重试 / 换模型）
#   4. 最后一步才是「Shirley 实际会发的报文能过吗」—— 复制适配器的真实 body
#      （stream + thinking + tools），因为 curl 最小报文能过 ≠ 应用能跑。

set -uo pipefail

BASE="${1:-}"
KEY="${2:-}"
shift 2 2>/dev/null || true

if [[ -z "$BASE" ]]; then
  sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
fi
BASE="${BASE%/}"

# 本地端点绕开代理（否则 127.0.0.1 的探测会被 http_proxy 劫持成 502）；
# 远端端点走代理（沙箱环境常常必须）。bash 3.2 下避免用空数组展开。
USE_NOPROXY=0
case "$BASE" in
  *127.0.0.1*|*localhost*|*0.0.0.0*) USE_NOPROXY=1 ;;
esac

PY="${PYTHON:-python3}"

# do_curl <curl 参数...> —— 统一的 curl 入口（含代理策略）
do_curl() {
  if [[ "$USE_NOPROXY" -eq 1 ]]; then
    curl -s --noproxy '*' "$@"
  else
    curl -s "$@"
  fi
}

# probe <标签> <curl 参数...>
probe() {
  local label="$1"; shift
  local body code
  body=$(mktemp)
  code=$(do_curl -m 60 -o "$body" -w '%{http_code}' "$@" 2>/dev/null)
  printf '%-48s HTTP %-4s %s\n' "$label" "$code" "$(head -c 150 "$body" | LC_ALL=C tr -d '\n')"
  rm -f "$body"
}

echo "=== 1. 端点存活 & 模型清单  GET $BASE/v1/models ==="
if [[ -n "$KEY" ]]; then
  probe "GET /v1/models" "$BASE/v1/models" -H "Authorization: Bearer $KEY"
else
  probe "GET /v1/models" "$BASE/v1/models"
fi

MODELS=("$@")
if [[ ${#MODELS[@]} -eq 0 ]]; then
  echo
  echo "(未指定模型，自动从 /v1/models 取前 6 个)"
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
  echo "拿不到模型清单，无法继续。可手动传入模型名。"
  exit 1
fi

echo
echo "=== 2. 协议 × 模型 交叉探测 ==="
for m in "${MODELS[@]}"; do
  probe "  $m | chat_completions" "$BASE/v1/chat/completions" \
    -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
    -d "{\"model\":\"$m\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"max_tokens\":16}"

  probe "  $m | anthropic_messages" "$BASE/v1/messages" \
    -H "x-api-key: $KEY" -H 'anthropic-version: 2023-06-01' -H 'Content-Type: application/json' \
    -d "{\"model\":\"$m\",\"max_tokens\":16,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
done

echo
echo "=== 3. 读法 ==="
cat <<'TIPS'
  200            该组合可用
  404 「不支持所选模型」  该协议不认这个模型 → 换协议或换模型
  400 「请启用 1m 上下文」模型可用，但缺 anthropic-beta 头 → 需要能加自定义 header 的客户端
  429 / 503      先排除参数问题（A/B 对照），再重试 2~3 次判断是不是上游抖动
  500            上游异常 → 重试或换模型
  401 / 403      鉴权方式不对：chat 用 Authorization: Bearer，anthropic 用 x-api-key
TIPS
