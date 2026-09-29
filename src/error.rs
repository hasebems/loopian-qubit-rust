//  Created by Hasebe Masahiko on 2026/02/11.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! エラーコード
//!
//! パニックさせない方針で、失敗は ERROR_CODE に 2 桁のコードを書き込む。
//! 内蔵 LED (status_led_task) が十の位 → 一の位の回数だけ点滅する。
//! 新しいコードは一の位・十の位とも 1–9 の範囲で採番する。
use portable_atomic::{AtomicU8, Ordering};

// エラーコード（0: 正常）
static ERROR_CODE: AtomicU8 = AtomicU8::new(0);

// 1x: 初期化・入力
pub const ADC_READ: u8 = 13; // ADC値取得エラー（タイムアウトを含む）
pub const TOUCH_INIT_TIMEOUT: u8 = 14; // タッチセンサ初期化タイムアウト

// 2x: Core1 のタスクの起動失敗
pub const SPAWN_TOUCH: u8 = 21; // touch_task

// 3x: Core0 のタスクの起動失敗
pub const SPAWN_MIDI_TX: u8 = 31; // midi_tx_task
pub const SPAWN_USB: u8 = 32; // usb_task
pub const SPAWN_MIDI_RX: u8 = 33; // midi_rx_task
pub const SPAWN_RINGLED: u8 = 34; // ringled_task
pub const SPAWN_PRESSURE: u8 = 35; // pressure_task
pub const SPAWN_UI: u8 = 36; // ui_task
pub const SPAWN_STATUS_LED: u8 = 37; // status_led_task

// 4x: MIDI・LED の出力
pub const MIDI_TX_QUEUE_FULL: u8 = 41; // MIDI送信キュー (MIDI_TX) のあふれ
pub const MIDI_TX_TIMEOUT: u8 = 42; // MIDI送信のタイムアウト（USB未接続など）
pub const RINGLED_WRITE_TIMEOUT: u8 = 44; // RingLEDへの書き込みのタイムアウト

// 5x: OLED・MIDI の受信
pub const OLED_INIT: u8 = 51; // OLED初期化エラー
pub const OLED_FLUSH: u8 = 52; // OLED転送エラー（タイムアウトを含む）
pub const MIDI_RX: u8 = 54; // MIDIイベントの受信エラー

// panic ハンドラが書き込む
pub const PANIC: u8 = 255;

/// エラーコードを記録する（後から起きたエラーで上書きする）
pub fn set(code: u8) {
    ERROR_CODE.store(code, Ordering::Relaxed);
}

/// エラーコードを消す（設定画面に入ったとき）
pub fn clear() {
    ERROR_CODE.store(0, Ordering::Relaxed);
}

/// 現在のエラーコード（0: 正常）
pub fn get() -> u8 {
    ERROR_CODE.load(Ordering::Relaxed)
}
