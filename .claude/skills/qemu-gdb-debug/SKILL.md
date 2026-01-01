---
name: qemu-gdb-debug
description: Debug the vitrOS Rust kernel running in QEMU using GDB. Use when the user wants to debug the kernel, investigate crashes, triple faults, panics, or examine kernel state. Triggers on keywords like "gdb", "debug", "debugger", "crash", "triple fault", "panic", "backtrace", "registers", or debugging requests in Japanese like "デバッグ", "クラッシュ", "パニック".
allowed-tools: Read, Bash, Edit
---
  
# QEMU + GDB Kernel Debugging

このスキルは、QEMUで実行中のvitrOS Rustカーネルに対してGDBを接続し、デバッグを行います。

## 前提条件

- `ENABLE_GDB=1` 環境変数でQEMUを起動（ポート1234でGDBサーバーが待機）
- カーネルのデバッグシンボル付きELFファイルが `target/x86_64-unknown-none/debug/vitros-kernel` に存在

## 使用可能なデバッグコマンド

### 1. 基本的な状態確認

```bash
gdb -batch \
  -ex "target remote :1234" \
  -ex "bt" \
  -ex "info registers" \
  -ex "info threads" \
  target/x86_64-unknown-none/debug/vitros-kernel
```

### 2. 特定の関数にブレークポイントを設定

```bash
gdb -batch \
  -ex "target remote :1234" \
  -ex "break 関数名" \
  -ex "continue" \
  -ex "bt" \
  -ex "info locals" \
  -ex "info registers" \
  target/x86_64-unknown-none/debug/vitros-kernel
```

### 3. スタックとメモリのダンプ

```bash
gdb -batch \
  -ex "target remote :1234" \
  -ex "x/32x \$rsp" \
  -ex "x/32i \$rip" \
  -ex "info frame" \
  target/x86_64-unknown-none/debug/vitros-kernel
```

### 4. 特定のアドレスにブレークポイント

```bash
gdb -batch \
  -ex "target remote :1234" \
  -ex "break *0xアドレス" \
  -ex "continue" \
  -ex "bt" \
  -ex "disassemble" \
  target/x86_64-unknown-none/debug/vitros-kernel
```

## QEMUの起動

`launch_qemu.sh` は**デフォルトでGDBサーバーが無効**になっています。デバッグ時は環境変数で有効化します。

### 通常起動（GDB無効）
```bash
cargo run
```

### 環境変数での制御

- **GDB有効化**
  ```bash
  ENABLE_GDB=1 cargo run
  ```

- **起動時にGDB接続を待機**（カーネル初期化処理のデバッグ用）
  ```bash
  ENABLE_GDB=1 GDB_WAIT=1 cargo run
  ```

- **QEMUデバッグログの有効化**（トリプルフォールト調査用）
  ```bash
  QEMU_DEBUG_LOG=1 cargo run
  ```

- **GDBとデバッグログの組み合わせ**
  ```bash
  ENABLE_GDB=1 QEMU_DEBUG_LOG=1 cargo run
  ```

## デバッグワークフロー

1. **QEMUをGDBモードで起動**
   - `ENABLE_GDB=1 cargo run` でQEMUを起動
   - カーネルが実行され、ポート1234でGDB接続を待機

2. **問題の特定**
   - トリプルフォールト、パニック、予期しない動作が発生
   - または、特定の機能をステップ実行したい

3. **GDBコマンドの実行**
   - 状況に応じて適切なGDBコマンドを組み立て
   - バックトレース、レジスタ、メモリを確認

4. **段階的な調査**
   - 初回の結果を分析
   - 必要に応じて追加のブレークポイントや調査を実施
   - 問題の根本原因を特定

## カーネルパニック発生時のデバッグ

ユーザーがソースコードを編集後、自分で `ENABLE_GDB=1 cargo run` を実行してカーネルパニックが発生した場合の手順：

### カーネルパニック vs トリプルフォールトの違い

**カーネルパニック**
- Rustのpanicやassertionの失敗
- カーネルコード内でのソフトウェアエラー
- パニック後、カーネルは無限ループ（`loop {}`）で待機
- QEMUプロセスは**実行中のまま**
- **GDB接続可能** ✅

**トリプルフォールト**
- CPU例外（無効なIDT、ページフォルトの連鎖など）
- ハードウェアレベルのエラー
- `-no-reboot` のみの場合、QEMUプロセスが**終了**
- **`-no-reboot -no-shutdown` の組み合わせで、クラッシュ後もQEMU実行中** ✅
- この設定により**GDB接続可能** ✅

### カーネルパニック発生時のデバッグ手順

**シナリオ**
1. ユーザーがコードを編集
2. `ENABLE_GDB=1 cargo run` でQEMUを起動
3. カーネルパニックが発生
4. カーネルは無限ループで待機、QEMUは実行中
5. Claude Codeがデバッグを依頼される

**デバッグ手順**

**重要**: QEMUは既にユーザーが起動しており、実行中のため、新たに起動しない。

1. **パニックメッセージの確認**
   - `serial.log` または画面出力からパニックメッセージを確認
   - パニックの種類を特定（assertion failed, unwrap on None, など）

2. **実行中のQEMUに接続**
   ```bash
   # QEMUは既に起動しているので、直接GDBで接続
   gdb -batch \
     -ex "target remote :1234" \
     -ex "bt" \
     -ex "info registers" \
     -ex "x/32x \$rsp" \
     -ex "x/32i \$rip" \
     target/x86_64-unknown-none/debug/vitros-kernel
   ```

3. **パニックの原因を特定**
   - バックトレースからパニック発生箇所を特定
   - レジスタとスタックから状態を確認
   - 該当ソースコードを読んで原因を分析

4. **追加調査が必要な場合**
   - 特定の変数の値を確認
   - メモリダンプの取得
   ```bash
   gdb -batch \
     -ex "target remote :1234" \
     -ex "print 変数名" \
     -ex "x/100x アドレス" \
     target/x86_64-unknown-none/debug/vitros-kernel
   ```

### 注意事項
- **QEMUを新規に起動しない**: ユーザーが既に起動している
- **serial.log を確認**: パニックメッセージが記録されている可能性
- **バックトレースから問題箇所を特定**: ソースコードの該当行（file:line）を確認
- パニック後、カーネルは無限ループで待機しているため、何度でもGDB接続可能

## トリプルフォールトのデバッグ

`launch_qemu.sh` は `-no-reboot -no-shutdown` の組み合わせを使用しており、トリプルフォールト発生後もQEMUプロセスが実行中のため、**GDB接続が可能**です。

### トリプルフォールト発生後のデバッグ

**シナリオ**
1. ユーザーが `ENABLE_GDB=1 cargo run` でQEMUを起動
2. トリプルフォールトが発生
3. QEMUは終了せず、停止状態で実行中
4. Claude Codeがデバッグを依頼される

**デバッグ手順**

1. **QEMUログとGDB接続の併用**
   ```bash
   # 既に起動しているQEMUに接続
   gdb -batch \
     -ex "target remote :1234" \
     -ex "bt" \
     -ex "info registers" \
     -ex "x/32x \$rsp" \
     -ex "x/32i \$rip" \
     target/x86_64-unknown-none/debug/vitros-kernel
   ```

2. **QEMU_DEBUG_LOG=1 を使用している場合**
   - `qemu_debug.log` からトリプルフォールト発生時のCPU状態を確認
   - GDBのバックトレースと組み合わせて原因を特定

3. **典型的なトリプルフォールトの原因**
   - 無効なIDTエントリへのアクセス
   - ページフォルト処理中の更なるページフォルト
   - スタックオーバーフロー
   - カーネルスタックの破損

### 事前のブレークポイント設定（予防的デバッグ）

トリプルフォールトが再現可能な場合、事前にブレークポイントを設定して原因を特定：

```bash
# ターミナル1: QEMUを起動
ENABLE_GDB=1 cargo run

# ターミナル2: GDBで接続してブレークポイント設定
gdb -batch \
  -ex "target remote :1234" \
  -ex "break page_fault_handler" \
  -ex "continue" \
  -ex "bt" \
  -ex "info registers" \
  target/x86_64-unknown-none/debug/vitros-kernel
```

### 注意事項
- `-no-reboot -no-shutdown` により、トリプルフォールト後もQEMUは実行中
- トリプルフォールト発生時点の状態がそのまま保持される
- 何度でもGDB接続して状態を確認可能
- `qemu_debug.log` と併用することで、より詳細な分析が可能

## ベストプラクティス

- 最初に `bt` と `info registers` で全体像を把握
- スタックポインタ（$rsp）とインストラクションポインタ（$rip）を必ず確認
- シンボル情報がない場合は、アドレスから逆算して対応箇所を特定
- 複数回の調査が必要な場合は、段階的にブレークポイントを絞り込む

## 注意事項

- GDBは基本的に非対話モードで使用（`-batch` オプション）
- 完全に対話的なデバッグは制約がある
- 調査 → 結果分析 → 次の調査、のサイクルを回すことで効果的にデバッグ可能
- メモリ安全性の問題（unsafe ブロック、ポインタ操作）に特に注意
