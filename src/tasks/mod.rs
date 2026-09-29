//  Created by Hasebe Masahiko on 2026/02/11.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! タスク本体（ループ・周期・共有状態の読み書き）
//!
//! | タスク              | コア  | 周期          | ファイル        |
//! |---------------------|-------|---------------|-----------------|
//! | qubit_touch_task    | Core0 | 10ms          | touch.rs        |
//! | midi_rx_task        | Core0 | USB受信で起動 | midi.rs         |
//! | usb_task            | Core0 | 常駐          | main.rs         |
//! | ringled_task        | Core0 | 20ms          | ringled.rs      |
//! | adc_task            | Core0 | 10ms          | pressure.rs     |
//! | core1_i2c_task      | Core1 | 周期なし      | core1_i2c.rs    |
//! | core1_oled_ui_task  | Core1 | 100ms         | ui.rs           |
//! | core1_led_task      | Core1 | 常駐          | status_led.rs   |
pub mod core1_i2c;
pub mod midi;
pub mod pressure;
pub mod ringled;
pub mod status_led;
pub mod touch;
pub mod ui;
