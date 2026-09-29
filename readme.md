# Loopian::QUBIT in Rust

96 個の静電タッチパッドを円形に並べた MIDI コントローラ「Loopian::QUBIT」のファームウェア。Seeed XIAO RP2350 上で、組み込み Rust と Embassy で動作する。

> **注意**: 本書は `doc/task_architecture.md` の新しいコア・タスク構成を前提に書いている。この構成への移行は `task_architecture` ブランチで進めている。

## 概要

- 2026/2 より開発開始。2025 年に開発した Arduino 版 Loopian::QUBIT の機能を Rust に移植
- `#![no_std]` / `#![no_main]`、Embassy の非同期タスクで構成し、RP2350 の 2 つのコアを使い分ける
- リングに触れた位置をノートに変換して USB MIDI で送る。受信したノートはリング LED に表示する
- 動作モードは 2 つ
    - **Piano**: タッチでノートを送る（MIDI ch 13）
    - **Violin**: ノートに加えて、圧力センサの値を Expression（CC11）として送る（MIDI ch 2、SWAM Solo Strings 向け）

## ハードウェア

| 機能 | デバイス | 接続 |
|---|---|---|
| マイコン | Seeed XIAO RP2350 | |
| タッチセンサ | AT42QT1070 ×16（6 キー × 16 = 96 キー） | I2C1（GP6/GP7）、PCA9544 ×4 で切り替え |
| 表示 | SSD1306 OLED | I2C0（D6 = GP0 / D7 = GP1） |
| リング LED | NeoPixel（RGBW）×96 | GP5、PIO0 + DMA |
| 圧力センサ | 3ch | ADC（GP26/27/28） |
| スイッチ | 左右 2 個 | GP4（左）、GP2（右） |
| 内蔵 LED | ハートビート／エラーコード表示 | GP25 |
| USB | USB MIDI（＋デバッグ用 CDC） | |

OLED を専用の I2C0 に分けるハードウェアの改修は `doc/hw_modify.md` を参照。

## システム構成

2 つのコアに役割を分ける。Core1 はタッチの処理だけを一定周期で行い、Core0 は入出力をまとめて受け持つ。

```
Core1: タッチ                               Core0: 入出力
┌─────────────────────────────┐            ┌──────────────────────────────────────┐
│ touch_task（10ms 周期）      │            │ midi_tx_task   ──► USB MIDI 送信      │
│   スキャン (I2C1)            │  MIDI_TX   │ midi_rx_task   ◄── USB MIDI 受信      │
│     ▼                        │ ─────────► │ usb_task                              │
│   信号処理（基準値・ノイズ）  │            │ pressure_task  ADC → 圧力 → CC11      │
│     ▼                        │  TOUCH0-3  │ ringled_task   NeoPixel の描画        │
│   QubitTouch で位置を解析    │ ─────────► │ ui_task        スイッチ・OLED (I2C0)  │
│     ▼                        │            │ status_led_task 内蔵 LED              │
│   ノートイベント             │            │ （debug_stream_task  デバッグ出力）   │
└─────────────────────────────┘            └──────────────────────────────────────┘
```

- Core1 はスキャンした直後に同じコアで解析するので、周期のずれや、他の処理による遅れが起きない
- コアの間の受け渡しは、MIDI のイベントのキュー（`MIDI_TX`）と、タッチ位置・動作モードなどの Atomic だけ
- 各タスクは共有状態（`src/shared.rs`）を介してやり取りし、それぞれ独立した周期で動く

### ソースの構成

```
src/
├── main.rs        初期化、両コアの起動、タスクの spawn
├── shared.rs      タスク間の共有状態
├── error.rs       エラーコード
├── constants.rs   キー数・MIDI チャンネルなどの定数
├── tasks/         タスク本体（ループ・周期・共有状態の読み書き）
├── touch/         タッチのロジック（スキャン、信号処理、位置の解析、圧力）
├── ui/            表示のロジック（OLED のページ、リング LED）
└── devices/       I2C デバイスの最小ドライバ（AT42QT1070, PCA9544, SSD1306）
```

## 使い方

- **モードの切替**: 左右のスイッチを同時に押すと設定画面に入り、左スイッチでモードを切り替える。設定画面にいる間はタッチと圧力の基準値を補正するので、センサーに触れないこと
- **エラーの表示**: 異常があると、内蔵 LED が 2 桁のエラーコードを十の位 → 一の位の回数だけ点滅する。一覧は `src/error.rs`

## ビルドと書き込み

```sh
cargo build --release
cargo run --release                # picotool で書き込み・実行（BOOTSEL/USB 接続が必要）
cargo build --features test_mode   # PCA9544 1 台 × 1ch 構成（6 キー）
cargo build --features no_pca9544  # PCA9544 無しで AT42QT1070 1 個を直結（6 キー）
cargo clippy --all-features -- --deny=warnings
```

開発環境は `flake.nix` + direnv でも用意できる。

## デバッグ環境（計画中）

タッチセンサの生値を USB CDC で PC に送り、PC のモニターアプリ（`tools/qubit_monitor`、Rust + egui）でリアルタイムに表示・記録する。基準値やノイズ除去のアルゴリズムは、ファームと PC アプリで同じコード（`crates/touch_algo`）を使い、PC 上で調整してからファームに移す。詳細は `doc/debug_env.md`。

## ドキュメント

| ファイル | 内容 |
|---|---|
| `doc/task_architecture.md` | コア・タスク構成の改修 |
| `doc/hw_modify.md` | ハードウェアの改修（OLED の I2C0 への移動） |
| `doc/debug_env.md` | デバッグ環境（USB CDC、PC モニターアプリ） |
| `doc/touch_baseline.md` | タッチセンサの基準値の自前管理とノイズ対策 |
| `doc/midi_spec.md` | MIDI の仕様 |
| `doc/i2c_problem260802.md` | I2C ロックアップの調査記録 |
| `doc/after_mft26.md` | MFT2026 での記録と今後の課題 |

## 開発について

- Embassy は git の `main` ブランチを追従している
- Embassy 周りのかなりの部分は GitHub Copilot を、2026/9 以降の設計・改修は Claude Code を利用して開発
