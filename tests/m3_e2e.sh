#!/usr/bin/env bash
# M3 端到端：验证 IPC 管道（面板↔服务）与"面板只是可选"的机制。
#
# 覆盖：
#   1) 无凭据时 status.configured=false
#   2) 通过 IPC 写凭据 → 服务自己加密落盘 → 状态变已配置
#   3) 服务自动登录成功（面板不参与）
#   4) pause / resume / reconnect / get_logs / get_diag 全部走通
#   5) 没有任何面板连接时，服务仍持续 tick（"不需要启动面板"）
set -u
# 测试用独立管道名：避免连到本机已安装的正式服务
export CAMPUSNET_PIPE="cn-test-m3"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT_W="$(cygpath -m "$ROOT" 2>/dev/null || echo "$ROOT")"
EXE="$ROOT/target/release/BJUTNetSvc.exe"
DATA="$ROOT/_testdata-m3e"
DATA_W="$(cygpath -m "$DATA" 2>/dev/null || echo "$DATA")"
LOG="/tmp/m3-e2e.log"
BASE="http://127.0.0.1:18080"
FAIL=0
SVC_PID=""
MOCK_PID=""

cleanup() {
  [ -n "$SVC_PID" ] && { kill "$SVC_PID" 2>/dev/null || taskkill //F //PID "$SVC_PID" >/dev/null 2>&1; }
  [ -n "$MOCK_PID" ] && { kill "$MOCK_PID" 2>/dev/null || taskkill //F //PID "$MOCK_PID" >/dev/null 2>&1; }
}
trap cleanup EXIT

check() {
  if [ "$2" = "$3" ]; then echo "  ✓ $1"; else echo "  ✗ $1（期望 [$2]，实际 [$3]）"; FAIL=1; fi
}

rpc() { "$EXE" --rpc "$1" 2>/dev/null; }

# 从 status 响应里取字段
st_field() {
  rpc '{"cmd":"status"}' | python -c "import json,sys
try:
    d=json.load(sys.stdin)
    print(d.get('data',{}).get('$1',''))
except Exception:
    print('<err>')"
}

wait_log() { # 模式 次数 超时
  local pat="$1" need="$2" timeout="$3" n
  for _ in $(seq 1 "$timeout"); do
    n=$(grep -c -- "$pat" "$LOG" 2>/dev/null || true)
    [ "${n:-0}" -ge "$need" ] && return 0
    sleep 1
  done
  return 1
}

echo "== 准备 =="
rm -rf "$DATA" "$LOG"
mkdir -p "$DATA"
cat > "$DATA/config.json" <<'JSON'
{
  "version": 1,
  "account": "",
  "accountSuffixFallback": "@campus",
  "checkIntervalSec": 5,
  "offlineIntervalSec": 2,
  "probeTimeoutSec": 2,
  "probeUrls": ["http://127.0.0.1:18080/204"],
  "backoff": [1, 1, 2],
  "maxPasswordFails": 3,
  "pauseMinutes": 1,
  "logLevel": "DEBUG",
  "dorm": { "base": "http://127.0.0.1:18080", "loadConfig": true, "fallbackSuffix": "@campus" },
  "public": { "base": "http://127.0.0.1:9", "refererPath": "/a79.htm", "preferStandardPort": true }
}
JSON

python "$ROOT_W/tests/mock_portal.py" >/tmp/m3-mock.log 2>&1 &
MOCK_PID=$!
sleep 1
curl -sf "$BASE/__state" >/dev/null || { echo "✗ 假 AC 没起来"; exit 1; }

echo "== 启动服务（--console，且**不启动面板**） =="
CAMPUSNET_DATA="$DATA_W" "$EXE" --console --debug >"$LOG" 2>&1 &
SVC_PID=$!
sleep 3

echo "== 场景 1：无凭据 =="
check "未配置时 configured=false" "False" "$(st_field configured)"
check "未配置时 state=未配置" "未配置" "$(st_field state)"

echo "== 场景 2：走 IPC 写凭据（服务身份落盘） =="
OUT=$(rpc '{"cmd":"set_credentials","params":{"account":"20250000","password":"secret-test-pw"}}')
check "set_credentials ok" "True" "$(echo "$OUT" | python -c "import json,sys;print(json.load(sys.stdin)['ok'])")"
sleep 1
check "写完后 configured=true" "True" "$(st_field configured)"
check "账号按打码返回" "250****23" "$(st_field account)"
[ -f "$DATA/secrets.dat" ] && check "secrets.dat 已由服务写出" "1" "1" || check "secrets.dat 已由服务写出" "1" "0"

echo "== 场景 3：服务自动登录（面板全程未运行） =="
if wait_log "登录成功\[" 1 25; then check "自动登录成功" "1" "1"; else check "自动登录成功" "1" "0"; fi
sleep 1
check "登录后 online=true" "True" "$(st_field online)"
check "登录后 state=在线" "在线" "$(st_field state)"

echo "== 场景 4：pause / resume / reconnect =="
rpc '{"cmd":"pause","params":{"minutes":1}}' >/dev/null
sleep 1
check "pause 后 state=已暂停" "已暂停" "$(st_field state)"
rpc '{"cmd":"resume"}' >/dev/null
sleep 1
STATE=$(st_field state)
[ "$STATE" != "已暂停" ] && check "resume 后离开暂停" "1" "1" || check "resume 后离开暂停" "1" "0"
check "reconnect 返回 ok" "True" "$(rpc '{"cmd":"reconnect"}' | python -c "import json,sys;print(json.load(sys.stdin)['ok'])")"

echo "== 场景 5：日志 / 诊断 =="
N=$(rpc '{"cmd":"get_logs","params":{"tail":50}}' | python -c "import json,sys;print(len(json.load(sys.stdin)['data']['lines']))")
[ "${N:-0}" -ge 5 ] && check "get_logs 返回日志行($N)" "1" "1" || check "get_logs 返回日志行($N)" "1" "0"
# 注意：中文不能写进 python -c 的参数（MSYS 会按 ANSI 转换弄坏），所以这里只比对 ASCII 前缀
D=$(rpc '{"cmd":"get_diag"}' | python -c "import json,sys;t=json.load(sys.stdin)['data']['text'];print('yes' if t.startswith('===== BJUTNet') and len(t)>200 else 'no')")
check "get_diag 返回诊断文本" "yes" "$D"

echo "== 场景 6：没有面板连接时服务仍持续工作 =="
T1=$(st_field ticks); T2=$T1
for _ in $(seq 1 15); do
  sleep 1
  T2=$(st_field ticks)
  [ "${T2:-0}" -gt "${T1:-0}" ] && break
done
[ "${T2:-0}" -gt "${T1:-0}" ] && check "无面板时 tick 继续增长（$T1→$T2）" "1" "1" || check "无面板时 tick 继续增长（$T1→$T2）" "1" "0"

echo
echo "===== 关键日志 ====="
grep -E "IPC 管道|面板已连接|凭据已更新|面板指令|开始登录|登录成功|暂停" "$LOG" | tail -20
echo
if [ "$FAIL" = "0" ]; then echo "===== M3 E2E 全部通过 ====="; exit 0; else echo "===== M3 E2E 有失败项 ====="; exit 1; fi
