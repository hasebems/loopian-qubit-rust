> **その後の変更**（2026-10-01 追記）: 本書は RingLED をイベント駆動から共有状態の参照に変えたとき（2026-05）の作業記録。方針（送信は `TOUCH0-3` を直接読む、受信はビット共有、キューを使わない）は今も同じ。その後、次のように変わっている
> - `main.rs` のタスクは `src/tasks/` に分けた。`qubit_touch_task` は Core1 の `touch_task`（`tasks/touch.rs`）、`ringled_task` は `tasks/ringled.rs`、`midi_rx_task` は `tasks/midi.rs`。共有状態は `src/shared.rs`（`doc/task_architecture.md`）
> - このとき `RINGLED_RX_BITS` を `u32` 1 語（32 LED 分）にしたため、受信ノートの LED が 3 箇所光る不具合が入った。`[AtomicU32; RINGLED_RX_WORDS]`（96 LED 分）にして直した（`doc/task_architecture.md` §2.3）

以下の変更をします

# 概要
現在 main.rs: async fn qubit_touch_task() において、MIDI出力時にRINGLED_MESSAGE を使ってメッセージを RingLED に送っています。
この処理をやめて、RingLED では、イベントメッセージを送らず TOUCH0-3 の内容を直接参照し値が入っていれば、その位置を光らす方針に変更します。

# 実装方針（更新）
- TX表示: RingLEDタスクが `TOUCH0-3` を直接読み取り、`0..9999` の値を位置として描画する。
- RX表示: `midi_rx_task()` が Note On/Off に応じて共有ビット `RINGLED_RX_BITS` を更新し、RingLEDタスクが参照して描画する。
- イベントキュー: `RINGLED_MESSAGE` は廃止し、RingLEDは他タスクと非同期キュー連携しない独立タスクとして動作する。

# 期待効果
- RingLED表示が MIDI/TX イベントバーストやキュー詰まりに影響されにくくなる。
- RingLEDの表示周期を他タスクの処理負荷と独立して調整しやすくなる。
MIDI RX についてもビット共有にし、イベントメッセージでは送らない方式に変更します。

-----

対応しました。ご要望どおり、RX表示もイベントキューではなくビット共有に変更し、RingLEDを他タスクから独立動作にしました。

変更内容
1. RingLEDイベントキューを廃止
- src/main.rs
- qubit_touch_task から RingLED 送信処理を削除
- ringled_task はキュー受信をやめ、共有状態を毎周期参照

2. RX表示をビット共有化
- src/main.rs
- 共有変数 RINGLED_RX_BITS を追加（1bit/LED）
- midi_rx_task の Note On/Off で RINGLED_RX_BITS を fetch_or / fetch_and 更新
- これにより RX も非イベント化

3. TX表示を TOUCH0-3 直接参照
- src/main.rs
- ringled_task が TOUCH0-3 を直接読み取り
- 0..9999 を有効値として location に変換（value / 100.0）
- 10000 は未タッチ扱い

4. RingLED描画APIを共有状態入力型へ変更
- src/ui/ringled.rs
- 旧 set_color(cmd, location) を廃止
- 新 render(touchkey_state, rxkey_bits) に変更
- 内部保持を最小化（counter のみ）

5. 不要定数の整理
- src/constants.rs
- 使わなくなった RINGLED_MESSAGE 系・RXコマンド系定数を削除

6. 仕様メモ更新
- doc/ringled_modify.md
- TX=TOUCH直接参照、RX=ビット共有、キュー廃止の方針を明記

検証結果
- 実行コマンド: cargo fmt && cargo check
- 結果: エラーなしで通過

この状態で、RingLED表示周期は ringled_task 側だけで独立調整できます。

変更を行いました。