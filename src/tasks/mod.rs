//  Created by Hasebe Masahiko on 2026/02/11.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! タスク本体（ループ・周期・共有状態の読み書き）
//!
//! | タスク              | コア  | 周期          | ファイル        |
//! |---------------------|-------|---------------|-----------------|
//! | midi_tx_task        | Core0 | MIDI_TX で起動 | midi.rs        |
//! | midi_rx_task        | Core0 | USB受信で起動 | midi.rs         |
//! | usb_task            | Core0 | 常駐          | main.rs         |
//! | ringled_task        | Core0 | 20ms          | ringled.rs      |
//! | pressure_task       | Core0 | 10ms          | pressure.rs     |
//! | ui_task             | Core0 | 100ms (描画 200ms) | ui.rs      |
//! | status_led_task     | Core0 | 常駐          | status_led.rs   |
//! | touch_task          | Core1 | 10ms (debug_stream では 2ms) | touch.rs |
//! | debug_stream_task   | Core0 | DEBUG_FRAMES / CDC 受信で起動 | debug_stream.rs（debug_stream feature） |
//!
//! I2C: I2C1 (GP6/GP7) はタッチセンサで touch_task が、PIO の I2C (GP3/GP4) は OLED と
//! スイッチの PCF8574 で ui_task が専有する。I2C1 は Core1 で生成し、その割り込みを Core1 で処理する。
//! I2C0 (GP0/GP1) はタッチセンサの 2 系統目にする（doc/hw96_firmware261114.md §5。まだ使っていない）
//!
//! Core1 の touch_task はスキャン → 信号処理（touch_algo）→ 解析 → ノートイベントの生成をひと続きに行う。
//! コアをまたぐのは MIDI_TX（キュー）と Atomic だけ
#[cfg(feature = "debug_stream")]
pub mod debug_stream;
pub mod midi;
pub mod pressure;
pub mod ringled;
pub mod status_led;
pub mod touch;
pub mod ui;
