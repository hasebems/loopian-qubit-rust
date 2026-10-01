# コア・タスク構成の改修 設計書

> **その後の変更**（2026-10-01 追記）: 段階 1〜6 の改修は 2026-09-30 に終わった（§6.1）。その後、デバッグ環境（`doc/debug_env.md`）で次のように変わっている
> - `constants.rs` の `SCAN_PERIOD_MS` は `SCAN_PERIOD_US`（µs、既定値）に、`ANALYSIS_DIVIDER` は `analysis_divider(周期)` に変わった。実行中の周期は `shared::scan_period_us()` で読む（`debug_stream` feature では PC から変えられる）
> - エラー 25（予約）は `SPAWN_DEBUG_STREAM` として使っている（`debug_stream` feature のときだけ）
> - `task_architecture` ブランチは、そこから分けた `debug_env` ブランチにすべて含まれているので削除した（2026-10-01）。その後の改修（§4.4.1 の描画の分割など）も `debug_env` で行う


## 1. 目的と位置づけ

### 1.1 目的

- タッチの処理（スキャン → 解析 → ノートイベント）を **一定周期で、他の処理に乱されずに** 行えるようにする。触れる速さの検出（`doc/touch_baseline.md`）の前提になる
- I2C を 2 系統に分け、OLED がタッチのスキャンに影響しないようにする
- 肥大化した `main.rs`（約 770 行）を整理し、タスクと共有状態の役割をはっきりさせる
- 改修の途中で見つかった不具合を直す（§2.3）

### 1.2 作業の順番

本件は次の順で進める。本書はその 1 番目。

1. **コア・タスク構成の改修（本書）**
2. デバッグ環境（`doc/debug_env.md`）
3. タッチ信号処理・基準値（`doc/touch_baseline.md`）

### 1.3 方針

- **改修は動作を変えない** ことを原則とする。タッチの判定・MIDI の出力・LED の表示は、改修前と同じに振る舞うこと。変えるのは「どこで・いつ動くか」だけにする。例外は §2.3 の不具合の修正
- 段階（§6）ごとにビルドと実機確認をし、コミットする。どの段階で挙動が変わったかを追えるようにするため
- 作業は `task_architecture` ブランチで行う。ただし §2.3 の不具合の修正は、`main` から分岐した `fix_ringled_rx_bits` ブランチで先に行い、`main` に入れてから取り込む

## 2. 現状

### 2.1 コアとタスク

```
Core0 (Executor)                              Core1 (Executor)
├ qubit_touch_task  10ms+α  TOUCH_RAW_DATA を   ├ core1_i2c_task   周期なし (yield_now)
│                    読み qtouch 解析、           │   I2C1 を専有: タッチのスキャン
│                    MIDI 送信 (await)、CC11 送信 │   + OLED への転送 (約 25ms、100ms 毎)
├ usb_task                                      ├ core1_oled_ui_task 100ms
├ midi_rx_task       RINGLED_RX_BITS を更新      │   スイッチ判定・ページ／モード切替・描画
├ ringled_task       20ms  NeoPixel 描画         └ core1_led_task    内蔵 LED（ハートビート／エラー）
└ adc_task           10ms  圧力の計算

割り込み: I2C1_IRQ も含め、すべて Core0 の NVIC で有効化されている
```

### 2.2 問題点

| # | 問題 | 影響 |
|---|---|---|
| P1 | タッチのスキャンと OLED の転送が同じ I2C バス・同じタスクにある | OLED の転送中（約 25ms、100ms 毎。見積もり）はスキャンが止まる |
| P2 | Core1 のスキャンループに周期が無い | サンプル間隔がばらつき、時間を基準にした処理ができない |
| P3 | I2C1 の割り込みが Core0 で処理されている（`I2c::new_async` を Core0 の `main()` で呼んでいるため。embassy-rp は呼び出したコアの NVIC で割り込みを有効にする） | Core0 が割り込みを禁止している区間の分だけ、Core1 のタッチの I2C が遅れる |
| P4 | スキャン（Core1、周期なし）と解析（Core0、10ms＋処理時間）が別々の周期で動き、受け渡しにスキャン番号も時刻も無い | 同じデータを 2 回解析する、1 回飛ばす、が起き、最大 10ms 余分に遅れる。速さ検出に必要な「全フレームの解析」ができない |
| P5 | MIDI の送信（`with_timeout` 20ms 付きの `await`）が `qubit_touch_task` の中にある | USB が詰まると、タッチの解析そのものが止まる |
| P6 | 解析（Core0）が、OLED の描画・USB・リング LED・ADC と同じ Executor で動く | 協調型の Executor なので、他のタスクの処理が長いと解析が待たされる |
| P7 | 共有状態（Atomic・Mutex・Channel）が `main.rs` に散らばり、誰が書き誰が読むのかがコードから読み取りにくい | 改修・デバッグのときに影響範囲を追いにくい |
| P8 | エラーコードが数値の直書き | 一覧（コメント）とコードのずれに気づきにくい |

### 2.3 見つかった不具合: 受信ノートの LED が 3 箇所光る

- `RINGLED_RX_BITS` は `AtomicU32`（32 ビット）だが、LED は 96 個ある
- `midi_rx_task` の `1u32 << led`（[main.rs:477](../src/main.rs#L477)）と、`ringled.rs` の `(rxkey_bits >> source_index)`（[ringled.rs:67](../src/ui/ringled.rs#L67)）は、32 以上のシフト量になり得る
- release ビルド（`overflow-checks = false`）では、シフト量が 32 で折り返される。そのため 1 つのノート n で、LED n, n+32, n+64 の **3 箇所が光る**
- dev ビルド（`overflow-checks = true`）では、シフトの桁あふれで panic する（エラーコード 255）
- MFT2026 の「Loopian::App からの伴奏の LED 点灯が 3 箇所光ってしまう」（`doc/after_mft26.md`）の原因と考えられる
- 修正: 96 ビット分（`[AtomicU32; 3]`）にする。§6 の段階 1 で先に直す

## 3. 新しい構成

### 3.1 コアの役割

| | Core0: 入出力 | Core1: タッチ |
|---|---|---|
| 役割 | USB・MIDI・圧力・LED・OLED・スイッチ | タッチのスキャン → 信号処理 → 位置の解析 → ノートイベントの生成 |
| タスク | midi_tx、midi_rx、usb、pressure、ringled、ui、status_led、（後で）debug_stream | touch |
| I2C | I2C0（D6/D7 = GP0/GP1）: OLED | I2C1（GP6/GP7）: AT42QT1070 / PCA9544 |
| 割り込み | I2C0, USB, ADC, PIO0, DMA | **I2C1**（Core1 で `new_async` を呼ぶ） |

- Core1 にはタッチの処理だけを置く。スキャンした直後に同じコアで解析するので、2 つの周期のずれ（P4）が無くなり、Core0 の処理に待たされること（P6）も無くなる
- 後で信号処理（`touch_baseline.md` の `TouchSignal`）を載せるのも Core1 で、スキャンと解析の間に入る
- RP2350 の 2 つのコア（Cortex-M33）はどちらも FPU を持つので、`qtouch.rs` の f32 の計算を Core1 に移しても問題ない

### 3.2 タスク一覧

| タスク | コア | 周期・起動 | 内容 | 現在のタスク |
|---|---|---|---|---|
| `touch_task` | 1 | `Ticker`（`SCAN_PERIOD_MS`） | I2C1 を専有。タッチセンサの初期化、スキャン、`QubitTouch` での解析。ノートイベントを `MIDI_TX` に入れ、タッチ位置を `TOUCH0-3` に公開 | `core1_i2c_task` のタッチ部分 ＋ `qubit_touch_task` の解析部分 |
| `midi_tx_task` | 0 | `MIDI_TX` の受信で起動 | `Sender` を専有し、USB MIDI に送る（`with_timeout`） | `qubit_touch_task` の送信部分 |
| `midi_rx_task` | 0 | USB の受信で起動 | 受信ノートを `RINGLED_RX_BITS`（96 ビット）に反映 | 同名 |
| `usb_task` | 0 | 常駐 | USB デバイスの駆動 | 同名 |
| `pressure_task` | 0 | 10ms | ADC の読み取り、圧力の計算。Violin モードの CC11 と、モードに入ったときの All Sound Off を決めて `MIDI_TX` に入れる | `adc_task` ＋ `qubit_touch_task` の CC11 部分 |
| `ringled_task` | 0 | `Ticker` 20ms | NeoPixel の描画 | 同名 |
| `ui_task` | 0 | スイッチ 100ms、描画 200ms（5fps） | スイッチの判定、ページ／モードの切替、OLED の描画と I2C0 への転送 | `core1_oled_ui_task` ＋ `core1_i2c_task` の OLED 部分 |
| `status_led_task` | 0 | 常駐 | 内蔵 LED（ハートビート／エラーコード） | `core1_led_task` |
| `debug_stream_task` | 0 | （後で）`debug_env.md` | USB CDC へのデバッグデータ送信 | 新規 |

### 3.3 データの流れ

```
Core1                                        Core0
touch_task                                   midi_tx_task ──► USB MIDI
  スキャン (I2C1)                                 ▲
    ▼                                            │ MIDI_TX (Channel、コアをまたぐ)
  （後で）信号処理                                │
    ▼                                            ├────────── pressure_task ◄── ADC
  QubitTouch で解析 ──ノートイベント────────────┘     (CC11, All Sound Off)
    │                                                  ▲ ANY_TOUCH
    ├── TOUCH0-3, ANY_TOUCH ──────────────────────────┴──► ringled_task ◄── RINGLED_RX_BITS ◄── midi_rx_task ◄── USB MIDI
    ▲
    └── WORK_MODE, SETTING_MODE ◄──────────────── ui_task (スイッチ・OLED)
```

コアをまたぐのは `MIDI_TX`（キュー）と、いくつかの Atomic だけになる。96 キー分のデータをコアの間でコピーする必要は無くなる。

## 4. 詳細

### 4.1 Core1: touch_task

**I2C1 の生成**

- `p.I2C1`・`p.PIN_6`・`p.PIN_7` を `spawn_core1` のクロージャに渡し、その中で `I2c::new_async` を呼ぶ。これで I2C1 の割り込みは Core1 の NVIC で処理される（P3 の解消）
- `bind_interrupts!` の `Irqs` は共通のままでよい（ハンドラの登録は全体で 1 つ、有効化がコアごとになる）

**1 周期の処理**

```
loop {
    ticker.next().await;                       // SCAN_PERIOD_MS 毎
    read_touch.touch_sensor_scan(...).await;   // 全キーを読む（I2C の完了は await で待つ）
    if 解析する周期なら {
        qt.set_value(...);                     // 読んだ値を QubitTouch に渡す（関数呼び出し）
        qt.seek_and_update_touch_point(work_mode);
        // ノートイベントは QubitTouch のコールバックから MIDI_TX に try_send
    }
    時間を計測して診断用の Atomic に公開する
}
```

- `SCAN_PERIOD_MS` の初期値は **10ms**。`QubitTouch` の中の時間の定数（`TOUCH_SAMPLE_PERIOD_SEC` = 0.01、`RELEASE_WAITING_TIME`、ベロシティ計算の 10ms tick など）は「10ms 毎に呼ばれる」ことを前提にしているため。これで改修前と同じ振る舞いになる
- **解析する周期**: `SCAN_PERIOD_MS` を 10ms より短くする場合（デバッグ環境での 2ms など）でも、解析は 10ms 毎に行う。`ANALYSIS_DIVIDER = ANALYSIS_PERIOD_MS / SCAN_PERIOD_MS` フレームに 1 回解析する（`ANALYSIS_PERIOD_MS` = 10）。そのため `SCAN_PERIOD_MS` は 10 の約数（1, 2, 5, 10）に限り、倍数であることをコンパイル時に確認する。3 つの定数は `constants.rs` に置く
- **起動直後の `TOUCH_STARTUP_SETTLE_MS`（500ms）の間は、スキャンだけを行い解析しない**。`read_touch` はチップの基準値を最初のスキャンの後に初めて読むので、最初のフレームは基準値 0 で引かれて全キーが大きな値になる。スキャンの直後に必ず解析する構成では、これを確実に拾って誤ったノートを出してしまう（改修前は Core0 の解析が別周期で、読み飛ばすことが多かった）。チップ自身の校正（電源投入から 230ms 未満）と、基準値の読み直し（120ms 毎）が何度か行われるまで待つ
- 毎フレームの解析（フレーム駆動）と、`QubitTouch` の時間基準化は、`touch_baseline.md`（速さ検出）の段階で行う
- `ReadTouch` と `QubitTouch` は同じタスクの中の構造体で、値は関数呼び出しで渡す。今の `TOUCH_RAW_DATA`（Mutex）は不要になる
- 読み取り中に `SETTING_MODE`（§4.5）を参照する。基準値の補正（現状の `reference_adjust`）は、今の段階では従来どおり

**周期を超えたときの扱い**

- embassy-time の `Ticker::next()` は、期限を過ぎていると待たずにすぐ完了し、期限を 1 周期だけ進める。そのまま使うと、1 回の処理が周期を超えたとき、遅れた周期分を **待たずに連続して処理** して取り戻そうとする（例: 1 回 25ms かかると、次の 2 周期をほぼ 0ms 間隔で処理する）
- `QubitTouch` は 10ms 毎に呼ばれる前提なので、0ms 間隔で呼ばれると時間の計算（離したと判断するまでの待ち時間、ビブラート、ベロシティ）が狂う
- そのため、**遅れた周期は取り戻さずに捨てる**。処理の後で経過時間が周期を超えていたら、`PERIOD_OVERRUN` を数えて `ticker.reset()` を呼び、期限を「今＋1 周期」にし直す
    - 周期を超えた回は、その周期が延びるだけになる
    - 回数は診断ページに出し、普段は収まっているかを確かめる
- 現状の `qubit_touch_task` は `Timer::after(10ms)` なので連続処理は起きないが、毎回「処理時間＋10ms」の周期になっている
- 速さ検出の段階で `QubitTouch` を時間基準（実際の経過時間を使う形）にすれば、周期が延びても計算は狂わなくなる

**1 周期に収まらない場合**

- 96 キーの 1 周のスキャンは 8ms 前後と見積もっている。これに解析の時間を足して `SCAN_PERIOD_MS`（10ms）に収まることを期待するが、事前に厳密には確かめない
- たまに超える程度なら、上の「周期を超えたときの扱い」で対処できる。診断ページの `PERIOD_OVERRUN` を見て、**超えることが常態化するようなら、そのときに次の対処を考える**
- 収まらない場合は、Core1 の上でスキャンのタスクと解析のタスクを分ける
    - スキャンのタスクは、最新のフレームを `Signal` などで解析のタスクに渡す
    - 解析は、スキャンが I2C の完了を待っている間に進むので、全体が 1 周期に収まる
    - 代わりに、解析の処理中に I2C が完了すると、スキャンの再開が解析の分だけ遅れる（読み取りのタイミングが少しばらつく）

**ノートイベントの出力**

- `QubitTouch` の MIDI コールバックは、ノートイベントを `tasks::midi::queue_midi` で `MIDI_TX` に `try_send` する。今の `RefCell` のバッファ（`send_buffer` / `send_index`）は不要になる
- コールバックは `Fn + Clone` なので、動作モードは解析の直前に読んで `Cell` でコールバックに渡す
- チャンネルの決定（Piano: `MIDI_CH_FLOW`、Violin: `MIDI_CH_VIOLIN`）と、`RINGLED_CMD_TX_MOVED` を Note Off に読み替える処理は、`touch_task` 側で行ってから `MIDI_TX` に入れる。`midi_tx_task` は受け取ったものをそのまま送るだけにする
- キューがあふれたらエラーコード（22）に記録し、あふれた回数と最大使用数を診断用に数える

### 4.2 Core0: midi_tx_task

- `MIDI_TX: Channel<CriticalSectionRawMutex, MidiPacket, 16>` を受信し、`Sender` で送る。送信は `with_timeout(MIDI_TX_TIMEOUT_MS)` で包む
- `MidiPacket`（`[u8; 4]`）は USB MIDI の 4 バイトのパケット（CIN, status, data1, data2）
- エラー 23（MIDI 送信のタイムアウト）は改修前と同じく、タイムアウトのときだけ記録する（`write_packet` のエラーは記録しない）
- 書き手は Core1（`touch_task`）と Core0（`pressure_task`）。`CriticalSectionRawMutex` はコアをまたいでも排他が効き、受信側の起床もコアをまたいで働く
- USB が詰まっても、送信を待つのはこのタスクだけになり、タッチの解析は止まらない（P5 の解消）

### 4.3 Core0: pressure_task

- 現在の `adc_task` の処理に、`qubit_touch_task` の中にある CC11 の送信判定（`send_pressure_cc11_if_needed`）を移す
- CC11 と、Violin モードに入ったときの All Sound Off・CC11 の初期値は、`MIDI_TX` に入れる。`pressure.rs` の送信関数は、`Sender` を直接使う `async` 関数から、送るパケットを引数の関数に渡す同期関数（`pressure_cc11_if_needed`）に変え、`pressure_task` が `queue_midi` を渡す
- 改修前は送信に失敗すると、その回の CC の処理を途中で打ち切っていた。キューに入れる形では打ち切らない
- 圧力と CC11 は同じタスクで扱う方が、流れを追いやすい。10ms 周期は変えない
- CC11 の判定に使う `ANY_TOUCH` は Core1 が書く Atomic。コアをまたいでも問題ない
- 注意: Violin モードに入ったときの All Sound Off（Core0）と、その後のノート（Core1）は、同じ `MIDI_TX` を通るが、コアが違うので順序は厳密には保証されない。モードの切替は設定画面（`SETTING_MODE`、タッチしない前提）で行うので、実用上は問題ないと考える

### 4.4 Core0: ui_task と OLED

- スイッチの判定、ページ／モードの切替、OLED の描画を行う（現在の `core1_oled_ui_task` の内容）
- **OLED は I2C0（D6 = GP0 = SDA、D7 = GP1 = SCL）に移す**。ハードウェアの改修が必要（§5）
- 描画と転送が同じタスクになるので、コアをまたぐダブルバッファ（`BUFFER_FROM_DISPLAY` / `BUFFER_TO_DISPLAY`）は廃止する。描画 → `flush_buffer`（`with_timeout` 付き）を順に行う
- 描画は `await` を挟まない CPU 処理なので、Core0 の他のタスク（MIDI の送信など）を待たせる。タッチの解析は Core1 にあるので影響を受けない。描画時間を計測して公開し（§4.6）、MIDI の送信の遅れが問題になるなら次の順で対処する
    1. 表示内容が変わったときだけ描画・転送する
    2. 更新周期を下げる
    3. `midi_tx_task` を `InterruptExecutor`（優先度付き）に移す。`Cargo.toml` では `executor-interrupt` がすでに有効
- 起動画面の描画と転送も `ui_task` の最初に行う
- **OLED の初期化の前に 100ms 待つ**（`OLED_POWER_ON_WAIT_MS`）。改修前は、タッチセンサの初期化が終わってから OLED を初期化していたので、自然に待ち時間があった。I2C を分けると起動直後に並行して走るため、電源投入直後の OLED が初期化コマンドを受け付けられるように待つ
- **転送のタイムアウトは 50ms**（`OLED_FLUSH_TIMEOUT_MS`）。1 画面（1024 バイト）の転送は 400kHz で約 25ms かかる見積もりなので、余裕を持たせる。失敗・タイムアウトはエラー 43 に記録する
- **描画は 5fps（200ms 毎）、スイッチの判定は 100ms 毎**（`SWITCH_POLL_MS`、`DRAW_DIVIDER`）。実機で描画に平均約 3ms かかることが分かった（診断ページの `Drw`）。描画は `await` を挟まないので、その間 Core0 の他のタスクが止まる。頻度を下げて影響を減らす。スイッチの判定まで 200ms にすると短い押下を取りこぼしやすいので、判定は 100ms 毎のままにする。スイッチ操作でページや表示が変わったときは、次の描画を待たずにすぐ描く。点滅などに使う `counter` は 100ms 毎に進めるので、表示の時間は変わらない

#### 4.4.1 描画の分割（2026-10-01 決定、未実装。`debug_env` ブランチで行う）

**測定**: 診断ページ（page 5）を表示しているときの `Drw` は 最小 1215 / 平均 2780 / 最大 3087µs だった。診断ページはテキスト 7 行（FONT_6X10、130〜140 文字）だけなので、時間のほぼすべてがテキストの描画で、1 文字あたり約 20µs、1 行あたり約 400µs になる。embedded-graphics の MonoText は文字の画像を 1 ピクセルずつ `draw_iter` に渡し、`OledBuffer` の `DrawTarget` も 1 ピクセルずつ範囲を確かめて書くため、1 ピクセルに約 50 サイクルかかっている。塗りつぶしをまとめる（`fill_solid` の実装）だけでは、テキストが中心のページには効かない

**方針**: 描画の途中で、他のタスクに順番を譲る（`embassy_futures::yield_now()`）。1 回に Core0 を止める時間を、約 2.8ms から約 0.4ms（テキスト 1 行分）にする

- `GraphicsDisplay::tick` と、ページを描く関数（`draw_bringup_screen`, `display1`〜`display4`, `display_diag`）を `async fn` にし、**テキスト 1 行・図形 1 つを描くたびに `yield_now().await` を入れる**。`ui_task` は `gui.tick(&mut buffer, counter).await` で呼ぶ
- 描画のロジックは変えない。描く内容・順番・位置は今と同じにする
- バッファ（`OledBuffer`）は `ui_task` だけが持ち、全部描き終えてから転送するので、描きかけの画面が表示されることはない。描いている途中で共有状態（Atomic）が変わっても、行ごとに読んだ値が少しずれるだけで、表示として問題は無い
- デモのページ（10〜22）は開発用なので、関数は同期のままにし、`tick` の中で描画の前に 1 回だけ譲る
- 起動画面（`draw_bringup_screen`）は、`ui_task` の最初の 1 回もループの中の page 0 も、同じ async 関数を使う
- **計測**（`UI_DRAW_TIME`）: 譲っている間の他のタスクの時間を含めないよう、**譲らずに走った区間のうち最長のもの**（1 回に Core0 を止めた最長の時間）を記録する。この値が、MIDI の送信や RingLED の遅れの上限になるため。診断ページの表示（`Drw`）はそのままで、意味だけが「描画 1 回の時間」から「1 回に止めた最長の時間」に変わる
    - 区間の計測は、描画の関数の中で譲るたびに行う（譲る処理と計測を 1 つの小さな関数にまとめる）
- **確認**: 診断ページの `Drw` の最大が約 0.4ms（テキスト 1 行分）に下がること。各ページの表示が変わらないこと。エラー 45（RingLED の書き込みのタイムアウト）・23（MIDI 送信のタイムアウト）が出ないこと
- この改修で十分に短くなれば、`InterruptExecutor` への移行（§4.4 の 3、§8）は不要になる。テキストの描画そのものを速くする（文字を OLED のバッファの形式のまま書き込む専用の関数）のは、描画を 10fps に戻したいときなどに改めて考える

### 4.5 共有状態の整理（`src/shared.rs`）

`main.rs` に散らばっている static を `src/shared.rs` に集め、それぞれに「書き手・読み手・コア」をコメントで書く。

| 名前 | 型 | 書き手 | 読み手 | 備考 |
|---|---|---|---|---|
| `MIDI_TX` | `Channel<MidiPacket, 16>` | touch (C1), pressure (C0) | midi_tx (C0) | 新規。コアをまたぐ |
| `TOUCH0`–`TOUCH3` | `AtomicI32` | touch (C1) | ringled, ui (C0) | 変更なし |
| `ANY_TOUCH` | `AtomicBool` | touch (C1) | pressure (C0) | 変更なし |
| `PRESSURE` | `AtomicU32` | pressure (C0) | pressure, ui (C0) | 変更なし |
| `RINGLED_RX_BITS` | `[AtomicU32; RINGLED_RX_WORDS]` | midi_rx, ui（モード切替時の全消去）(C0) | ringled (C0) | **LED の数のビット列に拡張（§2.3）**。96 キーで 3 語 |
| `WORK_MODE` | `AtomicU8` | ui (C0) | touch (C1), pressure, midi_rx, ui (C0) | 変更なし |
| `SETTING_MODE` | `AtomicBool` | ui (C0) | touch (C1), pressure, ringled (C0) | 旧 `WORK_MODE_DISPLAY`。設定画面（基準値の補正中）であることを表すので、名前を意味に合わせる |
| `ERROR_CODE` | `AtomicU8` | 全タスク | status_led, ui (C0) | `src/error.rs` へ移し、外からは見えないようにする。`error::set` / `clear` / `get` で扱う（§4.7） |
| `POINT0`–`POINT5`, `DEBUG_VALUE`, `AD_VALUE*` | Atomic | 各タスク | ui (C0) | デバッグ表示用。`debug_env.md` の段階で PC 側に移し、整理する |
| `SCAN_TIME`, `ANALYSIS_TIME`, `UI_DRAW_TIME` | `TimeStat`（最小・平均・最大の Atomic の組） | touch (C1)、ui (C0) | ui (C0) | 新規。診断用（§4.6） |
| `PERIOD_OVERRUN`, `MIDI_TX_MAX_USED`, `MIDI_TX_OVERFLOW` | `AtomicU32` | touch (C1)、`queue_midi` の呼び出し元 | ui (C0) | 新規。診断用（§4.6） |

- `TOUCH_RAW_DATA` と `ELAPSED_TIME` は廃止する
- `Ordering::Relaxed` を基本とする方針は変えない

### 4.6 診断表示

改修の効果（周期の安定・遅れの減少）を確かめるため、OLED に診断ページを 1 つ用意する。デバッグ環境ができるまでは、これが唯一の確認手段になる。

- 置き場所は OLED の **page 5**。左右スイッチで巡回するページを 0→1→2→3→5→0 にする（改修前は 0→1→2→3→0）。4 は設定画面のまま
- 表示する内容（時間は us）
    - `touch_task`: スキャン時間と解析時間（それぞれ最小・平均・最大）、周期を超えた回数
    - `ui_task` の描画時間（転送は含まない。最小・平均・最大）
    - `MIDI_TX` のキューの最大使用数、あふれた回数
    - エラーコード
- 平均は 1/16 の指数移動平均。最小・最大と回数は、設定画面に入ったとき（エラーコードのクリアと同時）に `reset_diagnostics()` でリセットする
- 段階 5 では診断ページがまだ無いので、`PERIOD_OVERRUN` を page 3 に仮に表示し、段階 6 で診断ページに移す

### 4.7 エラーコード（`src/error.rs`）

- `ERROR_CODE` と、コードの定数（例: `pub const ADC_READ: u8 = 32;`）を `src/error.rs` に集める。数値の直書きをやめる（P8）
- 記録・消去・読み出しは `error::set(error::XXX)` / `error::clear()` / `error::get()` で行う。panic ハンドラは `error::PANIC`（55）を書く
- 改修前の番号との互換は取らず、全体を振り直す。**十の位は機能の分類、一の位はその中の番号とし、どちらも 1–5 の範囲にする**（点滅の回数を数えやすくするため）
- USB・MIDI 系の 3 つのタスク（usb, midi_tx, midi_rx）の起動失敗は 1 つのコードにまとめる。起動失敗はタスクのプールが足りないなどの作り込みの誤りで、起動直後に必ず起きるので、どのタスクかはコードを見れば分かる

| コード | 分類 | 意味 | 定数 |
|---|---|---|---|
| 11 | タッチ | touch_task の起動に失敗（Core1） | `SPAWN_TOUCH` |
| 12 | タッチ | タッチセンサ初期化タイムアウト | `TOUCH_INIT_TIMEOUT` |
| 21 | USB・MIDI | usb_task / midi_tx_task / midi_rx_task の起動に失敗 | `SPAWN_USB_MIDI` |
| 22 | USB・MIDI | `MIDI_TX` のキューあふれ | `MIDI_TX_QUEUE_FULL` |
| 23 | USB・MIDI | MIDI 送信のタイムアウト（USB 未接続など） | `MIDI_TX_TIMEOUT` |
| 24 | USB・MIDI | MIDI 受信エラー | `MIDI_RX` |
| 25 | USB・MIDI | （予約: debug_stream_task の起動に失敗。`doc/debug_env.md`） | ― |
| 31 | 圧力 | pressure_task の起動に失敗 | `SPAWN_PRESSURE` |
| 32 | 圧力 | ADC 値の取得エラー（タイムアウトを含む） | `ADC_READ` |
| 41 | 表示 | ui_task の起動に失敗 | `SPAWN_UI` |
| 42 | 表示 | OLED 初期化エラー | `OLED_INIT` |
| 43 | 表示 | OLED 転送エラー（タイムアウトを含む） | `OLED_FLUSH` |
| 44 | 表示 | ringled_task の起動に失敗 | `SPAWN_RINGLED` |
| 45 | 表示 | RingLED への書き込みのタイムアウト | `RINGLED_WRITE_TIMEOUT` |
| 51 | システム | status_led_task の起動に失敗（LED では表示できないので、OLED の診断ページで確認する） | `SPAWN_STATUS_LED` |
| 55 | システム | panic（`status_led_task` が Core0 にあるため、Core0 で panic したときは LED では表示できない。§8） | `PANIC` |

改修前のコード（11, 12, 21, 23, 43, 53 など）は、ダブルバッファの廃止やタスクの統合で意味を失ったので、対応は取らない。

コードの一覧は `src/error.rs` の定数と、その先頭のコメントを正とする。

### 4.8 ソースの配置

```
src/
├── main.rs            初期化・ペリフェラルの割り当て・両コアの起動とタスクの spawn だけ
├── shared.rs          共有状態（§4.5）
├── error.rs           エラーコード（§4.7）
├── constants.rs       変更なし（SCAN_PERIOD_MS などを追加）
├── tasks/             タスク本体（1 タスク 1 ファイルを基本とする）
│   ├── mod.rs
│   ├── touch.rs       touch_task (Core1)。ロジックは touch/read_touch.rs, touch/qtouch.rs
│   ├── midi.rs        midi_tx_task, midi_rx_task
│   ├── pressure.rs    pressure_task（ロジックは touch/pressure.rs）
│   ├── ringled.rs     ringled_task（ロジックは ui/ringled.rs）
│   ├── ui.rs          ui_task（ロジックは ui/oled_display.rs）
│   └── status_led.rs
├── devices/           変更なし
├── touch/             ロジックのみ（変更は最小限）
└── ui/                ロジックのみ
```

- タスク（`#[embassy_executor::task]`）とロジック（計算・描画）を分ける。ロジックの側は、どのコア・どのタスクから呼ばれるかを知らない
- `usb_task` は小さいので `main.rs` に残してもよい

### 4.9 スタックとメモリ

- Core1 のスタック: **`CORE1_STACK_SIZE` を 8KB → 16KB にする**。`QubitTouch` は約 3.7KB、`ReadTouch` は約 0.6KB ある（コンパイル時に確認）。embassy のタスクの状態（future）は static に置かれるが、初期化のときに一時的にスタックへ置かれることがあるため、余裕を持たせる。RP2350 の RAM（520KB）に対しては小さい
- Core0 のスタック（`memory.x` の `_stack_size` = 8KB）: 同様。OLED のバッファ（1KB）は 2 つから 1 つに減る

## 5. ハードウェアの改修

OLED を I2C0（D6 = GP0 = SDA、D7 = GP1 = SCL）に移す。詳細は `doc/hw_modify.md` に分けて記述する。

- 段階 3 より前に改修を済ませる。改修前の基板では、段階 3 以降のファームの OLED は表示されない（エラー 42）
- 改修後の基板では、逆に `main` ブランチのファームの OLED が表示されなくなる（`doc/hw_modify.md` §5）

## 6. 作業の段階

各段階の終わりに、次を確認してコミットする。

- ビルド: `cargo build`, `--release`, `--features test_mode`, `--features no_pca9544`, `cargo clippy --all-features -- --deny=warnings`, `cargo fmt --check`
- 実機: 下の「確認」欄

| 段階 | 内容 | 確認 |
|---|---|---|
| 1 | **不具合の修正**: `RINGLED_RX_BITS` を 96 ビットにする（§2.3） | 受信ノートの LED が 1 箇所だけ光る。dev ビルドでも panic しない |
| 2 | **ファイルの分割**: `shared.rs`, `error.rs`, `tasks/` を作り、タスクを移す。動作は一切変えない | 改修前と同じに動く |
| 3 | **I2C の分離**（ハードウェアの改修後）: I2C1 を Core1 で生成、OLED を I2C0 へ、`ui_task` と `status_led_task` を Core0 へ。ダブルバッファの廃止 | OLED の表示、スイッチの操作、設定画面、エラーの点滅 |
| 4 | **MIDI 送信の分離**: `MIDI_TX` と `midi_tx_task`。CC11 を `pressure_task` へ。解析はまだ Core0 のまま | ノートの送信、Violin モードの CC11・All Sound Off。USB を抜き差ししてもタッチの解析が止まらない |
| 5 | **解析の Core1 への移動と周期化**: `touch_task`（`Ticker`、スキャン → 解析、周期を超えたときの処理）。`TOUCH_RAW_DATA` の廃止。`PERIOD_OVERRUN` を OLED の既存のページに仮に表示する（診断ページは段階 6） | タッチの反応が改修前と同等以上（演奏して確かめる）。`PERIOD_OVERRUN` が増え続けていないこと |
| 6 | **診断と名前の整理**: 診断ページ、`SETTING_MODE` への名前変更、エラーコードの振り直し、CLAUDE.md の更新 | 診断ページの表示。CLAUDE.md が新しい構成と一致している |

- 段階 1 は他と独立しているので、`main` から分岐した `fix_ringled_rx_bits` ブランチで行い、`main` に入れて本番用のファームにも反映する。`task_architecture` ブランチには、その後で `main` から取り込む
- 段階 2 は差分が大きいが、中身は移動だけにする。レビューで「移動以外の変更が無い」ことを確かめやすくするため
- 段階 4 で先に `MIDI_TX` を作っておくと、段階 5 で解析を Core1 に移すときに、MIDI の出口を変えずに済む

### 6.1 実装の記録

各段階を実装したときに、設計から補ったこと・途中の状態を記録する。

**段階 1**（`fix_ringled_rx_bits` → `main`）

- `RINGLED_RX_BITS` を `[AtomicU32; RINGLED_RX_WORDS]` にした。`RINGLED_RX_WORDS = NUM_LEDS.div_ceil(32)` なので、`test_mode`（6 キー）では 1 語になる
- 不具合は 2026-05-27 の `8671a50`（RingLED をイベント駆動から共有状態の参照に変えたとき）で入った。それ以前は `[bool; NUM_LEDS]` で正しく扱えていた

**段階 2**

- タスクの関数名は変えずに `tasks/` に移した。移したタスクの本体は、元の `main.rs` と一字一句同じ（スクリプトで照合）
- 途中のファイル名: `qubit_touch_task` は `tasks/touch.rs`、`adc_task` は `tasks/pressure.rs`、`core1_i2c_task` は `tasks/core1_i2c.rs`（段階 3 で `touch_scan.rs` に改名）
- `ringled_task` が `Irqs` を使うので、`bind_interrupts!` の `Irqs` を `pub` にした
- `usb_task` は `main.rs` に残した
- `error.rs` には `ERROR_CODE` と一覧のコメントだけを移した。コードの定数化は段階 6 で行う

**段階 3**（ハードウェアの改修前に実装した。実機では未確認）

- `core1_i2c_task` を `touch_scan_task`（`tasks/touch_scan.rs`）に改名し、タッチのスキャンだけにした。段階 5 で解析と合わせて `touch_task` にする
- `core1_oled_ui_task` → `ui_task`、`core1_led_task` → `status_led_task` に改名し、Core0 に移した
- I2C0・I2C1 とも 400kHz で、同じ `I2cConfig` を使う（`Copy`）
- OLED の電源安定待ちと転送のタイムアウトを追加した（§4.4）
- エラーコードは段階 6 の振り直しまでの仮の割り当てにした

| コード | 段階 3 時点の意味 |
|---|---|
| 11, 12, 21, 23, 53 | 廃止（ダブルバッファ・Core1 の LED/UI タスク） |
| 22 | touch_scan_task（Core1）の起動に失敗 |
| 36 | ui_task の起動に失敗 |
| 37 | status_led_task の起動に失敗 |
| 52 | OLED 転送エラー（タイムアウトを含む） |

**段階 4**

- `MIDI_TX` の要素は USB MIDI の 4 バイトのパケット（`MidiPacket = [u8; 4]`）。送る側は `tasks::midi::queue_midi` で `try_send` する
- `pressure.rs` の CC 送信は、USB を直接使う `async` 関数から、送るパケットを引数の関数に渡す同期関数（`pressure_cc11_if_needed`）に変えた。以前は送信に失敗すると `?` でその回の処理を打ち切っていたが、キューに入れる形では打ち切らない
- `midi_tx_task` のエラー 42（現在の番号では 23）は、改修前と同じくタイムアウトのときだけ記録する（`write_packet` のエラーは記録しない）
- 段階 4 の時点では、解析（`qubit_touch_task`）は Core0 のままで、コールバックからキューに入れる形にした

**段階 5**

- `QubitTouch` は約 3.7KB、`ReadTouch` は約 0.6KB（コンパイル時に確認）。初期化のときに一時的にスタックへ置かれることがあるため、**`CORE1_STACK_SIZE` を 8KB → 16KB にした**（§4.9 の想定から変更）
- `read_touch::touch_sensor_scan` は、結果を引数の配列に書き込む形にした（`TOUCH_RAW_DATA` を廃止）
- `SCAN_PERIOD_MS`・`ANALYSIS_PERIOD_MS`・`ANALYSIS_DIVIDER` は `constants.rs` に置き、`ANALYSIS_PERIOD_MS` が `SCAN_PERIOD_MS` の倍数であることをコンパイル時に確認する
- 動作モードは解析の直前に読み、コールバックへは `Cell` で渡す（コールバックは `Fn + Clone`）

**段階 6**

- 診断ページは OLED の **page 5** に置いた。左右スイッチで巡回するページを 0→1→2→3→5→0 にした（改修前は 0→1→2→3→0）。4 は設定画面のまま
- 診断ページの内容: スキャン・解析・描画の時間（us、最小/平均/最大）、周期超過回数、`MIDI_TX` の最大使用数/あふれた回数、エラーコード。平均は 1/16 の指数移動平均
- 最小・最大と回数は、設定画面に入ったとき（エラーコードのクリアと同時）にリセットする
- 計測値は `shared.rs` の `TimeStat`（最小・平均・最大の組）にまとめた。`ELAPSED_TIME` は廃止した
- エラーコードは `error.rs` の定数にし、`error::set` / `clear` / `get` で扱う。`ERROR_CODE` 自体は `error.rs` の外から見えないようにした
- その後、改修前の番号との互換は不要として、十の位・一の位とも 1–5 の範囲に振り直した（§4.7）。panic は 255 から 55 に変えた

**動作チェック後の修正**（2026-09-30）

- 実機でエラー 45（RingLED 書き込みのタイムアウト）が出た。リング LED を付けていない状態でも、PIO・DMA の送信は同じように行われる
- 原因: 段階 3 で OLED の描画（`ui_task`）が Core0 に移り、描画（平均約 3ms、`await` なし）の間 `ringled_task` が動けない。96 LED の送信（約 3.9ms）の完了の処理が遅れ、8ms のタイムアウトを超えた
- 対策:
    - RingLED の書き込みのタイムアウトを 8ms → 15ms にした（`RINGLED_WRITE_TIMEOUT_MS`）。周期 (20ms) に収まり、固着したときの保護という目的は変わらない
    - OLED の描画・転送を 10fps → 5fps にした。スイッチの判定は 100ms 毎のまま（§4.4）
- MIDI の送信が描画の分だけ遅れる件は残っている（§8）
- 続いて、起動直後からエラー 23（MIDI 送信のタイムアウト）が出た。診断ページの `MIDIq` は最大 8 / あふれ 0 で、起動時に 8 個ほどのパケットがまとめて作られていた
- 原因: 最初のスキャンのフレームは基準値 0 で引かれるため全キーが大きな値になり、段階 5 で解析をスキャンの直後に行うようにしたことで、これを `QubitTouch` が確実に拾ってノートを出していた。起動直後は PC が USB を認識している途中で、USB MIDI の送信は前のパケットをホストが受け取るまで次を書けないため、2 つ目以降がタイムアウトした
- 対策: 起動後 `TOUCH_STARTUP_SETTLE_MS`（500ms）は解析をしない（§4.1）。起動時の誤ったノートも出なくなる

## 7. 他の設計書への影響

- `doc/debug_env.md`
    - OLED は Core0 の I2C0 に移るので、`debug_stream` のときに OLED を止める必要は無い
    - デバッグ用のフレームは `touch_task`（Core1）で作り、送信タスクは Core0。CDC の追加は `main.rs` の USB の初期化部分に入れる
    - スキャン周期を 2ms にしても、解析は 10ms 毎（`ANALYSIS_DIVIDER`）なので、`QubitTouch` の振る舞いは変わらない
- `doc/touch_baseline.md`
    - OLED 転送の分割は不要になる
    - 信号処理（`TouchSignal`）は `touch_task` の中で、スキャンと解析の間に入る。結果はコアをまたがず、関数呼び出しで `QubitTouch` に渡す
    - 毎フレームの解析（フレーム駆動）と `QubitTouch` の時間基準化は、`touch_baseline.md`（速さ検出）の段階で扱う

## 8. 未決事項

- 96 キー構成で、スキャン＋解析が 10ms を超えることが常態化するか。段階 5 以降、`PERIOD_OVERRUN` を見て、常態化するようなら §4.1 のとおりタスクを分けるなどの対処を考える
- OLED の描画時間が Core0 の MIDI 送信に与える影響。段階 3 以降、MIDI の送信の遅れなど問題が見えたら §4.4 の対処を行う
- `InterruptExecutor` を使うかどうか（§4.4）
- **Core0 で panic したときの表示**: 段階 3 で `status_led_task` を Core0 に移したため、Core0 で panic すると LED の点滅も止まり、エラー 55 を表示できない（Core1 の panic は Core0 の LED で 55 と表示される）。改修前は LED のタスクが Core1 にあったので、Core0 の panic も表示できた。対策としては、panic ハンドラの中で LED を直接点滅させる（Executor に頼らないビジーループ）などが考えられる
- **OLED の描画による Core0 のタスクの遅れ**（→ 描画の分割で対処する。§4.4.1）: 描画に平均約 3ms かかり、その間 `midi_tx_task` と `ringled_task` が待たされる。5fps にしたので頻度は半分になったが、MIDI の送信が最大で描画時間の分だけ遅れることは変わらない。根本的には、`midi_tx_task` と `ringled_task` を `InterruptExecutor`（優先度付き）で動かし、描画の途中でも割り込めるようにする（§4.4 の 3）。描画処理そのものを速くする（`OledBuffer` の `DrawTarget` で塗りつぶしをまとめて処理する、など）余地もある
- `pressure_task`（ADC）を Core1 に置く案。センシングをまとめる考え方もあるが、Core1 の周期を乱す要因を増やさないため、今回は Core0 に置く
