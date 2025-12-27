sudo apt install qemu-system-x86
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

qemu-system-x86_64 \
    -enable-kvm \
    -m 2G \
    -bios /usr/share/ovmf/OVMF.fd \
    -drive format=raw,file=fat:rw:mnt

メモリアロケータ可視化
KERNEL_FEATURES=visualize-allocator cargo run