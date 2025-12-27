#!/bin/bash -e
PROJ_ROOT="$(dirname $(dirname ${BASH_SOURCE:-$0}))"
cd "${PROJ_ROOT}"

# cargo run の場合、$1 にブートローダーのパスが渡される
PATH_TO_EFI="$1"

# カーネルをビルド
echo "Building kernel..."
KERNEL_BUILD_CMD="cargo +nightly build -p je4os-kernel --target x86_64-unknown-none"

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
cp target/x86_64-unknown-none/debug/je4os-kernel mnt/kernel.elf

# QEMU起動
echo "Launching QEMU..."
qemu-system-x86_64 \
    -m 4G \
    -bios /usr/share/ovmf/OVMF.fd \
    -drive format=raw,file=fat:rw:mnt \
    -device isa-debug-exit,iobase=0xf4,iosize=0x01 \
    -chardev stdio,id=char_com1,mux=on,logfile=serial.log \
    -serial chardev:char_com1 \
    -mon chardev=char_com1