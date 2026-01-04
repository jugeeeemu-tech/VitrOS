#!/bin/bash -e
PROJ_ROOT="$(dirname $(dirname ${BASH_SOURCE:-$0}))"
cd "${PROJ_ROOT}"

# cargo run の場合、$1 にブートローダーのパスが渡される
PATH_TO_EFI="$1"

# カーネルをビルド
echo "Building kernel..."
KERNEL_BUILD_CMD="cargo +nightly build -p vitros-kernel --target x86_64-unknown-none"

# 環境変数 KERNEL_FEATURES でカーネルの features を制御
if [ -n "$KERNEL_FEATURES" ]; then
    echo "  with features: $KERNEL_FEATURES"
    KERNEL_BUILD_CMD="$KERNEL_BUILD_CMD --features $KERNEL_FEATURES"
fi

eval $KERNEL_BUILD_CMD

# EFIパーティション構造を準備
rm -rf mnt
mkdir -p mnt/EFI/BOOT/

# ブートローダをコピー
cp ${PATH_TO_EFI} mnt/EFI/BOOT/BOOTX64.EFI

# カーネルをコピー（将来的にブートローダが読み込む）
cp target/x86_64-unknown-none/debug/vitros-kernel mnt/kernel.elf

# QEMU起動
echo "Launching QEMU..."

# GDBデバッグオプション（デフォルトで無効）
GDB_OPTS=""
if [ "$ENABLE_GDB" = "1" ]; then
    if [ "$GDB_WAIT" = "1" ]; then
        echo "  GDB server enabled on port 1234 (waiting for connection)"
        GDB_OPTS="-s -S"
    else
        echo "  GDB server enabled on port 1234"
        GDB_OPTS="-s"
    fi
fi

# QEMUデバッグログオプション
QEMU_LOG_OPTS=""
if [ "$QEMU_DEBUG_LOG" = "1" ]; then
    echo "  QEMU debug logging enabled -> qemu_debug.log"
    QEMU_LOG_OPTS="-d int,cpu_reset -D qemu_debug.log"
fi

# KVMオプション（利用可能な場合は自動的に有効化、DISABLE_KVM=1で無効化）
KVM_OPTS=""
if [ "$DISABLE_KVM" != "1" ] && [ -e /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    echo "  KVM acceleration enabled"
    KVM_OPTS="-enable-kvm -cpu host"
else
    if [ "$DISABLE_KVM" = "1" ]; then
        echo "  KVM disabled by user"
    else
        echo "  KVM not available (no /dev/kvm or permission denied)"
    fi
fi

qemu-system-x86_64 \
    -machine q35,accel=kvm:tcg \
    -m 4G \
    -no-reboot \
    -no-shutdown \
    -bios /usr/share/ovmf/OVMF.fd \
    -drive format=raw,file=fat:rw:mnt \
    -device isa-debug-exit,iobase=0xf4,iosize=0x01 \
    -chardev stdio,id=char_com1,mux=on,logfile=serial.log \
    -serial chardev:char_com1 \
    -mon chardev=char_com1 \
    $KVM_OPTS \
    $GDB_OPTS \
    $QEMU_LOG_OPTS