#!/bin/bash -e
PROJ_ROOT="$(dirname $(dirname ${BASH_SOURCE:-$0}))"
cd "${PROJ_ROOT}"

PATH_TO_EFI="$1"

# 引数が渡されない場合はブートローダもビルド
if [ -z "$PATH_TO_EFI" ]; then
    echo "Building bootloader..."
    cargo build --release -p je4os-bootloader --target x86_64-unknown-uefi
    PATH_TO_EFI="target/x86_64-unknown-uefi/release/je4os-bootloader.efi"
fi

# カーネルをビルド
echo "Building kernel..."
KERNEL_BUILD_CMD="cargo +nightly build --release -p je4os-kernel --target x86_64-unknown-none"

# KERNEL_FEATURES環境変数またはビルドマーカーファイルをチェック
if [ -n "$KERNEL_FEATURES" ]; then
    echo "  with features: $KERNEL_FEATURES"
    KERNEL_BUILD_CMD="$KERNEL_BUILD_CMD --features $KERNEL_FEATURES"
elif [ -f "target/x86_64-unknown-uefi/release/VISUALIZE_ENABLED" ]; then
    echo "  with features: visualize-allocator"
    KERNEL_BUILD_CMD="$KERNEL_BUILD_CMD --features visualize-allocator"
fi

eval $KERNEL_BUILD_CMD

# EFIパーティション構造を準備
rm -rf mnt
mkdir -p mnt/EFI/BOOT/

# ブートローダをコピー
cp ${PATH_TO_EFI} mnt/EFI/BOOT/BOOTX64.EFI

# カーネルをコピー（将来的にブートローダが読み込む）
cp target/x86_64-unknown-none/release/je4os-kernel mnt/kernel.elf

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