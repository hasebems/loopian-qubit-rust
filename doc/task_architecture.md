# コア・タスク構成 設計書

ファームのコアとタスクの分け方、タスク間の受け渡し、診断とエラーコードの約束事を書く。

## 1. 目的

- タッチの処理（スキャン → 信号処理 → 解析 → ノートイベント）を、**一定周期で、他の処理に乱されずに** 行う。触れる速さ・ベロシティなど、時間を基準にした解析の前提になる
- OLED の転送・USB・リング LED などの入出力が、タッチのスキャンを止めないようにする
- タスクと共有状態の役割（誰が書き、誰が読むか）を、コードからすぐ読み取れるようにする

## 2. コアの役割

| | Core0: 入出力 | Core1: タッチ |
|---|---|---|
| 役割 | USB・MIDI・圧力・リング LED・OLED・スイッチ・内蔵 LED | タッチのスキャン → 信号処理 → 解析 → ノートイベントの生成 |
| タスク | midi_tx、midi_rx、usb、pressure、ringled、ui、status_led、debug_stream（feature） | touch |
| バス | PIO の I2C（GP3/GP4、PIO1）: OLED・PCF8574（スイッチ） | I2C1（GP6/GP7）: PCA9544 / AT42QT1070。I2C0（GP0/GP1）もセンサーにする（`hw96_firmware261114.md` §5） |
| 割り込み | USB, ADC, PIO0, PIO1, DMA | I2C1（Core1 で `new_async` を呼ぶ） |

- Core1 にはタッチの処理だけを置く。スキャンした直後に同じコアで信号処理と解析を行うので、スキャンと解析の周期がずれず、Core0 の処理に待たされることも無い
- 各コアは独立した Embassy の `Executor`（協調型）を持つ
- embassy-rp は、`new_async`（I2C）や `Pio::new` を呼んだコアの NVIC で割り込みを有効にする。I2C1 は Core1 の中で生成し、割り込みも Core1 で処理させる。`bind_interrupts!` の `Irqs` は両コアで共通
- RP2350 の 2 つのコア（Cortex-M33）はどちらも FPU を持つので、Core1 で f32 の計算をしてよい
- `pressure_task`（ADC）は Core0 に置く。Core1 の周期を乱す要因を増やさないため

## 3. タスクとデータの流れ

### 3.1 タスク一覧

| タスク | コア | 周期・起動 | 内容 |
|---|---|---|---|
| `touch_task` | 1 | `Ticker`（`scan_period_us()`、既定 10ms） | I2C1 を専有。タッチセンサの初期化、スキャン、信号処理（`touch_algo::electrode`）、解析（`TouchTracker` → `PlayInfo`）。ノートイベントと Expression（CC11）を `MIDI_TX` に入れ、タッチ位置を `TOUCH0-3` に公開（§4.1） |
| `midi_tx_task` | 0 | `MIDI_TX` の受信で起動 | USB MIDI の `Sender` を専有し、送る（§4.2） |
| `midi_rx_task` | 0 | USB の受信で起動 | USB MIDI を受信する。受信ノートを `RINGLED_RX_BITS` に反映する処理は止めている（`SHOW_RX_NOTES` = false） |
| `usb_task` | 0 | 常駐 | USB デバイスの駆動（`main.rs`） |
| `pressure_task` | 0 | 10ms | ADC 3ch の読み取りと圧力の計算。Bow Pressure（CC80）を `MIDI_TX` に入れる（§4.3） |
| `ringled_task` | 0 | `Ticker` 20ms | `TOUCH0-3` と `RINGLED_RX_BITS` を読んで NeoPixel を描画。書き込みのタイムアウトは 15ms |
| `ui_task` | 0 | スイッチ 100ms、描画 200ms（5fps） | スイッチ（PCF8574）の判定、ページの切替、OLED の描画と転送（§4.4） |
| `status_led_task` | 0 | 常駐 | 内蔵 LED（ハートビート／エラーコードの点滅） |
| `debug_stream_task` | 0 | 常駐（`debug_stream` feature のみ） | タッチの生値と Note のイベントを USB CDC で PC に送り、PC のコマンドを受ける（`debug_env.md`） |

### 3.2 データの流れ

```
Core1                                        Core0
touch_task                                   midi_tx_task ──► USB MIDI
  スキャン (I2C1)                                 ▲
    ▼                                            │ MIDI_TX (Channel、コアをまたぐ)
  信号処理 (touch_algo::electrode)                │
    ▼                                            ├────────── pressure_task ◄── ADC
  解析 (TouchTracker → PlayInfo) ──ノート・CC11──┘     (CC80)
    │                                                  ▲ ANY_TOUCH
    ├── TOUCH0-3, ANY_TOUCH ──────────────────────────┴──► ringled_task ◄── RINGLED_RX_BITS ◄── midi_rx_task
    │
    ├── POINT0-5, DEBUG_VALUE, TOUCH_CALIBRATING ──────────► ui_task (OLED)
    ▲
    └── SETTING_MODE ◄──────────────────────────────────── ui_task (スイッチ)
```

コアをまたぐのは `MIDI_TX`（キュー）と Atomic だけ。タッチの生データはコアをまたがない（`debug_stream` の `DEBUG_FRAMES` を除く）。

## 4. 詳細

### 4.1 Core1: touch_task

**1 周期の処理**

```
loop {
    ticker.next().await;                         // scan_period_us() 毎
    read_touch.touch_sensor_scan(...).await;     // 全キーを読む（I2C の完了は await で待つ）
    if 解析する時刻なら {
        signal.process(時刻, 生値, 読めたか, SETTING_MODE);   // 信号処理（ノイズ除去・基準値）
        tracker.set_value(...); tracker.update(時刻);          // depth・position と追跡
        play.update(&tracker);                                 // Note On/Off・Expression・ビブラート
        共有状態（TOUCH0-3 など）を書く
    }
    時間を計測して診断用の Atomic に記録する
}
```

- `ReadTouch`・`ElectrodeSignal`・`TouchTracker`・`PlayInfo` は同じタスクの中の構造体で、値は関数呼び出しで渡す
- スキャン周期の既定は `SCAN_PERIOD_US`（10ms）。`debug_stream` feature では既定 2ms で、PC の `period` コマンドで 2 / 5 / 10 / 20ms に変えられる。周期が変わったら `Ticker` を作り直す
- **解析する時刻**: 信号処理と解析は、前の解析から `ANALYSIS_PERIOD_US`（10ms）経ったフレームで行う。`ANALYSIS_TOLERANCE_US`（1ms）まで早くてもよい（2ms 周期のフレームの揺れを吸収する）。遅れたときは、次の時刻を `ANALYSIS_PERIOD_US` 先にして間隔を保ち、1 周期以上遅れたら今のフレームから数え直す。フレームの数ではなく時刻で決めるので、スキャンが周期を超えてフレームの間隔が延びても、解析の間隔は平均 10ms に保たれる
- `touch_algo` の時間（指を保持する時間・Note On までの時間・ビブラート）は、周期の数ではなく実時間（`TouchTracker::update` に渡す µs）で数える（`touch_analysis.md` §3.8）
- **起動直後**: `TOUCH_STARTUP_SETTLE_MS`（500ms）の間は、チップが自身を校正しているので、スキャンだけを行い、信号処理にも解析にも渡さない。その後、`touch_algo` が基準値の初期取得と noise_floor の計測を行う（`touch_baseline.md` §3.11・§3.12）。終わるまで `TOUCH_CALIBRATING` が true
- **設定画面**: `SETTING_MODE` の間は、`ElectrodeSignal::process()` の `calibrating` を true にして、`touch_algo` の校正（基準値の速い追従と noise_floor の計測）を行う（`touch_baseline.md` §3.7）
- **初期化**: タッチセンサの初期化は `TOUCH_INIT_TIMEOUT_MS`（150ms）で打ち切る。失敗しても全チャネルを続け、エラー 12 を記録する

**周期を超えたときの扱い**

- embassy-time の `Ticker::next()` は、期限を過ぎていると待たずにすぐ完了し、期限を 1 周期だけ進める。そのままだと、1 回の処理が周期を超えたとき、遅れた周期分を待たずに連続して処理してしまう
- そのため、**遅れた周期は取り戻さずに捨てる**。処理の後で経過時間が周期を超えていたら、`PERIOD_OVERRUN` を数えて `ticker.reset()` を呼び、期限を「今＋1 周期」にし直す。周期を超えた回は、その周期が延びるだけになる
- 回数は診断ページ（page 5）に出す

**1 周期に収まらない場合**

- たまに超える程度なら、上の扱いで足りる。`PERIOD_OVERRUN` が増え続けるようなら、Core1 の上でスキャンのタスクと解析のタスクを分ける
    - スキャンのタスクは、最新のフレームを `Signal` などで解析のタスクに渡す
    - 解析は、スキャンが I2C の完了を待っている間に進むので、全体が 1 周期に収まる
    - 代わりに、解析の処理中に I2C が完了すると、スキャンの再開が解析の分だけ遅れる（読み取りのタイミングが少しばらつく）

**ノートイベントの出力**

- `PlayInfo` の MIDI コールバック（`on_play_event`）が、ノートイベントと CC を `tasks::midi::queue_midi` で `MIDI_TX` に `try_send` する。待たないので、USB が詰まってもタッチの処理は止まらない
- チャンネルは `MIDI_CH_VIOLIN`（ch 2）。ノートの移動（`STATUS_NOTE_MOVED`）は Note Off として送る
- キューがあふれたらエラー 22 を記録し、あふれた回数と最大使用数を診断用に数える

### 4.2 Core0: midi_tx_task

- `MIDI_TX: Channel<CriticalSectionRawMutex, MidiPacket, 16>` を受信し、`Sender` で送る。送信は `with_timeout(MIDI_TX_TIMEOUT_MS)`（20ms）で包み、タイムアウトのときだけエラー 23 を記録する
- `MidiPacket`（`[u8; 4]`）は USB MIDI の 4 バイトのパケット（CIN, status, data1, data2）
- 書き手は Core1（`touch_task`）と Core0（`pressure_task`）。`CriticalSectionRawMutex` はコアをまたいでも排他が効き、受信側の起床もコアをまたいで働く
- USB への送信はこのタスクだけが行う。送信を待つのもこのタスクだけになる

### 4.3 Core0: pressure_task

- 10ms 毎に ADC 3ch を DMA で読み（タイムアウト 20ms、失敗はエラー 32）、移動平均の基準値と圧力を計算する（`touch/pressure.rs`）。設定画面の間は基準値を補正する
- Bow Pressure（CC80）は、送るパケットを `queue_midi` に渡して `MIDI_TX` に入れる（最初の 1 回は初期値）。圧力センサを ADC につなぐまでは送らない（`pressure.rs` の `BOW_PRESSURE_ENABLED` = false）
- `ANY_TOUCH`（Core1 が書く Atomic）を参照する

### 4.4 Core0: ui_task と OLED

- スイッチの判定、ページの切替、OLED の描画と転送を行う。OLED とスイッチの PCF8574 は同じ PIO の I2C（GP3/GP4）にあり、`ui_task` がこのバスを専有して順番に使う（`hw96_firmware261114.md` §3・§4）
- 描画と転送が同じタスクにあるので、バッファ（`OledBuffer`）は 1 つ。描画 → `flush_buffer`（`with_timeout` 付き）を順に行う。全部描き終えてから転送するので、描きかけの画面は表示されない
- 起動画面の描画と転送は `ui_task` の最初に行う
- **OLED の初期化の前に 100ms 待つ**（`OLED_POWER_ON_WAIT_MS`）。起動直後は他のタスクと並行して走るので、電源投入直後の OLED が初期化コマンドを受け付けられるように待つ。初期化の失敗・タイムアウト（50ms）はエラー 42
- **転送のタイムアウトは 50ms**（`OLED_FLUSH_TIMEOUT_MS`）。1 画面（1024 バイト）の転送は 400kHz で約 25ms かかるので、余裕を持たせる。失敗・タイムアウトはエラー 43
- **描画は 5fps（200ms 毎）、スイッチの判定は 100ms 毎**（`SWITCH_POLL_MS`、`DRAW_DIVIDER`）。描画の頻度を下げて Core0 の他のタスクへの影響を減らす。スイッチの判定まで 200ms にすると短い押下を取りこぼしやすいので、判定は 100ms 毎にする。スイッチ操作でページや表示が変わったときは、次の描画を待たずにすぐ描く。点滅などに使う `counter` は 100ms 毎に進めるので、表示の時間は描画の頻度に関係しない
- **スイッチ**: PCF8574 を 1 回読み、P6（左）・P7（右）のビットから押下（Low）を取り出す。読み込みのタイムアウトは 5ms（`SWITCH_READ_TIMEOUT_MS`）。初期化・読み込みの失敗はエラー 52 で、読めなかった回はスイッチを前回の状態のままにする（「押されていない」にすると、押し続けているスイッチが、読めるようになった回に押し直したように見えるため）
- **ページ**: 右で次、左で前のページ。巡回は 0→1→2→3→5→6→0（5 は診断、6 は I2C 診断）。左右の同時押しで設定画面（page 4、`SETTING_MODE` = true）に入り、エラーコードと診断値をクリアする。設定画面は右で抜ける

#### 4.4.1 描画の分割

- 描画は `await` を挟まない CPU 処理で、その間 Core0 の他のタスク（MIDI の送信・リング LED）が止まる。テキストの描画は 1 文字あたり約 28µs かかり（embedded-graphics の MonoText が 1 ピクセルずつ `draw_iter` に渡すため）、1 画面を一度に描くと約 3ms 止まる
- そこで、**テキスト 1 行・図形 1 つを描くたびに、他のタスクに順番を譲る**（`DrawPacer`。`ui/oled_display.rs`）。描画の部品は `draw(buffer, pacer, &item)`（1 つ描いて譲る）で描く。`GraphicsDisplay::tick` と各ページの描画は `async fn`
- デモのページ（10〜22）は開発用なので同期のままにし、描く前に 1 回だけ譲る
- 描いている途中で共有状態（Atomic）が変わっても、行ごとに読んだ値が少しずれるだけで、表示として問題は無い
- **計測**（`UI_DRAW_TIME`）: 譲らずに走った区間のうち最長のもの（1 回に Core0 を止めた最長の時間）を記録する。この値が、MIDI の送信やリング LED の遅れの上限になるため。診断ページの `Drw` に出す。実機では最大約 0.9ms（診断ページで最も長い 21 文字の行）
- `InterruptExecutor`（`midi_tx_task` を優先度付きで動かす）は使わない。1 回に止める時間が十分に短いため。描画を 10fps に戻すなど、止める時間が再び問題になったら、長い行を区切って譲る・テキストを速く描く専用の関数を作る・`InterruptExecutor` に移す、を考える

### 4.5 共有状態（`src/shared.rs`）

共有状態は `src/shared.rs` に集め、それぞれに「書き手・読み手」をコメントで書く。基本は `portable_atomic` の Atomic（`Ordering::Relaxed`）で、各タスクは共有状態をポーリングして独立した周期で動く。イベントのキューで知らせ合わないので、MIDI のイベントが集中したりキューが詰まったりしても、リング LED などの表示はその影響を受けず、周期も各タスクで決められる。キューは MIDI の送信（`MIDI_TX`）だけに使う。

| 名前 | 型 | 書き手 | 読み手 | 備考 |
|---|---|---|---|---|
| `MIDI_TX` | `Channel<MidiPacket, 16>` | touch (C1), pressure (C0) | midi_tx (C0) | コアをまたぐ |
| `TOUCH0`–`TOUCH3` | `AtomicI32` | touch (C1) | ringled, ui (C0) | タッチ位置 ×100（0–9999）、10000 は未タッチ |
| `ANY_TOUCH` | `AtomicBool` | touch (C1) | pressure (C0) | |
| `POINT0`–`POINT5` | `AtomicU16` | touch (C1) | ui (C0) | キー 0〜5 の `touch_algo` の output |
| `DEBUG_VALUE` | `AtomicU32` | touch (C1) | ui (C0) | ビブラートの速さ（Hz ×10） |
| `TOUCH_CALIBRATING` | `AtomicBool` | touch (C1) | ui (C0) | 起動時の校正中。一度 false にしたら戻さない |
| `PRESSURE`, `AD_VALUE*` | `AtomicU32` | pressure (C0) | ui (C0) | |
| `RINGLED_RX_BITS` | `[AtomicU32; RINGLED_RX_WORDS]` | midi_rx (C0) | ringled (C0) | 1 ビット/LED。LED n は `[n / 32]` の `n % 32` ビット目 |
| `SETTING_MODE` | `AtomicBool` | ui (C0) | touch (C1), pressure (C0) | 設定画面（基準値の補正中） |
| `ERROR_CODE` | `AtomicU8` | 全タスク | status_led, ui (C0) | `src/error.rs` の中だけ。`error::set` / `clear` / `get` で扱う（§4.7） |
| 診断値（§4.6） | `TimeStat`, `AtomicU32` | touch (C1), ui (C0), `queue_midi` の呼び出し元 | ui (C0) | |
| `DEBUG_FRAMES`, `DEBUG_EVENTS` など | `Channel`, Atomic | touch (C1), debug_stream (C0) | debug_stream (C0), touch (C1) | `debug_stream` feature のみ（`debug_env.md`） |

### 4.6 診断表示

OLED の診断ページで、周期の安定や処理の遅れを確かめる。時間は µs、平均は 1/16 の指数移動平均（`TimeStat`）。最小・最大と回数は、設定画面に入ったとき（エラーコードのクリアと同時）に `reset_diagnostics()` でリセットする。

- **page 5（診断）**
    - `touch_task`: スキャン時間（`SCAN_TIME`）と解析時間（`ANALYSIS_TIME`）の最小・平均・最大、周期を超えた回数（`PERIOD_OVERRUN`）
    - `ui_task`: 描画で 1 回に Core0 を止めた最長の時間（`UI_DRAW_TIME`、`Drw`。§4.4.1）
    - `MIDI_TX` のキューの最大使用数（`MIDI_TX_MAX_USED`）、あふれた回数（`MIDI_TX_OVERFLOW`）
    - エラーコード
- **page 6（I2C 診断）**: スキャン時間の内訳
    - I2C1 の通信 1 回の時間: PCA9544 の ch の選択（`I2C_SELECT_TIME`）、AT42QT1070 の 6 キーの読み取り（`I2C_READ_TIME`）、PCA9544 の切断（`I2C_DISCONNECT_TIME`）
    - hi/lo ずれの読み直しの回数（`I2C_REREAD_COUNT`）、失敗の回数（`I2C_FAIL_COUNT`）
    - OLED・スイッチの PIO の I2C で、バスを戻した回数（`UI_BUS_RECOVERY_COUNT`、`UI bus Rcv`）

### 4.7 エラーコード（`src/error.rs`）

- パニックさせない方針で、失敗は `error::set(error::XXX)` で 2 桁のコードを記録する。`status_led_task` が内蔵 LED を十の位 → 一の位の回数だけ点滅させる
- **十の位は機能の分類、一の位はその中の番号とし、どちらも 1–5 の範囲にする**（点滅の回数を数えやすくするため）
- タスクの起動の失敗は、作り込みの誤り（タスクのプールが足りないなど）で起動直後に必ず起きる。USB・MIDI 系の 3 つのタスク（usb, midi_tx, midi_rx）は 1 つのコードにまとめる
- panic ハンドラは 55 を書く。ただし `status_led_task` は Core0 にあるので、Core0 で panic したときは LED では表示できない（Core1 の panic は表示できる）

| コード | 分類 | 意味 | 定数 |
|---|---|---|---|
| 11 | タッチ | touch_task の起動に失敗（Core1） | `SPAWN_TOUCH` |
| 12 | タッチ | タッチセンサの初期化の失敗（タイムアウトを含む） | `TOUCH_INIT_TIMEOUT` |
| 21 | USB・MIDI | usb_task / midi_tx_task / midi_rx_task の起動に失敗 | `SPAWN_USB_MIDI` |
| 22 | USB・MIDI | `MIDI_TX` のキューあふれ | `MIDI_TX_QUEUE_FULL` |
| 23 | USB・MIDI | MIDI 送信のタイムアウト（USB 未接続など） | `MIDI_TX_TIMEOUT` |
| 24 | USB・MIDI | MIDI 受信エラー | `MIDI_RX` |
| 25 | USB・MIDI | debug_stream_task の起動に失敗（`debug_stream` feature のみ） | `SPAWN_DEBUG_STREAM` |
| 31 | 圧力 | pressure_task の起動に失敗 | `SPAWN_PRESSURE` |
| 32 | 圧力 | ADC 値の取得エラー（タイムアウトを含む） | `ADC_READ` |
| 41 | 表示 | ui_task の起動に失敗 | `SPAWN_UI` |
| 42 | 表示 | OLED の初期化エラー | `OLED_INIT` |
| 43 | 表示 | OLED の転送エラー（タイムアウトを含む） | `OLED_FLUSH` |
| 44 | 表示 | ringled_task の起動に失敗 | `SPAWN_RINGLED` |
| 45 | 表示 | リング LED への書き込みのタイムアウト | `RINGLED_WRITE_TIMEOUT` |
| 51 | システム | status_led_task の起動に失敗（LED では表示できないので、OLED の診断ページで確認する） | `SPAWN_STATUS_LED` |
| 52 | システム | スイッチ（PCF8574）の初期化・読み込みのエラー（タイムアウトを含む） | `SWITCH_READ` |
| 55 | システム | panic | `PANIC` |

コードの一覧は `src/error.rs` の定数と、その先頭のコメントを正とする。

### 4.8 ソースの配置

```
src/
├── main.rs            初期化・ペリフェラルの割り当て・両コアの起動とタスクの spawn、usb_task
├── shared.rs          共有状態と診断値（§4.5・§4.6）
├── error.rs           エラーコード（§4.7）
├── constants.rs       キー数・MIDI チャンネル・スキャン周期など
├── debug_protocol.rs  debug_stream のパケットの組み立て
├── tasks/             タスク本体（ループ・周期・共有状態の読み書き）
│   ├── touch.rs       touch_task (Core1)。ロジックは touch/read_touch.rs と crates/touch_algo
│   ├── midi.rs        midi_tx_task, midi_rx_task, queue_midi
│   ├── pressure.rs    pressure_task（ロジックは touch/pressure.rs）
│   ├── ringled.rs     ringled_task（ロジックは ui/ringled.rs）
│   ├── ui.rs          ui_task（ロジックは ui/oled_display.rs）
│   ├── status_led.rs
│   └── debug_stream.rs
├── devices/           デバイスの最小ドライバ（I2C を所有せず、呼び出し毎に借りる）と PIO の I2C（pio_i2c）
├── touch/             タッチの読み込み・圧力のロジック
└── ui/                OLED・リング LED の描画のロジック
crates/touch_algo/     信号処理と解析（no_std。PC アプリと共有）
```

- タスク（`#[embassy_executor::task]`）とロジック（計算・描画）を分ける。ロジックの側は、どのコア・どのタスクから呼ばれるかを知らない

### 4.9 スタックとメモリ

- Core1 のスタックは `CORE1_STACK_SIZE` = 16KB。`TouchTracker`・`PlayInfo` は 96 キーで合わせて約 0.9KB。タスクの状態（future）は static に置かれるが、初期化のときに一時的にスタックへ置かれることがあるので、余裕を持たせる。`debug_stream` で信号処理と解析を作り直すときは、96 キーで約 8KB の一時の値がスタックに置かれる
- Core0 のスタックは `memory.x` の `_stack_size` = 8KB
- 大きな配列をタスク内のローカル変数に置くときは、スタック量に注意する
