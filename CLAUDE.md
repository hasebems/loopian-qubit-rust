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
cargo build --features etude   # PCA9544 1台×2ch 構成（AT42QT1070 2個、12 キー）で動作仕様を決めるための版。PCA9544 が必須。debug_stream を含む
cargo build --features no_pca9544  # PCA9544 無しで AT42QT1070 1個を直結する試験用（6 キー。etude とは独立）
cargo build --features no_pca9544,debug_stream  # デバッグ環境: USB を MIDI + CDC にし、タッチの生値を PC に送る（doc/debug_env.md）
cargo fmt                       # CI で --check される
cargo clippy --all-features -- --deny=warnings   # CI と同じ条件。push 前に通すこと

# PC アプリ（qubit_etude、デバッグ環境用）。必ず tools/qubit_etude の中で実行する（直下の .cargo/config.toml のターゲットが効くため）
cd tools/qubit_etude && cargo run --release       # GUI
cd tools/qubit_etude && cargo run --example probe # GUI 無しでファームとの通信を確かめる
cd tools/qubit_etude && cargo run --example replay_algo -- 記録.qlog   # 記録を touch_algo に通して結果を表示
cd crates/touch_algo && cargo build && cargo test --target host-tuple  # 信号処理とタッチの解析（no_std のビルドと、ホストでのテスト）
```

- ユニットテストは無い（no_std・実機依存）。確認は `cargo check` / clippy と実機動作で行う。PC アプリ（`tools/qubit_etude`）と `crates/touch_algo` には `cargo test` がある（プロトコルの解釈・記録ファイル・統計・信号処理・タッチの解析）
- `build.rs` が `memory.x` を OUT_DIR にコピーし、`BUILD_DATE` と `BUILD_VERSION`（`Cargo.toml` の version）を環境変数として埋め込む。起動画面（OLED page 0）に表示される
- リンカの引数（`--nmagic`・`-Tlink.x`・`-Map=output.map`）も `build.rs` から渡す（`cargo:rustc-link-arg-bins`）。`.cargo/config.toml` の rustflags には `target-cpu` だけを置く。rustflags で渡すと、新しい cargo（nightly）でリンカに届かずリンクに失敗したため
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
- `touch_task`: I2C1 を専有。`Ticker` で `scan_period_us()`（既定は `SCAN_PERIOD_US` = 10ms）毎に、スキャン（`read_touch`）→ 信号処理（`touch_algo::electrode` の `ElectrodeSignal`。ノイズ除去・基準値）→ `touch_algo::touch` の `TouchTracker` で depth・position を求めて追跡 → `touch_algo::playinfo` の `PlayInfo` で演奏情報（Note On/Off・ビブラート）にして `MIDI_TX` へ、をひと続きに行う。信号処理と解析は、前の解析から `ANALYSIS_PERIOD_US`（10ms）経ったフレームで行う。`touch_algo` の時間（指を保持する時間・Note On までの時間・ビブラート）は、周期の数ではなく実時間（`TouchTracker::update` に渡す µs）で数えるので、解析の間隔が延びても時間の感覚はずれない（`doc/touch_analysis.md` §3.8）。中央値フィルタ・平滑化はサンプルの数のまま。フレームの数ではなく時刻で決めるので、スキャンが周期を超えても解析の間隔は平均 10ms に保たれる。周期を超えたら `PERIOD_OVERRUN` を数えて `ticker.reset()` し、遅れた周期を取り戻さない。起動後 `TOUCH_STARTUP_SETTLE_MS`（500ms）はチップが自身を校正しているので、信号処理にも解析にも渡さない。その後 `touch_algo` が基準値を初期取得し（300ms）、続けてキー毎のノイズ幅（noise_floor）を自動で測る間（3 秒、`doc/touch_baseline.md` §3.12）は output が 0。この間はセンサーに触れないこと（触れていたキーの noise_floor は既定値になる）。その後も Running の間は、noise_floor を常に測り直す（負の振れで上げ、5 秒毎に |delta| の最大より大きければ 1 下げる。§3.12）。終わるまで `TOUCH_CALIBRATING` が true で、OLED の起動画面（page 0）に `Calibrating...` が点滅する。OLED の POINT 表示（キー 0〜5）は output
- I2C1 は Core1 の中で `I2c::new_async` する。embassy-rp は呼び出したコアの NVIC で割り込みを有効にするので、こうすると I2C1 の割り込みも Core1 で処理される

**Core0**
- `midi_tx_task`: `MIDI_TX` のパケットを USB MIDI に送る（USB への送信はこのタスクだけ）
- `usb_task`: USB デバイス駆動（`main.rs`）
- `midi_rx_task`: USB MIDI を受信する。受信した Note On/Off を `RINGLED_RX_BITS` に反映する処理は、今は止めている（`SHOW_RX_NOTES` = false。動作モードを無くして Violin の形にしたため）
- `pressure_task`: 10ms 毎に ADC 3ch をサンプリングし、`PRESSURE` を算出。CC11 (Expression) を常に `MIDI_TX` へ（最初の 1 回は初期値）
- `ringled_task`: 20ms 毎に `TOUCH0-3` と `RINGLED_RX_BITS` を読んで NeoPixel を描画（書き込みのタイムアウトは 15ms）
- `ui_task`: 100ms 毎にスイッチを判定してページを切り替え、OLED は 200ms 毎（5fps）に描画して I2C0 で転送。描画（1 画面 約 3ms）はテキスト 1 行・図形 1 つ毎に `yield_now` で他のタスクに順番を譲る（`DrawPacer`、`doc/task_architecture.md` §4.4.1）。診断ページの `Drw` は 1 回に Core0 を止めた最長の時間
- `status_led_task`: 内蔵 LED（ハートビート / エラーコード）
- `debug_stream_task`（`debug_stream` feature のみ）: `DEBUG_FRAMES` のタッチの生値を USB CDC で PC に送り、PC からのコマンドを受ける。この feature ではスキャン周期の既定が 2ms になり、PC の `period` コマンドで 2 / 5 / 10 / 20ms に変えられる。Note On/Off を EVENT として送る（`doc/debug_env.md`）

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
- `src/constants.rs`: キー数・MIDI チャンネル・スキャン周期など。`etude`（12 キー）・`no_pca9544`（6 キー）feature でキー数が変わる
- `src/devices/`: I2C デバイスの最小ドライバ（`at42qt`, `pca9544`, `ssd1306`）。I2C を所有せず、呼び出し毎に `&mut I2C` を借用する
- `src/touch/read_touch.rs`: PCA9544 の ch 切替と AT42QT の生値（Key Signal）の読み込み、hi/lo ずれの読み直し。チップの基準値は使わない。ch 順は `CH_CONVERSION`（`[0, 1, 2, 3]`、そのまま）で決まり、キー番号は読み取り順のまま（回転しない）
- `src/touch/pressure.rs`: ADC の移動平均基準値（上昇 / 下降で追従率を変える）と CC11 変換テーブル。送る CC はパケットとして返し、送信はタスク側が行う
- `src/ui/oled_display.rs`: OLED ページ描画（page 0–3 と 5（診断）・6（I2C 診断）を左右スイッチで巡回、4 は設定画面、10 以降はデモ）
- `src/ui/ringled.rs`: NeoPixel の描画ロジック
- `src/debug_protocol.rs`: `debug_stream` のパケットの組み立て。PC 側の解釈は `tools/qubit_etude/src/protocol.rs` で、形式を変えるときは両方を直す
- `tools/qubit_etude/`: PC アプリ（ホスト向けの独立したプロジェクト。`doc/debug_env.md` §4）
- `crates/touch_algo/`: タッチの計算。no_std で、ファーム（`touch_task`）と PC アプリが同じコードを使う。電極 1 つの単位は electrode、指 1 本の単位は touch と呼ぶ（`doc/touch_analysis.md` §7.2）
    - `electrode.rs`: 電極毎の信号処理（ノイズ除去・基準値、`doc/touch_baseline.md` §3）。`ElectrodeSignal`、パラメータは `Params::DEFAULT`
    - `touch.rs`: depth・position の層。電極の output から指 1 本の depth（output の合計）と position（重心）を求め、追跡する（最大 4 点。`doc/touch_analysis.md` §3）。`TouchTracker`。ノートは知らない。`MAX_TOUCH_POINTS` もここにあり、`constants.rs` が再公開する
    - `playinfo.rs`: 演奏情報の層。`TouchTracker` の結果（指毎の position・depth・`Track`）から、ノート変換・Note On/Off・Move・リングのつなぎ目の扱い・ベロシティ・ビブラートを決める（§4）。`PlayInfo`。指毎に状態（待機 → 測定中 → 発音中）を持ち、Note On は depth が `NOTE_ON_DEPTH` に届いた・はじいた・上限の時間が過ぎた、のどれかで出す（§4.2）。電極の output は見ない。MIDI 出力はコールバック経由
    - 共有状態（`TOUCH0-3`・`ANY_TOUCH`・`DEBUG_VALUE`）は `touch_task` が両方の結果を読んで書く（2 つの層の分け方は `doc/touch_analysis.md` §5.6）

## MIDI

- 動作モード（Piano / Violin）は 2026-11-13 に無くした。今は Violin の形だけで、Piano の処理は後で足す
- ノートと CC11 は ch 2 (`MIDI_CH_VIOLIN` = 1)。電極 2 つで 1 ノート（`ELECTRODES_PER_NOTE`）、ノート 0 は `NOTE_OFFSET`（55）。SWAM Solo Strings 向けで、Expression (CC11) を送らないと音が出ない（`doc/midi_spec.md`）ので、起動後に CC11 の初期値を送り、その後は圧力から常に送る
- ベロシティは音量ではなく音の立ち上がりを変えるもの。Note On までの時間で決める（`doc/touch_analysis.md` §4.2）。Move の Note On は `VELOCITY_MOVE`（50）で固定
- ビブラート（CC1・CC19）の検出は、コードは残して止めている（`PlayInfo::set_vibrato_enabled`）
- 左右スイッチの同時押しで設定画面（page 4、`SETTING_MODE=true`）に入り、エラーコードと診断値（最小・最大・回数）もクリアされる。設定画面で設定する項目は今は無い。設定画面にいる間はタッチ・圧力の基準値補正（タッチは `touch_algo` の校正: 基準値の速い追従と noise_floor の計測）を行うので、センサーに触れないこと

## エラー処理の約束事

- パニックさせない方針。失敗は `error::set(error::XXX)` で 2 桁のコードを記録し、内蔵 LED が十の位 → 一の位の回数だけ点滅する。panic ハンドラは 55（`error::PANIC`）を書く。ただし LED を点滅させる `status_led_task` は Core0 にあるので、Core0 で panic したときや `status_led_task` の起動に失敗したとき（51）は LED では表示できない
- コードは `src/error.rs` に定数で定義する。十の位は機能の分類（1x: タッチ、2x: USB・MIDI、3x: 圧力、4x: 表示、5x: システム）、一の位はその中の番号で、点滅を数えやすいよう **どちらも 1–5 の範囲** で採番する
- I2C・MIDI 送信・NeoPixel 書き込みなど、ハードウェア待ちは `with_timeout` で包み、1 つのデバイスが固まってもタスク全体が止まらないようにする
- タスクの spawn 失敗もエラーコードに記録する（`match task(...) { Ok(token) => spawner.spawn(token), Err(_) => ... }`）

## コーディング上の注意

- ヒープ無し。配列は固定長、static は `StaticCell`（`make_static!` マクロ）で確保する
- Core1 のスタックは `CORE1_STACK_SIZE` = 16KB（`TouchTracker`・`PlayInfo` が合わせて約 3.7KB あるため）、Core0 は `memory.x` の `_stack_size` = 8KB。大きな配列をタスク内のローカル変数に置くときはスタック量に注意する
- 処理時間や周期超過は OLED の診断ページ（page 5）で確認できる。計測値は `shared.rs` の `SCAN_TIME` / `ANALYSIS_TIME` / `UI_DRAW_TIME`（`TimeStat`）、`PERIOD_OVERRUN`、`MIDI_TX_MAX_USED` / `MIDI_TX_OVERFLOW`
- スキャン時間の内訳は I2C 診断ページ（page 6）で確認できる。I2C1 の通信 1 回の時間（`I2C_SELECT_TIME`・`I2C_READ_TIME`・`I2C_DISCONNECT_TIME`）と、hi/lo ずれの読み直し（`I2C_REREAD_COUNT`）・失敗（`I2C_FAIL_COUNT`）の回数
- 感度などのチューニング値は各モジュール先頭の `const` にまとまっている（`pressure.rs`, `read_touch.rs`, `touch_algo` の `touch.rs`・`playinfo.rs`）と `touch_algo::electrode` の `Params::DEFAULT`。調整の意図を日本語コメントで残す
- `doc/` の Markdown は設計メモ・作業記録。`doc/_old/` は git 管理外の古い資料置き場（改修前の OLED ダブルバッファの説明 `double_buffering.md` など）
- 設計書: `doc/task_architecture.md`（コア・タスク構成）、`doc/hw_modify.md`（ハード改修）、`doc/debug_env.md`（デバッグ環境）、`doc/touch_baseline.md`（タッチの基準値）、`doc/touch_analysis.md`（タッチの解析: 複数のキーから 1 つのタッチを求め、演奏情報を取り出す）、`doc/hw96_check.md`（96 キーの新ハードウェアができてから確認すること）
