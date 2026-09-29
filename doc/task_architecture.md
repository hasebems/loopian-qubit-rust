# コア・タスク構成の改修 設計書

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
| `ui_task` | 0 | 100ms | スイッチの判定、ページ／モードの切替、OLED の描画と I2C0 への転送 | `core1_oled_ui_task` ＋ `core1_i2c_task` の OLED 部分 |
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
- **解析する周期**: `SCAN_PERIOD_MS` を 10ms より短くする場合（デバッグ環境での 2ms など）でも、解析は 10ms 毎に行う。`ANALYSIS_DIVIDER = 10 / SCAN_PERIOD_MS` フレームに 1 回解析する。そのため `SCAN_PERIOD_MS` は 10 の約数（1, 2, 5, 10）に限る
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

- 96 キーの 1 周のスキャンは 8ms 前後と見積もっている。これに解析の時間を足して `SCAN_PERIOD_MS`（10ms）に収まる必要がある。段階 0（§6）で両方を実測する
- 収まらない場合は、Core1 の上でスキャンのタスクと解析のタスクを分ける
    - スキャンのタスクは、最新のフレームを `Signal` などで解析のタスクに渡す
    - 解析は、スキャンが I2C の完了を待っている間に進むので、全体が 1 周期に収まる
    - 代わりに、解析の処理中に I2C が完了すると、スキャンの再開が解析の分だけ遅れる（読み取りのタイミングが少しばらつく）

**ノートイベントの出力**

- `QubitTouch` の MIDI コールバックは、ノートイベントを `MIDI_TX` に `try_send` する。今の `RefCell` のバッファ（`send_buffer` / `send_index`）は不要になる
- チャンネルの決定（Piano: `MIDI_CH_FLOW`、Violin: `MIDI_CH_VIOLIN`）と、`RINGLED_CMD_TX_MOVED` を Note Off に読み替える処理は、`touch_task` 側で行ってから `MIDI_TX` に入れる。`midi_tx_task` は受け取ったものをそのまま送るだけにする
- キューがあふれたらエラーコードに記録する

### 4.2 Core0: midi_tx_task

- `MIDI_TX: Channel<CriticalSectionRawMutex, MidiMessage, 16>` を受信し、`Sender` で送る。送信は `with_timeout(MIDI_TX_TIMEOUT_MS)` で包む
- `MidiMessage` は USB MIDI の 4 バイトのパケット
- 書き手は Core1（`touch_task`）と Core0（`pressure_task`）。`CriticalSectionRawMutex` はコアをまたいでも排他が効き、受信側の起床もコアをまたいで働く
- USB が詰まっても、送信を待つのはこのタスクだけになり、タッチの解析は止まらない（P5 の解消）

### 4.3 Core0: pressure_task

- 現在の `adc_task` の処理に、`qubit_touch_task` の中にある CC11 の送信判定（`send_pressure_cc11_if_needed`）を移す
- CC11 と、Violin モードに入ったときの All Sound Off・CC11 の初期値は、`MIDI_TX` に入れる。`pressure.rs` の送信関数は、`Sender` を直接使う形から `MIDI_TX` に入れる形に変える
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

### 4.5 共有状態の整理（`src/shared.rs`）

`main.rs` に散らばっている static を `src/shared.rs` に集め、それぞれに「書き手・読み手・コア」をコメントで書く。

| 名前 | 型 | 書き手 | 読み手 | 備考 |
|---|---|---|---|---|
| `MIDI_TX` | `Channel<MidiMessage, 16>` | touch (C1), pressure (C0) | midi_tx (C0) | 新規。コアをまたぐ |
| `TOUCH0`–`TOUCH3` | `AtomicI32` | touch (C1) | ringled, ui (C0) | 変更なし |
| `ANY_TOUCH` | `AtomicBool` | touch (C1) | pressure (C0) | 変更なし |
| `PRESSURE` | `AtomicU32` | pressure (C0) | pressure, ui (C0) | 変更なし |
| `RINGLED_RX_BITS` | `[AtomicU32; 3]` | midi_rx (C0) | ringled (C0) | **96 ビットに拡張（§2.3）** |
| `WORK_MODE` | `AtomicU8` | ui (C0) | touch (C1), pressure, midi_rx (C0) | 変更なし |
| `SETTING_MODE` | `AtomicBool` | ui (C0) | touch (C1), pressure, ringled (C0) | 旧 `WORK_MODE_DISPLAY`。設定画面（基準値の補正中）であることを表すので、名前を意味に合わせる |
| `ERROR_CODE` | `AtomicU8` | 全タスク | status_led, ui (C0) | `src/error.rs` へ（§4.7） |
| `POINT0`–`POINT5`, `DEBUG_VALUE`, `AD_VALUE*` | Atomic | 各タスク | ui (C0) | デバッグ表示用。`debug_env.md` の段階で PC 側に移し、整理する |
| `SCAN_TIME_*`, `ANALYSIS_TIME_*`, `PERIOD_OVERRUN`, `UI_DRAW_TIME` など | Atomic | 各タスク | ui (C0) | 新規。診断用（§4.6） |

- `TOUCH_RAW_DATA` と `ELAPSED_TIME` は廃止する
- `Ordering::Relaxed` を基本とする方針は変えない

### 4.6 診断表示

改修の効果（周期の安定・遅れの減少）を確かめるため、OLED に診断ページを 1 つ用意する。デバッグ環境ができるまでは、これが唯一の確認手段になる。

- `touch_task`: スキャン時間と解析時間（それぞれ最小・平均・最大）、周期を超えた回数
- `ui_task` の描画時間
- `MIDI_TX` のキューの最大使用数、あふれた回数

### 4.7 エラーコード（`src/error.rs`）

- `ERROR_CODE` と、コードの定数（例: `pub const ADC_READ: u8 = 13;`）を `src/error.rs` に集める。数値の直書きをやめる（P8）
- 一の位・十の位とも 1–9 で採番する約束事は変えない
- タスクの増減に合わせて、spawn 失敗のコードを振り直す。案:

| コード | 意味 | 旧 |
|---|---|---|
| 11, 12 | （廃止: OLED ダブルバッファの初期投入） | 11, 12 |
| 13 | ADC 値の取得エラー | 13 |
| 14 | タッチセンサ初期化タイムアウト | 14 |
| 21 | touch_task の起動に失敗（Core1） | 22 |
| 31–38 | Core0 の各タスクの起動に失敗（midi_tx, usb, midi_rx, ringled, pressure, ui, status_led, debug_stream） | 31–35, 21, 23 |
| 41 | `MIDI_TX` のキューあふれ | 41, 43 |
| 42 | MIDI 送信の失敗（タイムアウト・USB 未接続など） | 42 |
| 44 | RingLED への書き込みのタイムアウト | 44 |
| 51 | OLED 初期化エラー | 51 |
| 52 | OLED 転送エラー | 52 |
| 53 | （廃止: 描画バッファ返却エラー） | 53 |
| 54 | MIDI 受信エラー | 54 |

番号は実装時に確定し、CLAUDE.md の一覧も更新する。

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

- Core1 のスタック（`CORE1_STACK_SIZE` = 8KB）: `QubitTouch`（履歴などを含む）が Core1 に移るが、embassy のタスクの状態（future）は static に置かれるので、スタックへの影響は `await` をまたがない局所変数の分だけ。`qtouch.rs` の関数内の一時配列の大きさは、実装時に確認する
- Core0 のスタック（`memory.x` の `_stack_size` = 8KB）: 同様。OLED のバッファ（1KB）は 2 つから 1 つに減る

## 5. ハードウェアの改修

OLED を I2C0（D6 = GP0 = SDA、D7 = GP1 = SCL）に移す。詳細は `doc/hw_modify.md` に分けて記述する。

- 段階 3 より前に改修を済ませる。改修前の基板では、段階 3 以降のファームの OLED は表示されない（エラー 51）
- 改修後の基板では、逆に `main` ブランチのファームの OLED が表示されなくなる（`doc/hw_modify.md` §5）

## 6. 作業の段階

各段階の終わりに、次を確認してコミットする。

- ビルド: `cargo build`, `--release`, `--features test_mode`, `--features no_pca9544`, `cargo clippy --all-features -- --deny=warnings`, `cargo fmt --check`
- 実機: 下の「確認」欄

| 段階 | 内容 | 確認 |
|---|---|---|
| 0 | **改修前の計測**: 96 キーの 1 周のスキャン時間、`qubit_touch_task` の解析時間（既存の `total_time` / `loop_times` を使う）、OLED の描画時間と転送時間。タッチの反応・LED・Violin の CC11 の様子も記録する | 比較の基準にする。スキャン＋解析が 10ms に収まるかで、§4.1 の「1 周期に収まらない場合」の要否を決める |
| 1 | **不具合の修正**: `RINGLED_RX_BITS` を 96 ビットにする（§2.3） | 受信ノートの LED が 1 箇所だけ光る。dev ビルドでも panic しない |
| 2 | **ファイルの分割**: `shared.rs`, `error.rs`, `tasks/` を作り、タスクを移す。動作は一切変えない | 段階 0 と同じに動く |
| 3 | **I2C の分離**（ハードウェアの改修後）: I2C1 を Core1 で生成、OLED を I2C0 へ、`ui_task` と `status_led_task` を Core0 へ。ダブルバッファの廃止 | OLED の表示、スイッチの操作、設定画面、エラーの点滅 |
| 4 | **MIDI 送信の分離**: `MIDI_TX` と `midi_tx_task`。CC11 を `pressure_task` へ。解析はまだ Core0 のまま | ノートの送信、Violin モードの CC11・All Sound Off。USB を抜き差ししてもタッチの解析が止まらない |
| 5 | **解析の Core1 への移動と周期化**: `touch_task`（`Ticker`、スキャン → 解析）。`TOUCH_RAW_DATA` の廃止 | 診断ページで、スキャン＋解析が周期に収まっていること。タッチの反応が段階 0 と同等以上 |
| 6 | **診断と名前の整理**: 診断ページ、`SETTING_MODE` への名前変更、エラーコードの振り直し、CLAUDE.md の更新 | 診断ページの表示。CLAUDE.md が新しい構成と一致している |

- 段階 1 は他と独立しているので、`main` から分岐した `fix_ringled_rx_bits` ブランチで行い、`main` に入れて本番用のファームにも反映する。`task_architecture` ブランチには、その後で `main` から取り込む
- 段階 2 は差分が大きいが、中身は移動だけにする。レビューで「移動以外の変更が無い」ことを確かめやすくするため
- 段階 4 で先に `MIDI_TX` を作っておくと、段階 5 で解析を Core1 に移すときに、MIDI の出口を変えずに済む

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

- 96 キー構成で、スキャン＋解析が 10ms に収まるか。段階 0 の計測で確認し、収まらなければ §4.1 のとおりタスクを分ける
- OLED の描画時間が Core0 の MIDI 送信に与える影響。段階 3 で計測し、必要なら §4.4 の対処を行う
- `InterruptExecutor` を使うかどうか（§4.4）
- `pressure_task`（ADC）を Core1 に置く案。センシングをまとめる考え方もあるが、Core1 の周期を乱す要因を増やさないため、今回は Core0 に置く
