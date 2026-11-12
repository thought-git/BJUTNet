#!/usr/bin/env bash
# M5 用户反馈修复的端到端验证（不需要管理员）
#
# 针对 4 条反馈：
#   1. 校园网"连上但没登录"也被判成有网  → 严格 204 判定，劫持页不再算在线
#   2. 用 icon.ico 作图标              → 4 个 exe 都嵌了图标 + 版本信息
#   3. 宿舍网被识别成"非校园网"        → 在线时也做网络识别
#   4. 面板手动把当前网络加为校园网    → trustedNetworks + IPC 命令
set -u
# 测试用独立管道名：避免连到本机已安装的正式服务
export CAMPUSNET_PIPE="cn-test-m5"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT_W="$(cygpath -m "$ROOT" 2>/dev/null || echo "$ROOT")"
EXE="$ROOT/target/release/BJUTNetSvc.exe"
TMP="$ROOT/_testdata-m5"
BASE="http://127.0.0.1:18080"
FAIL=0
MOCK_PID=""
SVC_PID=""

cleanup() {
  [ -n "$SVC_PID" ] && { kill "$SVC_PID" 2>/dev/null || taskkill //F //PID "$SVC_PID" >/dev/null 2>&1; }
  [ -n "$MOCK_PID" ] && { kill "$MOCK_PID" 2>/dev/null || taskkill //F //PID "$MOCK_PID" >/dev/null 2>&1; }
}
trap cleanup EXIT

check() { if [ "$2" = "$3" ]; then echo "  ✓ $1"; else echo "  ✗ $1（期望 [$2]，实际 [$3]）"; FAIL=1; fi; }
contains() { grep -qF -- "$2" "$1" 2>/dev/null && echo yes || echo no; }
rpc() { "$EXE" --rpc "$1" 2>/dev/null; }
st() { rpc '{"cmd":"status"}' | python -c "import json,sys
try:
    d=json.load(sys.stdin)['data']; print(d.get('$1',''))
except Exception: print('<err>')"; }

write_cfg() { # $1=dir $2=probeUrls json $3=dorm base $4=public base $5=trusted json
  mkdir -p "$1"
  cat > "$1/config.json" <<JSON
{
  "version": 1,
  "account": "",
  "accountSuffixFallback": "@campus",
  "checkIntervalSec": 5,
  "offlineIntervalSec": 2,
  "probeTimeoutSec": 2,
  "probeUrls": $2,
  "backoff": [1, 1, 2],
  "maxPasswordFails": 3,
  "pauseMinutes": 1,
  "logLevel": "DEBUG",
  "dorm": { "base": "$3", "loadConfig": true, "fallbackSuffix": "@campus" },
  "public": { "base": "$4", "refererPath": "/a79.htm", "preferStandardPort": true },
  "trustedNetworks": $5
}
JSON
  printf 'secret-test-pw' | "$EXE" --set-credentials 20250000 --data "$(cygpath -m "$1")" >/dev/null 2>&1
}

rm -rf "$TMP"; mkdir -p "$TMP"
python "$ROOT_W/tests/mock_portal.py" >/tmp/m5-mock.log 2>&1 &
MOCK_PID=$!
sleep 1
curl -sf "$BASE/__state" >/dev/null || { echo "✗ 假 AC 没起来"; exit 1; }

echo "== 1. 未登录（劫持页）不能再判成在线 =="
write_cfg "$TMP/hijack" '["http://127.0.0.1:18080/captive"]' "$BASE" "http://127.0.0.1:9" "[]"
"$EXE" --probe --data "$(cygpath -m "$TMP/hijack")" >/tmp/m5-probe1.txt 2>&1
check "探测结论=被门户劫持" "yes" "$(contains /tmp/m5-probe1.txt "被门户劫持")"
check "没被判成在线" "no" "$(contains /tmp/m5-probe1.txt "结论     : 在线")"

echo "== 1b. 服务会因此去登录，登录后 204 恢复 → 在线 =="
CAMPUSNET_DATA="$(cygpath -m "$TMP/hijack")" "$EXE" --console --debug >/tmp/m5-svc1.log 2>&1 &
SVC_PID=$!
sleep 8
check '日志出现「开始登录」' "yes" "$(contains /tmp/m5-svc1.log "开始登录")"
check '日志出现「登录成功」' "yes" "$(contains /tmp/m5-svc1.log "登录成功[")"
check "登录后判定在线" "True" "$(st online)"
check "在线时网络类型=宿舍有线" "宿舍有线" "$(st net_kind)"
check "在线时也有判别依据" "yes" "$(contains /tmp/m5-svc1.log "网络判别")"
kill "$SVC_PID" 2>/dev/null || taskkill //F //PID "$SVC_PID" >/dev/null 2>&1
SVC_PID=""

echo "== 2. 图标与版本信息 =="
powershell -NoProfile -ExecutionPolicy Bypass -File "$ROOT_W/tests/check_icons.ps1" -Dir "$ROOT_W/target/release" 2>/dev/null | tr -d '\r' > /tmp/m5-icons.txt
for n in BJUTNet BJUTNetSvc BJUTNetUninstall BJUTNetSetup; do
  check "$n 有图标" "$n|icon" "$(grep -o "^$n|icon" /tmp/m5-icons.txt | head -1)"
done
check "版本信息里带产品名" "yes" "$(contains /tmp/m5-icons.txt "BJUTNet")"
cat /tmp/m5-icons.txt | sed 's/^/  /'

echo "== 3. 用户标记的校园网优先于门户探测 =="
write_cfg "$TMP/trusted" '["http://127.0.0.1:18080/204"]' "http://127.0.0.1:9" "http://127.0.0.1:9" \
  '[{"kind":"dorm","name":"我的宿舍网","ssid":"","gateway":"","subnet":"10.126.64.0/19"}]'
"$EXE" --probe --data "$(cygpath -m "$TMP/trusted")" >/tmp/m5-probe2.txt 2>&1
check "两个门户都不可达仍判宿舍有线" "yes" "$(contains /tmp/m5-probe2.txt "结果     : 宿舍有线")"
check "依据写明用户标记" "yes" "$(contains /tmp/m5-probe2.txt "用户标记")"

echo "== 4. 面板可把当前网络加为校园网（走 IPC） =="
write_cfg "$TMP/add" '["http://127.0.0.1:18080/204"]' "$BASE" "http://127.0.0.1:9" "[]"
CAMPUSNET_DATA="$(cygpath -m "$TMP/add")" "$EXE" --console >/tmp/m5-svc2.log 2>&1 &
SVC_PID=$!
sleep 4
OUT=$(rpc '{"cmd":"add_trusted_current","params":{"kind":"dorm"}}')
check "add_trusted_current 返回 ok" "True" "$(echo "$OUT" | python -c "import json,sys;print(json.load(sys.stdin)['ok'])")"
sleep 1  # 等 worker 重载配置后把 trusted 发到状态快照
check "返回 added=true" "True" "$(echo "$OUT" | python -c "import json,sys;print(json.load(sys.stdin)['data'].get('added'))")"
check "status 里能看到 1 条标记" "1" "$(rpc '{"cmd":"status"}' | python -c "import json,sys;print(len(json.load(sys.stdin)['data'].get('trusted',[])))")"
check "config.json 写入了 trustedNetworks" "yes" "$(contains "$TMP/add/config.json" "trustedNetworks")"
check "网段按 ip/prefix 算出来了" "yes" "$(contains "$TMP/add/config.json" "10.126.64.0/19")"
# 新增：重连并验证（先注销再登录）
check "verify_login 返回 ok" "True" "$(rpc '{"cmd":"verify_login"}' | python -c "import json,sys;print(json.load(sys.stdin)['ok'])")"
sleep 4
check "日志出现重连并验证" "yes" "$(contains /tmp/m5-svc2.log "重连并验证")"

check "remove_trusted ok" "True" "$(rpc '{"cmd":"remove_trusted","params":{"index":0}}' | python -c "import json,sys;print(json.load(sys.stdin)['ok'])")"
sleep 1
check "删除后 status 里 0 条" "0" "$(rpc '{"cmd":"status"}' | python -c "import json,sys;print(len(json.load(sys.stdin)['data'].get('trusted',[])))")"
echo "== 5. 暂停直到恢复 / 掉线即重登 / 关闭服务 =="
rpc '{"cmd":"pause","params":{"forever":true}}' >/dev/null
sleep 1
check "暂停直到恢复 → state=已暂停" "已暂停" "$(st state)"
check "paused_forever=true" "True" "$(rpc '{"cmd":"status"}' | python -c "import json,sys;print(json.load(sys.stdin)['data'].get('paused_forever'))")"
rpc '{"cmd":"resume"}' >/dev/null
sleep 5   # 先让它恢复在线，避免"注销"和"恢复登录"撞在一起
[ "$(st state)" != "已暂停" ] && check "恢复后离开暂停" "1" "1" || check "恢复后离开暂停" "1" "0"

# 掉线即重登：从"掉线"到日志出现下一次登录成功，应在 ~5s 探测间隔内完成
N0=$(grep -c '登录成功\[' /tmp/m5-svc2.log 2>/dev/null || true)
curl -sf "$BASE/__drop" >/dev/null
T0=$(date +%s)
OK=0
for _ in $(seq 1 20); do
  sleep 1
  N1=$(grep -c '登录成功\[' /tmp/m5-svc2.log 2>/dev/null || true)
  if [ "$N1" -gt "$N0" ]; then OK=1; break; fi
done
EL=$(( $(date +%s) - T0 ))
[ "$OK" = "1" ] && [ "$EL" -le 12 ] && check "掉线后 ${EL}s 内自动重登" "1" "1" || check "掉线后 ${EL}s 内自动重登" "1" "0"

check "关闭服务返回 ok" "True" "$(rpc '{"cmd":"stop_service"}' | python -c "import json,sys;print(json.load(sys.stdin)['ok'])")"
sleep 3
check "日志出现主循环结束" "yes" "$(contains /tmp/m5-svc2.log "主循环结束")"
SVC_PID=""


# stop_service 已经让服务自己退出；这里只做兜底清理
SVC_PID=""

echo
if [ "$FAIL" = "0" ]; then echo "===== M5 修复验证全部通过 ====="; exit 0; else echo "===== M5 有失败项 ====="; exit 1; fi
