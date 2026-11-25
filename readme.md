# Loopian::QUBIT in Rust

96 個の静電タッチパッドを円形に並べた MIDI コントローラ「Loopian::QUBIT」のファームウェア。Seeed XIAO RP2350 上で、組み込み Rust と Embassy で動作する。

## 概要

- 2026/2 より開発開始。2025 年に開発した Arduino 版 Loopian::QUBIT の機能を Rust に移植
- `#![no_std]` / `#![no_main]`、Embassy の非同期タスクで構成し、RP2350 の 2 つのコアを使い分ける
- リングに触れた位置をノートに変換して USB MIDI（ch 2）で送る。SWAM Solo Strings（Violin）向け
    - 触れる深さ（depth）から Expression（CC11、弓速）を作る。触れる速さで、Note On のベロシティ（音の立ち上がり）を決める
    - 指を揺らすとビブラート（CC1・CC19）、指を滑らせると隣のノートへ移る（ポルタメント）
    - 圧力センサは Bow Pressure（CC80、弓圧）に割り当てる（センサを ADC につなぐまでは送らない）
- 仕様の詳細は `doc/touch_analysis.md`（タッチの解析）と `doc/midi_spec.md`（SWAM の MIDI）

## ハードウェア

| 機能 | デバイス | 接続 |
|---|---|---|
| マイコン | Seeed XIAO RP2350 | |
| タッチセンサ | AT42QT1070 ×16（6 キー × 16 = 96 キー） | I2C1（GP6/GP7）、PCA9544 ×4 で切り替え。I2C0（GP0/GP1）と 2 系統に分ける予定 |
| 表示 | SSD1306 OLED | PIO の I2C（GP3/GP4） |
| スイッチ | 左右 2 個 | PCF8574（PIO の I2C）の P0（左）・P1（右） |
| リング LED | NeoPixel（RGBW）×96 | GP5、PIO0 + DMA |
| 圧力センサ | 3ch | ADC（GP26/27/28） |
| 内蔵 LED | ハートビート／エラーコード表示 | GP25 |
| USB | USB MIDI（＋デバッグ用 CDC） | |

ハードウェアの改修は `doc/hw_modify.md` を参照。上の表は 2 回目の改修（§8: OLED とスイッチを PIO の I2C に移し、I2C0 をセンサーに使う）の後の基板のもの。

## システム構成

2 つのコアに役割を分ける。Core1 はタッチの処理だけを一定周期で行い、Core0 は入出力をまとめて受け持つ（`doc/task_architecture.md`）。

```
Core1: タッチ                                  Core0: 入出力
┌────────────────────────────────┐            ┌────────────────────────────────────────────┐
│ touch_task（10ms 周期）         │            │ midi_tx_task    ──► USB MIDI 送信           │
│   スキャン (I2C1)               │  MIDI_TX   │ midi_rx_task    ◄── USB MIDI 受信           │
│     ▼                           │ ─────────► │ usb_task                                    │
│   信号処理（基準値・ノイズ）    │            │ pressure_task   ADC → 圧力 → CC80           │
│     ▼                           │  TOUCH0-3  │ ringled_task    NeoPixel の描画             │
│   depth・position の追跡        │ ─────────► │ ui_task         スイッチ・OLED（PIO の I2C）│
│     ▼                           │            │ status_led_task 内蔵 LED                    │
│   演奏情報（ノート・CC）        │            │ debug_stream_task デバッグ出力（feature）   │
└────────────────────────────────┘            └────────────────────────────────────────────┘
```

- Core1 はスキャンした直後に同じコアで信号処理と解析を行うので、周期のずれや、他の処理による遅れが起きない
- 信号処理と解析は `crates/touch_algo` にあり、ファームと PC アプリ（`tools/qubit_etude`）が同じコードを使う
- コアの間の受け渡しは、MIDI のパケットのキュー（`MIDI_TX`）と、タッチ位置などの Atomic だけ
- 各タスクは共有状態（`src/shared.rs`）を介してやり取りし、それぞれ独立した周期で動く

### ソースの構成

```
src/
├── main.rs            初期化、両コアの起動、タスクの spawn
├── shared.rs          タスク間の共有状態と診断値
├── error.rs           エラーコード
├── constants.rs       キー数・MIDI チャンネル・スキャン周期などの定数
├── debug_protocol.rs  デバッグ環境で PC に送るパケットの組み立て（debug_stream feature）
├── tasks/             タスク本体（ループ・周期・共有状態の読み書き）
├── touch/             タッチセンサの読み込み、圧力
├── ui/                表示のロジック（OLED のページ、リング LED）
└── devices/           デバイスの最小ドライバ（AT42QT1070, PCA9544, SSD1306, PCF8574）と PIO の I2C

crates/
└── touch_algo/      タッチの信号処理と解析（no_std。ファームと PC アプリで共有）

tools/
└── qubit_etude/     PC アプリ（タッチ信号の表示・記録・再生。ホスト向けの独立したプロジェクト）
```

## 使い方

- **起動直後**: 約 4 秒、タッチの基準値とノイズの幅を測る（OLED に `Calibrating...` が点滅する）。この間はセンサーに触れないこと
- **ページの切替**: 右スイッチで次、左スイッチで前のページ。page 5 は診断（処理時間・周期の超過・MIDI のキュー）、page 6 は I2C の診断
- **設定画面**: 左右のスイッチを同時に押すと入り、右スイッチで抜ける。入るとエラーコードと診断値がクリアされる。設定画面にいる間はタッチと圧力の基準値を補正するので、センサーに触れないこと
- **エラーの表示**: 異常があると、内蔵 LED が 2 桁のエラーコードを十の位 → 一の位の回数だけ点滅する。一覧は `src/error.rs`

## ビルドと書き込み

```sh
cargo build --release
cargo run --release                # picotool で書き込み・実行（BOOTSEL/USB 接続が必要）
cargo build --features etude   # PCA9544 1 台 × 2ch 構成（AT42QT1070 2 個、12 キー）。debug_stream を含み、生値を PC に送る
cargo build --features no_pca9544  # PCA9544 無しで AT42QT1070 1 個を直結（6 キー）
cargo run --release --features no_pca9544,debug_stream   # デバッグ環境（6 キー直結、生値を PC に送る）
cargo clippy --all-features -- --deny=warnings
```

feature フラグと PC アプリの起動方法の一覧は `doc/build.md`。

## デバッグ環境

タッチセンサの生値を USB CDC で PC に送り、PC のアプリ（`tools/qubit_etude`、Rust + egui）でリアルタイムに表示・記録する。詳細は `doc/debug_env.md`。

- ファームを `debug_stream` feature 付きで書き込むと、USB が MIDI + CDC（仮想シリアル）の複合デバイスになる。MIDI の演奏データはこれまでどおり流れる
- PC アプリは `cd tools/qubit_etude && cargo run --release` で起動する（必ずそのディレクトリで実行する）
- できること: 生値の時系列グラフ、Note On/Off とマーカーの表示、スキャン周期の切替（2 / 5 / 10 / 20ms）、記録（`.qlog`）と再生、統計（ノイズ・サンプル間隔・値の更新間隔）、CSV 書き出し
- 記録を `touch_algo` に通して結果を見られる（`cargo run --example replay_algo -- 記録.qlog`）。アルゴリズムは PC 上で調整してから、同じコードのままファームで使う

## ドキュメント

| ファイル | 内容 |
|---|---|
| `doc/build.md` | feature フラグ、ビルド・書き込み、PC アプリの起動方法のメモ |
| `doc/task_architecture.md` | コア・タスク構成 |
| `doc/touch_baseline.md` | タッチセンサの信号処理（基準値・ノイズ対策） |
| `doc/touch_analysis.md` | タッチの解析（depth・position、演奏情報） |
| `doc/midi_spec.md` | SWAM Solo Strings の MIDI の仕様 |
| `doc/debug_env.md` | デバッグ環境（USB CDC、PC アプリ qubit_etude） |
| `doc/hw_modify.md` | ハードウェアの改修 |
| `doc/hw96_firmware261114.md` | 96 キーの基板（I2C 2 系統）に合わせたファームの対応 |
| `doc/hw96_check.md` | 96 キーの基板ができてから確かめること・決めること |
| `doc/not_yet.md` | 決まっていないことの一覧 |
| `doc/i2c_problem260802.md` | I2C ロックアップの調査記録 |
| `doc/after_mft26.md` | MFT2026 での記録 |

## 開発について

- Embassy は git の `main` ブランチを追従している
- Embassy 周りのかなりの部分は GitHub Copilot を、2026/9 以降の設計・改修は Claude Code を利用して開発
