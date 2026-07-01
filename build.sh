#!/bin/bash
# build.sh — 交叉编译 m3u8mcd / m3u8mcctl 并将产物放到 bin/<target-triple>/
#
# 依赖：cargo-zigbuild（推荐）或 cross
#   pip install ziglang && cargo install cargo-zigbuild
#   或
#   cargo install cross --git https://github.com/cross-rs/cross
#
# 用法：
#   ./build.sh aarch64-unknown-linux-musl       # IPQ807x / RK35xx 等 AArch64 路由器
#   ./build.sh mipsel-unknown-linux-musl         # 老款 MIPS 路由器
#   ./build.sh x86_64-unknown-linux-musl         # x86 软路由
#   ./build.sh                                   # 默认 aarch64

set -euo pipefail

TARGET="${1:-aarch64-unknown-linux-musl}"
PROFILE=release
OUT_DIR="$(dirname "$0")/bin/${TARGET}"

# 选择编译后端
if command -v cargo-zigbuild &>/dev/null || python3 -c "import ziglang" &>/dev/null 2>&1; then
    BUILD_CMD="cargo zigbuild"
elif command -v cross &>/dev/null; then
    BUILD_CMD="cross build"
else
    echo "错误：请先安装 cargo-zigbuild 或 cross"
    echo "  cargo install cargo-zigbuild"
    echo "  pip install ziglang"
    exit 1
fi

echo "==> 目标架构: ${TARGET}"
echo "==> 编译后端: ${BUILD_CMD}"
echo "==> 输出目录: ${OUT_DIR}"

cd "$(dirname "$0")"

${BUILD_CMD} \
    --target "${TARGET}" \
    --profile "${PROFILE}" \
    -p m3u8mcd \
    -p m3u8mcctl

mkdir -p "${OUT_DIR}"
cp "target/${TARGET}/${PROFILE}/m3u8mcd"  "${OUT_DIR}/m3u8mcd"
cp "target/${TARGET}/${PROFILE}/m3u8mcctl" "${OUT_DIR}/m3u8mcctl"

echo ""
echo "==> 编译完成"
ls -lh "${OUT_DIR}/"
echo ""
echo "下一步：在 OpenWrt SDK 的 package/ 目录下执行"
echo "  RUST_TARGET_TRIPLE=${TARGET} make package/m3u8mc/compile V=s"
