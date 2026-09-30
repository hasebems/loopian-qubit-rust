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
| I2C0 (400kHz) | SDA=GP0 (D6), SCL=GP1 (D7) | SSD1306 OLED (0x3C)。`ui_task` (Core0) が専有 |
| I2C1 (400kHz) | SDA=GP6, SCL=GP7 | PCA9544 ×4 (0x70–0x73)。`touch_task` (Core1) が専有 |
| タッチセンサ | PCA9544 の各 ch 先に AT42QT1070 (0x1B) | 4台×4ch=16個 ×6キー = 96キー |
| NeoPixel (RGBW) | GP5, PIO0 SM0 + DMA_CH0 | 96 LED のリング |
| ADC (圧力センサ) | GP26/27/28, DMA_CH1 | 3ch |
| スイッチ | GP2 (右), GP4 (左) | プルアップ、Low で押下 |
| 内蔵 LED | GP25 | Active Low。ハートビート / エラーコード表示 |
| USB | USB MIDI (VID 0x1209 / PID 0x3691) | |

回路図は `mcuboard_schematic.pdf`、I2C ロックアップの調査記録は `doc/i2c_problem260802.md`。OLED を I2C0 に移すハードウェア改修は `doc/hw_modify.md`（改修前の基板では OLED が表示されず、エラー 42 になる）。

## アーキテクチャ

デュアルコアで、各コアが独立した Embassy `Executor` を持つ。Core1 はタッチの処理だけ、Core0 は入出力を受け持つ（設計は `doc/task_architecture.md`）。タスクの本体は `src/tasks/`、一覧は `src/tasks/mod.rs`。

**Core1**
- `touch_task`: I2C1 を専有。`Ticker` で `SCAN_PERIOD_MS`（10ms）毎に、スキャン（`read_touch`）→ `QubitTouch` で解析 → Note On/Off を `MIDI_TX` へ、をひと続きに行う。解析は `ANALYSIS_DIVIDER` フレームに 1 回（`QubitTouch` の時間の定数が 10ms 毎の呼び出しを前提としているため）。周期を超えたら `PERIOD_OVERRUN` を数えて `ticker.reset()` し、遅れた周期を取り戻さない。起動後 `TOUCH_STARTUP_SETTLE_MS`（500ms）は基準値が落ち着いていないので解析しない
- I2C1 は Core1 の中で `I2c::new_async` する。embassy-rp は呼び出したコアの NVIC で割り込みを有効にするので、こうすると I2C1 の割り込みも Core1 で処理される

**Core0**
- `midi_tx_task`: `MIDI_TX` のパケットを USB MIDI に送る（USB への送信はこのタスクだけ）
- `usb_task`: USB デバイス駆動（`main.rs`）
- `midi_rx_task`: 受信した Note On/Off を `RINGLED_RX_BITS` に反映（Violin モードでは無視）
- `pressure_task`: 10ms 毎に ADC 3ch をサンプリングし、`PRESSURE` を算出。Violin モードの CC11 (Expression) と、モードに入ったときの All Sound Off を `MIDI_TX` へ
- `ringled_task`: 20ms 毎に `TOUCH0-3` と `RINGLED_RX_BITS` を読んで NeoPixel を描画（書き込みのタイムアウトは 15ms）
- `ui_task`: 100ms 毎にスイッチを判定してページ / 動作モードを切り替え、OLED は 200ms 毎（5fps）に描画して I2C0 で転送。描画は `await` の無い CPU 処理（約 3ms）で、その間 Core0 の他のタスクは待たされる
- `status_led_task`: 内蔵 LED（ハートビート / エラーコード）

### タスク間の受け渡し

- 共有状態は `src/shared.rs` にまとめ、各項目に書き手と読み手をコメントで書く。基本は `portable_atomic` の Atomic（`Ordering::Relaxed`）で、各タスクは共有状態をポーリングして独立周期で動く（経緯は `doc/ringled_modify.md`）
- 例外は MIDI 送信の `MIDI_TX`（容量 16 の `Channel`）。書き手は `touch_task`（Core1）と `pressure_task`（Core0）で、`tasks::midi::queue_midi` で `try_send` する（待たない。あふれたら捨ててエラー 22）
- コアをまたぐのは `MIDI_TX` と Atomic だけ。タッチの生データはコアをまたがない
- `TOUCH0-3` はタッチ位置 ×100（0–9999）、10000 は未タッチ
- `RINGLED_RX_BITS` は `[AtomicU32; RINGLED_RX_WORDS]`（1 ビット/LED、LED n は `[n / 32]` の `n % 32` ビット目）

### モジュール

- `src/main.rs`: 初期化・両コアの起動・タスクの spawn だけ
- `src/shared.rs`: タスク間の共有状態と、診断用の計測値（`TimeStat` など）
- `src/error.rs`: エラーコードの定数と `error::set` / `clear` / `get`
- `src/tasks/`: タスク本体（ループ・周期・共有状態の読み書き）。計算や描画のロジックは `touch/`・`ui/` に置く
- `src/constants.rs`: キー数・MIDI チャンネル・`WorkMode`・スキャン周期など。`test_mode` feature でキー数が変わる
- `src/devices/`: I2C デバイスの最小ドライバ（`at42qt`, `pca9544`, `ssd1306`）。I2C を所有せず、呼び出し毎に `&mut I2C` を借用する
- `src/touch/read_touch.rs`: PCA9544 の ch 切替と AT42QT 読み込み、基準値の差し引き。ch 順は `CH_CONVERSION` で反転、キー番号は `TOUCH_INDEX_SHIFT` で回転する
- `src/touch/qtouch.rs`: タッチ位置検出・追跡（最大 4 点）、ノート変換、ベロシティ、ビブラート検出。MIDI 出力はコールバック経由
- `src/touch/pressure.rs`: ADC の移動平均基準値（上昇 / 下降で追従率を変える）と CC11 変換テーブル。送る CC はパケットとして返し、送信はタスク側が行う
- `src/ui/oled_display.rs`: OLED ページ描画（page 0–3 と 5（診断）を左右スイッチで巡回、4 は設定画面、10 以降はデモ）
- `src/ui/ringled.rs`: NeoPixel の描画ロジック

## 動作モードと MIDI

- `WorkMode::Piano`: ノートは ch 13 (`MIDI_CH_FLOW` = 12)。受信ノートをリング LED に表示
- `WorkMode::Violin`: ノートと CC11 は ch 2 (`MIDI_CH_VIOLIN` = 1)。SWAM Solo Strings 向けで、Expression (CC11) を送らないと音が出ない（`doc/midi_spec.md`）。モードに入るとき All Sound Off と CC11 初期値を送る
- 左右スイッチの同時押しで設定画面（page 4、`SETTING_MODE=true`）に入り、エラーコードと診断値（最小・最大・回数）もクリアされる。設定画面で左スイッチを押すとモードが切り替わる。設定画面にいる間はタッチ・圧力の基準値補正を行うので、センサーに触れないこと

## エラー処理の約束事

- パニックさせない方針。失敗は `error::set(error::XXX)` で 2 桁のコードを記録し、内蔵 LED が十の位 → 一の位の回数だけ点滅する。panic ハンドラは 55（`error::PANIC`）を書く。ただし LED を点滅させる `status_led_task` は Core0 にあるので、Core0 で panic したときや `status_led_task` の起動に失敗したとき（51）は LED では表示できない
- コードは `src/error.rs` に定数で定義する。十の位は機能の分類（1x: タッチ、2x: USB・MIDI、3x: 圧力、4x: 表示、5x: システム）、一の位はその中の番号で、点滅を数えやすいよう **どちらも 1–5 の範囲** で採番する
- I2C・MIDI 送信・NeoPixel 書き込みなど、ハードウェア待ちは `with_timeout` で包み、1 つのデバイスが固まってもタスク全体が止まらないようにする
- タスクの spawn 失敗もエラーコードに記録する（`match task(...) { Ok(token) => spawner.spawn(token), Err(_) => ... }`）

## コーディング上の注意

- ヒープ無し。配列は固定長、static は `StaticCell`（`make_static!` マクロ）で確保する
- Core1 のスタックは `CORE1_STACK_SIZE` = 16KB（`QubitTouch` が約 3.7KB あるため）、Core0 は `memory.x` の `_stack_size` = 8KB。大きな配列をタスク内のローカル変数に置くときはスタック量に注意する
- 処理時間や周期超過は OLED の診断ページ（page 5）で確認できる。計測値は `shared.rs` の `SCAN_TIME` / `ANALYSIS_TIME` / `UI_DRAW_TIME`（`TimeStat`）、`PERIOD_OVERRUN`、`MIDI_TX_MAX_USED` / `MIDI_TX_OVERFLOW`
- 感度などのチューニング値は各モジュール先頭の `const` にまとまっている（`qtouch.rs`, `pressure.rs`, `read_touch.rs`）。調整の意図を日本語コメントで残す
- `doc/` の Markdown は設計メモ・作業記録。`doc/build.md` の `adc_ch4` feature は現在の `Cargo.toml` には存在しない。`doc/double_buffering.md` の OLED ダブルバッファは廃止済み
- 設計書: `doc/task_architecture.md`（コア・タスク構成）、`doc/hw_modify.md`（ハード改修）、`doc/debug_env.md`（デバッグ環境）、`doc/touch_baseline.md`（タッチの基準値）
