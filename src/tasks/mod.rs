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
//! | ui_task             | Core0 | 100ms         | ui.rs           |
//! | status_led_task     | Core0 | 常駐          | status_led.rs   |
//! | touch_scan_task     | Core1 | 周期なし      | touch_scan.rs   |
//!
//! I2C は 2 系統: I2C0 (GP0/GP1) は OLED で ui_task が、I2C1 (GP6/GP7) はタッチセンサで
//! touch_scan_task が専有する。I2C1 は Core1 で生成し、その割り込みを Core1 で処理する
pub mod midi;
pub mod pressure;
pub mod ringled;
pub mod status_led;
pub mod touch;
pub mod touch_scan;
pub mod ui;
