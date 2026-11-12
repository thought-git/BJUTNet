#!/usr/bin/env bash
# M2 端到端验证：自动登录 → 掉线自愈 → 密码错暂停
#
# 全程用本地"假 AC"（tests/mock_portal.py），不碰真实校园网、不影响用户上网。
# 覆盖的分支：
#   1) 离线 → 判别宿舍 → 登录成功 → 复核出网
#   2) 被注销（/__drop）→ 自动重新登录
#   3) 密码错 → 连续 N 次后暂停并提示
#   4) 所有登录请求的账号都带 @campus（旧方案就是丢了这个后缀）
set -u
export CAMPUSNET_PIPE="cn-test-m2"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# 原生 exe / python 需要 Windows 风格路径（F:/...），否则 MSYS 的 /f/... 会被解释错
ROOT_W="$(cygpath -m "$ROOT" 2>/dev/null || echo "$ROOT")"
EXE="$ROOT/target/release/BJUTNetSvc.exe"
DATA="$ROOT/_testdata-m2"
DATA_W="$(cygpath -m "$DATA" 2>/dev/null || echo "$DATA")"
LOG="/tmp/m2-e2e.log"
BASE="http://127.0.0.1:18080"
FAIL=0
SVC_PID=""
MOCK_PID=""

cleanup() {
  [ -n "$SVC_PID" ] && { kill "$SVC_PID" 2>/dev/null || taskkill //F //PID "$SVC_PID" >/dev/null 2>&1; }
  [ -n "$MOCK_PID" ] && { kill "$MOCK_PID" 2>/dev/null || taskkill //F //PID "$MOCK_PID" >/dev/null 2>&1; }
}
trap cleanup EXIT

check() { # 描述 期望 实际
  if [ "$2" = "$3" ]; then
    echo "  ✓ $1"
  else
    echo "  ✗ $1（期望 $2，实际 $3）"
    FAIL=1
  fi
}

wait_count() { # pattern 次数 超时秒
  local pat="$1" need="$2" timeout="$3" n
  for _ in $(seq 1 "$timeout"); do
    n=$(grep -c -- "$pat" "$LOG" 2>/dev/null || true)
    [ "${n:-0}" -ge "$need" ] && return 0
    sleep 1
  done
  return 1
}

echo "== 准备测试数据目录 =="
rm -rf "$DATA" "$LOG"
mkdir -p "$DATA"
cat > "$DATA/config.json" <<'JSON'
{
  "version": 1,
  "account": "",
  "accountSuffixFallback": "@campus",
  "checkIntervalSec": 4,
  "offlineIntervalSec": 2,
  "probeTimeoutSec": 2,
  "probeUrls": ["http://127.0.0.1:18080/204"],
  "backoff": [1, 1, 2],
  "maxPasswordFails": 2,
  "pauseMinutes": 1,
  "logLevel": "DEBUG",
  "dorm": { "base": "http://127.0.0.1:18080", "loadConfig": true, "fallbackSuffix": "@campus" },
  "public": { "base": "http://127.0.0.1:9", "refererPath": "/a79.htm", "preferStandardPort": true }
}
JSON

printf 'secret-test-pw' | "$EXE" --set-credentials 20250000 --data "$DATA_W" >/dev/null
if [ $? -ne 0 ]; then
  echo "✗ 写入测试凭据失败"
  exit 1
fi

echo "== 启动假 AC =="
python "$ROOT_W/tests/mock_portal.py" >/tmp/m2-mock.log 2>&1 &
MOCK_PID=$!
sleep 1
curl -sf "$BASE/__state" >/dev/null || { echo "✗ 假 AC 没起来（端口 18080 被占用？）"; exit 1; }

echo "== 启动服务（--console） =="
CAMPUSNET_DATA="$DATA_W" "$EXE" --console --debug >"$LOG" 2>&1 &
SVC_PID=$!

# ---- 1. 首次自动登录 ----
echo "== 场景 1：离线 → 自动判别 → 登录 =="
if wait_count "登录成功\[" 1 25; then
  check "首次自动登录成功" "1" "1"
else
  check "首次自动登录成功" "1" "0"
fi
wait_count "复核出网: 已恢复" 1 8 >/dev/null 2>&1 && check "登录后复核出网" "1" "1" || check "登录后复核出网" "1" "0"

# ---- 2. 掉线自愈 ----
echo "== 场景 2：模拟被注销 → 自动重登 =="
curl -sf "$BASE/__drop" >/dev/null
if wait_count "登录成功\[" 2 30; then
  check "掉线后自动重登" "1" "1"
else
  check "掉线后自动重登" "1" "0"
fi

# ---- 3. 密码错 → 暂停 ----
echo "== 场景 3：密码错 → 连续失败后暂停 =="
printf 'WRONG-PW' | "$EXE" --set-credentials 20250000 --data "$DATA_W" >/dev/null
sleep 1
curl -sf "$BASE/__drop" >/dev/null
if wait_count "账号/密码类失败" 1 40; then
  check "密码错触发暂停" "1" "1"
else
  check "密码错触发暂停" "1" "0"
fi

# ---- 4. 断言账号后缀 ----
echo "== 场景 4：校验所有登录请求的账号格式 =="
STATE=$(curl -sf "$BASE/__state")
python - "$STATE" <<'PY'
import json, sys
st = json.loads(sys.argv[1])
seen = st.get("account_seen") or []
ok = {"20250000@campus", "20250000"}   # 带后缀是主尝试；纯学号是 AC 不认后缀时的兼容重试
bad = [a for a in seen if a not in ok]
suffixed = [a for a in seen if a.endswith("@campus")]
print(f"  账号请求样本={seen[:4]}  登录成功次数={st.get('logins')}")
if not seen:
    print("  ✗ 没有任何登录请求"); sys.exit(1)
if bad:
    print(f"  ✗ 出现意外的账号格式: {bad}"); sys.exit(1)
if not suffixed:
    print("  ✗ 没有一次带 @campus 的主尝试"); sys.exit(1)
sys.exit(0)
PY
check "账号格式正确（带 @campus 为主，纯学号仅兼容重试）" "0" "$?"

# ---- 汇总 ----
echo
echo "===== 关键日志 ====="
grep -E "网络判别|开始登录|登录成功|登录失败|复核出网|暂停|退避|网络变化|配置已重新加载|凭据已更新" "$LOG" | tail -30
echo
if [ "$FAIL" = "0" ]; then
  echo "===== M2 E2E 全部通过 ====="
  exit 0
else
  echo "===== M2 E2E 有失败项 ====="
  exit 1
fi
