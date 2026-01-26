#!/bin/bash
# VitrOS カーネルテスト実行スクリプト
#
# テストバイナリをビルドし、ブートローダー経由でQEMUで実行して結果を確認する。
# 終了コード:
#   0 - テスト成功
#   1 - テスト失敗またはエラー

set -euo pipefail

# 色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

# タイムアウト秒数
TIMEOUT_SECONDS=60

# スクリプトのディレクトリを取得
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${PROJECT_ROOT}"

echo -e "${YELLOW}=== VitrOS Test Runner ===${NC}"

# ブートローダーをビルド
echo -e "${YELLOW}Building bootloader...${NC}"
if ! cargo +nightly build -p vitros-bootloader --target x86_64-unknown-uefi 2>&1; then
    echo -e "${RED}Bootloader build failed!${NC}"
    exit 1
fi

# テストバイナリをビルドし、パスを取得
echo -e "${YELLOW}Building test binary...${NC}"
BUILD_OUTPUT=$(cargo +nightly test -p vitros-kernel --target x86_64-unknown-none --no-run --lib 2>&1)
BUILD_STATUS=$?

echo "$BUILD_OUTPUT"

if [ $BUILD_STATUS -ne 0 ]; then
    echo -e "${RED}Test build failed!${NC}"
    exit 1
fi

# カーゴ出力からテストバイナリのパスを抽出
# 形式: "Executable unittests src/lib.rs (target/...)"
TEST_BINARY=$(echo "$BUILD_OUTPUT" | sed -n 's/.*Executable unittests src\/lib\.rs (\([^)]*\)).*/\1/p')

if [ -z "$TEST_BINARY" ]; then
    echo -e "${RED}Test binary not found in cargo output!${NC}"
    exit 1
fi

echo -e "${YELLOW}Test binary: ${TEST_BINARY}${NC}"

# EFIパーティション構造を準備
rm -rf mnt
mkdir -p mnt/EFI/BOOT/

# ブートローダをコピー
cp target/x86_64-unknown-uefi/debug/vitros-bootloader.efi mnt/EFI/BOOT/BOOTX64.EFI

# テストバイナリをカーネルとしてコピー
cp "${TEST_BINARY}" mnt/kernel.elf

# QEMUでテストを実行
echo -e "${YELLOW}Running tests in QEMU...${NC}"
echo "----------------------------------------"

# QEMUを実行（タイムアウト付き）
# isa-debug-exit デバイスを使用してテスト結果を取得
# 終了コード: 33 (0x21) = 成功, 35 (0x23) = 失敗
set +e
timeout ${TIMEOUT_SECONDS}s qemu-system-x86_64 \
    -machine q35,accel=kvm:tcg \
    -m 4G \
    -bios /usr/share/ovmf/OVMF.fd \
    -drive format=raw,file=fat:rw:mnt \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -serial stdio \
    -display none \
    -no-reboot \
    2>&1

QEMU_EXIT_CODE=$?
set -e

echo "----------------------------------------"

# 終了コードを解釈
case $QEMU_EXIT_CODE in
    33)
        echo -e "${GREEN}All tests passed!${NC}"
        exit 0
        ;;
    35)
        echo -e "${RED}Tests failed!${NC}"
        exit 1
        ;;
    124)
        echo -e "${RED}Tests timed out after ${TIMEOUT_SECONDS} seconds!${NC}"
        exit 1
        ;;
    *)
        echo -e "${RED}Unexpected exit code: ${QEMU_EXIT_CODE}${NC}"
        exit 1
        ;;
esac
