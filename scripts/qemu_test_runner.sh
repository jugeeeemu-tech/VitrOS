#!/bin/bash
# QEMU Test Runner for VitrOS
#
# cargo test のランナーとして使用される。
# 引数としてテストバイナリのパスを受け取り、QEMUで実行する。

set -euo pipefail

# スクリプトのディレクトリを取得
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# テストバイナリのパス（cargo から渡される）
TEST_BINARY="$1"

# タイムアウト秒数
TIMEOUT_SECONDS=60

cd "${PROJECT_ROOT}"

# ブートローダーをビルド
cargo +nightly build -p vitros-bootloader --target x86_64-unknown-uefi

# EFIパーティション構造を準備
rm -rf mnt
mkdir -p mnt/EFI/BOOT/

# ブートローダをコピー
cp target/x86_64-unknown-uefi/debug/vitros-bootloader.efi mnt/EFI/BOOT/BOOTX64.EFI

# テストバイナリをカーネルとしてコピー
cp "${TEST_BINARY}" mnt/kernel.elf

# QEMUを実行（set +e で終了コードによる即座の終了を防ぐ）
# stdin を /dev/null にリダイレクトしてターミナル入力待ちを防ぐ
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
    </dev/null 2>&1

QEMU_EXIT_CODE=$?
set -e

# 終了コードを解釈
# isa-debug-exit: (value << 1) | 1
# Success (0x10) -> 33
# Failed (0x11) -> 35
case $QEMU_EXIT_CODE in
    33)
        exit 0
        ;;
    35)
        exit 1
        ;;
    124)
        echo "Tests timed out after ${TIMEOUT_SECONDS} seconds!" >&2
        exit 1
        ;;
    *)
        echo "Unexpected exit code: ${QEMU_EXIT_CODE}" >&2
        exit 1
        ;;
esac
