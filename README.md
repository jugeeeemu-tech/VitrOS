# VitrOS

Rust で書かれた x86_64 UEFI OS カーネル

## 必要なツール

### Ubuntu/Debian

```bash
# QEMU と UEFI ファームウェア
sudo apt install qemu-system-x86 ovmf

# Rust (インストール時に nightly を選択するか、後で追加)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# nightly ツールチェーンと必要なコンポーネント
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
```

## セットアップ

### KVM の有効化（推奨）

KVM を有効にすると QEMU のパフォーマンスが大幅に向上します。

```bash
# kvm グループに追加
sudo usermod -aG kvm $USER

# WSL2 の場合は再起動が必要
wsl --shutdown  # PowerShell から実行
```

再ログイン後、`groups` コマンドで `kvm` が含まれていることを確認してください。

## ビルド & 実行

```bash
# ビルドと QEMU 起動（一発で完了）
cargo run
```

`cargo run` を実行すると:
1. UEFI ブートローダーをビルド
2. カーネルをビルド
3. EFI パーティション構造を作成
4. QEMU で起動

## テスト

```bash
cargo +nightly test -p vitros-kernel --target x86_64-unknown-none
```

## オプション

### メモリアロケータ可視化

```bash
KERNEL_FEATURES=visualize-allocator cargo run
```

### 描画パイプライン可視化

```bash
KERNEL_FEATURES=visualize-pipeline cargo run
```

## プロジェクト構造

```
VitrOS/
├── bootloader/   # UEFI ブートローダー
├── kernel/       # OS カーネル
├── common/       # 共有ライブラリ
└── scripts/      # ビルド・起動スクリプト
```