# CLAUDE.md

Loopian::QUBIT のファームウェア。96 個の静電タッチパッドが円形に並んだ MIDI コントローラで、Seeed XIAO RP2350 上で動作する。2025 年の Arduino 版（`ref/qubit_arduino/`、git 管理外）を組み込み Rust + Embassy に移植したもの。

- `#![no_std]` / `#![no_main]`、edition 2024、ターゲット `thumbv8m.main-none-eabihf`（`.cargo/config.toml` で既定に設定済み）
- Embassy は git の `main` ブランチを追従しているため、`cargo update` で API が変わることがある
- コメント・ドキュメント・コミットメッセージは日本語

## ビルド・書き込み・チェック

```sh
cargo build                     # dev ビルド (opt-level 2, overflow-checks 有効)
cargo build --release
cargo run --release             # picotool load -u -v -x -t elf で書き込み・実行（BOOTSEL/USB 接続が必要）
cargo build --features test_mode   # PCA9544 1台×1ch 構成（6 キー）で動かす試験用
cargo build --features no_pca9544  # PCA9544 無しで AT42QT1070 1個を直結する試験用（test_mode を含む）
cargo fmt                       # CI で --check される
cargo clippy --all-features -- --deny=warnings   # CI と同じ条件。push 前に通すこと
```

- ユニットテストは無い（no_std・実機依存）。確認は `cargo check` / clippy と実機動作で行う
- `build.rs` が `memory.x` を OUT_DIR にコピーし、`BUILD_DATE` と `BUILD_VERSION`（`Cargo.toml` の version）を環境変数として埋め込む。起動画面（OLED page 0）に表示される
- リンク時に `output.map` を生成する（git 管理外）
- 開発環境は `flake.nix` + direnv（`.envrc`）でも用意できる（stable Rust + picotool）

## ハードウェア構成

| 機能 | ピン / 周辺 | 備考 |
|---|---|---|
| I2C1 (400kHz) | SDA=GP6, SCL=GP7 | SSD1306 OLED (0x3C) と PCA9544 ×4 (0x70–0x73) |
| タッチセンサ | PCA9544 の各 ch 先に AT42QT1070 (0x1B) | 4台×4ch=16個 ×6キー = 96キー |
| NeoPixel (RGBW) | GP5, PIO0 SM0 + DMA_CH0 | 96 LED のリング |
| ADC (圧力センサ) | GP26/27/28, DMA_CH1 | 3ch |
| スイッチ | GP2 (右), GP4 (左) | プルアップ、Low で押下 |
| 内蔵 LED | GP25 | Active Low。ハートビート / エラーコード表示 |
| USB | USB MIDI (VID 0x1209 / PID 0x3691) | |

回路図は `mcuboard_schematic.pdf`、I2C ロックアップの調査記録は `doc/i2c_problem260802.md`。

## アーキテクチャ

デュアルコアで、各コアが独立した Embassy `Executor` を持つ（`src/main.rs`）。

**Core0**
- `qubit_touch_task`: 10ms 毎に `TOUCH_RAW_DATA` をコピーして `QubitTouch` で解析し、Note On/Off を USB MIDI 送信。Violin モードでは圧力から CC11 (Expression) も送る
- `usb_task`: USB デバイス駆動
- `midi_rx_task`: 受信した Note On/Off を `RINGLED_RX_BITS` に反映（Violin モードでは無視）
- `ringled_task`: 20ms 毎に `TOUCH0-3` と `RINGLED_RX_BITS` を読んで NeoPixel を描画
- `adc_task`: 10ms 毎に 3ch をサンプリングし、`touch::pressure::update_pressure` で `PRESSURE` を算出

**Core1**
- `core1_i2c_task`: I2C バスを専有。全キーのスキャン → `TOUCH_RAW_DATA` に書き込み、OLED へのバッファ転送
- `core1_oled_ui_task`: 100ms 毎に画面を描画し、スイッチ操作でページ / 動作モードを切り替え
- `core1_led_task`: 内蔵 LED 点滅

### タスク間の受け渡し

- タスク間のやり取りは `main.rs` のグローバル static（`portable_atomic` の Atomic、`Ordering::Relaxed`）が基本。イベントキューは使わず、各タスクは共有状態をポーリングして独立周期で動く（経緯は `doc/ringled_modify.md`）
- `TOUCH_RAW_DATA` のみ `Mutex<CriticalSectionRawMutex, _>`。ロック中に他の `await` をしないよう、コピーしてすぐ解放する
- `TOUCH0-3` はタッチ位置 ×100（0–9999）、10000 は未タッチ
- OLED は `BUFFER_FROM_DISPLAY` / `BUFFER_TO_DISPLAY`（容量 2 の Channel）でダブルバッファリング。Core1 起動前に空バッファを 2 つ投入しておく必要がある（`doc/double_buffering.md`）

### モジュール

- `src/constants.rs`: キー数・MIDI チャンネル・`WorkMode` など。`test_mode` feature でキー数が変わる
- `src/devices/`: I2C デバイスの最小ドライバ（`at42qt`, `pca9544`, `ssd1306`）。I2C を所有せず、呼び出し毎に `&mut I2C` を借用する
- `src/touch/read_touch.rs`: PCA9544 の ch 切替と AT42QT 読み込み、基準値の差し引き。ch 順は `CH_CONVERSION` で反転、キー番号は `TOUCH_INDEX_SHIFT` で回転する
- `src/touch/qtouch.rs`: タッチ位置検出・追跡（最大 4 点）、ノート変換、ベロシティ、ビブラート検出。MIDI 出力はコールバック経由
- `src/touch/pressure.rs`: ADC の移動平均基準値（上昇 / 下降で追従率を変える）と CC11 変換テーブル
- `src/ui/oled_display.rs`: OLED ページ描画（page 0–4 が実使用、10 以降はデモ）
- `src/ui/ringled.rs`: NeoPixel の描画ロジック

## 動作モードと MIDI

- `WorkMode::Piano`: ノートは ch 13 (`MIDI_CH_FLOW` = 12)。受信ノートをリング LED に表示
- `WorkMode::Violin`: ノートと CC11 は ch 2 (`MIDI_CH_VIOLIN` = 1)。SWAM Solo Strings 向けで、Expression (CC11) を送らないと音が出ない（`doc/midi_spec.md`）。モードに入るとき All Sound Off と CC11 初期値を送る
- 左右スイッチの同時押しで設定画面（page 4、`WORK_MODE_DISPLAY=true`）に入り、エラーコードもクリアされる。設定画面で左スイッチを押すとモードが切り替わる。設定画面にいる間はタッチ・圧力の基準値補正を行うので、センサーに触れないこと

## エラー処理の約束事

- パニックさせない方針。失敗は `ERROR_CODE` に 2 桁のコードを書き込み、内蔵 LED が十の位 → 一の位の回数だけ点滅する。panic ハンドラは 255 を書く
- コード一覧は `main.rs` の先頭コメントにある。新しいエラーを追加するときは、一の位・十の位とも 1–9 の範囲で採番し、一覧にも追記する
- I2C・MIDI 送信・NeoPixel 書き込みなど、ハードウェア待ちは `with_timeout` で包み、1 つのデバイスが固まってもタスク全体が止まらないようにする
- タスクの spawn 失敗もエラーコードに記録する（`match task(...) { Ok(token) => spawner.spawn(token), Err(_) => ... }`）

## コーディング上の注意

- ヒープ無し。配列は固定長、static は `StaticCell`（`make_static!` マクロ）で確保する
- Core1 のスタックは `CORE1_STACK_SIZE` = 8KB、Core0 は `memory.x` の `_stack_size` = 8KB。大きな配列をタスク内のローカル変数に置くときはスタック量に注意する
- 感度などのチューニング値は各モジュール先頭の `const` にまとまっている（`qtouch.rs`, `pressure.rs`, `read_touch.rs`）。調整の意図を日本語コメントで残す
- `doc/` の Markdown は設計メモ・作業記録。`doc/build.md` の `adc_ch4` feature は現在の `Cargo.toml` には存在しない
