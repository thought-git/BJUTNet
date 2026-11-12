#!/usr/bin/env bash
# 一键出安装包：编译 → 组装 dist → 打成单文件 BJUTNetSetup-<ver>.exe
#
# 用法: bash build/bundle.sh [版本号]      默认 1.0.0
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
VER="${1:-1.0.0}"
DIST="$ROOT/dist"
OUT_DIR="$ROOT/release"

export CAMPUSNET_VERSION="$VER"

echo "== 编译（版本 $VER） =="
cargo build --release --offline --workspace

echo "== 组装 dist =="
rm -rf "$DIST" "$OUT_DIR"
mkdir -p "$DIST" "$OUT_DIR"
cp target/release/BJUTNet.exe          "$DIST/"
cp target/release/BJUTNetSvc.exe       "$DIST/"
cp target/release/BJUTNetUninstall.exe "$DIST/"
printf '%s\n' "$VER" > "$DIST/version.txt"

echo "== 打包 =="
./target/release/cn-pack.exe \
  --template target/release/BJUTNetSetup.exe \
  --dist "$DIST" \
  --version "$VER" \
  --out "$OUT_DIR/BJUTNetSetup-$VER.exe"

echo
echo "== 产物 =="
ls -la "$OUT_DIR"
echo "dist 内容:"
ls -la "$DIST"
