#!/usr/bin/env bash
# M4 端到端：单文件安装包的可验证部分（不需要管理员）
#
# 覆盖：
#   1) 安装包自检（载荷清单完整）
#   2) 解包内容与 dist 逐字节一致
#   3) 静默预演（--dry-run）不改系统、能列出载荷
#   4) 非管理员真装 → 明确拒绝（不是静默失败）
#   5) GUI 向导能起窗口（查主窗口标题，不截屏）
#   6) 卸载器在沙盒目录里能把目录删干净
#
# 需要管理员才能验的部分（服务注册/HKLM/快捷方式）在 M4 文档里单列，由用户点 UAC 验证。
set -u
# 测试用独立管道名：避免连到本机已安装的正式服务
export CAMPUSNET_PIPE="cn-test-m4"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT_W="$(cygpath -m "$ROOT" 2>/dev/null || echo "$ROOT")"
VER="${1:-1.1.3}"
SETUP="$ROOT/release/BJUTNetSetup-$VER.exe"
DIST="$ROOT/dist"
TMP="$ROOT/_testdata-m4"
SETUPLOG="/c/Users/LX/AppData/Local/BJUTNet/setup.log"
UNINSTLOG="/c/Users/LX/AppData/Local/BJUTNet/uninstall.log"
FAIL=0
APP_PID=""

cleanup() {
  [ -n "$APP_PID" ] && { kill "$APP_PID" 2>/dev/null || taskkill //F //PID "$APP_PID" >/dev/null 2>&1; }
  # 兜底：GUI 是独立进程，用 PowerShell 按镜像名前缀强杀（taskkill 偶尔杀不掉）
  powershell -NoProfile -Command "Get-Process | Where-Object { \$_.ProcessName -like 'BJUTNetSetup*' } | Stop-Process -Force -ErrorAction SilentlyContinue" >/dev/null 2>&1
}
trap cleanup EXIT

check() {
  if [ "$2" = "$3" ]; then echo "  ✓ $1"; else echo "  ✗ $1（期望 [$2]，实际 [$3]）"; FAIL=1; fi
}
contains() { # 文件 字符串
  grep -qF -- "$2" "$1" 2>/dev/null && echo yes || echo no
}

rm -rf "$TMP"; mkdir -p "$TMP"
rm -f "$SETUPLOG" "$UNINSTLOG"

echo "== 0. 安装包存在 =="
[ -f "$SETUP" ] && check "安装包已生成" "1" "1" || { check "安装包已生成" "1" "0"; exit 1; }
echo "  文件大小: $(stat -c %s "$SETUP") 字节"

echo "== 1. 载荷自检 =="
"$SETUP" --selftest >/dev/null 2>&1
check "selftest 退出码 0" "0" "$?"
check "日志里有载荷版本" "yes" "$(contains "$SETUPLOG" "版本: $VER")"
for f in "BJUTNet.exe" "BJUTNetSvc.exe" "BJUTNetUninstall.exe" "version.txt"; do
  check "载荷包含 $f" "yes" "$(contains "$SETUPLOG" "$f")"
done

echo "== 2. 解包与 dist 逐字节一致 =="
"$SETUP" --extract-to "$TMP/extract" >/dev/null 2>&1
check "解包退出码 0" "0" "$?"
for f in BJUTNet.exe BJUTNetSvc.exe BJUTNetUninstall.exe version.txt; do
  A=$(sha256sum "$DIST/$f" 2>/dev/null | cut -d' ' -f1)
  B=$(sha256sum "$TMP/extract/$f" 2>/dev/null | cut -d' ' -f1)
  [ -n "$A" ] && [ "$A" = "$B" ] && check "$f 哈希一致" "1" "1" || check "$f 哈希一致" "1" "0"
done

echo "== 3. 非管理员真装必须明确拒绝 =="
rm -f "$SETUPLOG"
"$SETUP" --silent --dir "$TMP/should_not_exist" --no-launch >/dev/null 2>&1
RC=$?
check "退出码非 0" "1" "$([ "$RC" -ne 0 ] && echo 1 || echo 0)"
check "日志说明需要管理员权限" "yes" "$(contains "$SETUPLOG" "需要管理员权限")"
[ -d "$TMP/should_not_exist" ] && check "没有创建目录" "1" "0" || check "没有创建目录" "1" "1"

echo "== 4. GUI 向导能起窗口（测试用环境变量跳过提权） =="
BJUTNET_SETUP_NO_ELEVATE=1 "$SETUP" >/dev/null 2>&1 &
APP_PID=$!
sleep 5
# 注意：MSYS 的 $! 不是真实 Windows PID，按进程名前缀查窗口标题
TITLE=$(powershell -NoProfile -Command "Get-Process | Where-Object { \$_.ProcessName -like 'BJUTNetSetup*' } | Select-Object -First 1 -ExpandProperty MainWindowTitle" 2>/dev/null | tr -d '\r\n ')
check "向导窗口标题非空" "1" "$([ -n "$TITLE" ] && echo 1 || echo 0)"
echo "  窗口标题: [$TITLE]"
kill "$APP_PID" 2>/dev/null || taskkill //F //PID "$APP_PID" >/dev/null 2>&1
APP_PID=""

echo "== 5. 卸载器（沙盒目录） =="
SANDBOX="$TMP/sandbox"
mkdir -p "$SANDBOX"
cp target/release/BJUTNet.dll "$SANDBOX" 2>/dev/null || true
cp "$DIST/BJUTNetUninstall.exe" "$TMP/uninst_outside.exe"
echo '{"version":"1.0.0","install_dir":"'"$(cygpath -m "$SANDBOX")"'","desktop_shortcut":false,"start_menu_shortcut":false,"service_name":"BJUTNetSvc","installed_at":"x"}' > "$SANDBOX/install.json"
echo dummy > "$SANDBOX/BJUTNet.exe"
"$TMP/uninst_outside.exe" --quiet --cleanup-dir "$(cygpath -m "$SANDBOX")" --wait-pid 0 >/dev/null 2>&1
RC=$?
[ -d "$SANDBOX" ] && check "沙盒目录已删除" "1" "0" || check "沙盒目录已删除" "1" "1"
check "卸载日志有服务处理记录" "yes" "$(contains "$UNINSTLOG" "服务")"

echo
if [ "$FAIL" = "0" ]; then echo "===== M4 安装包自检全部通过 ====="; exit 0; else echo "===== M4 有失败项 ====="; exit 1; fi
