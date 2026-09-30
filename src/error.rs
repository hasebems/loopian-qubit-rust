//  Created by Hasebe Masahiko on 2026/02/11.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! エラーコード
//!
//! パニックさせない方針で、失敗は ERROR_CODE に 2 桁のコードを書き込む。
//! 内蔵 LED (status_led_task) が十の位 → 一の位の回数だけ点滅する。
//! 点滅を数えやすいよう、十の位は機能の分類、一の位はその中の番号とし、どちらも 1–5 の範囲で採番する。
use portable_atomic::{AtomicU8, Ordering};

// エラーコード（0: 正常）
static ERROR_CODE: AtomicU8 = AtomicU8::new(0);

// 1x: タッチ (Core1)
pub const SPAWN_TOUCH: u8 = 11; // touch_task の起動に失敗
pub const TOUCH_INIT_TIMEOUT: u8 = 12; // タッチセンサ初期化タイムアウト

// 2x: USB・MIDI
pub const SPAWN_USB_MIDI: u8 = 21; // usb_task / midi_tx_task / midi_rx_task の起動に失敗
pub const MIDI_TX_QUEUE_FULL: u8 = 22; // MIDI送信キュー (MIDI_TX) のあふれ
pub const MIDI_TX_TIMEOUT: u8 = 23; // MIDI送信のタイムアウト（USB未接続など）
pub const MIDI_RX: u8 = 24; // MIDIイベントの受信エラー
#[cfg(feature = "debug_stream")]
pub const SPAWN_DEBUG_STREAM: u8 = 25; // debug_stream_task の起動に失敗（doc/debug_env.md）

// 3x: 圧力 (ADC)
pub const SPAWN_PRESSURE: u8 = 31; // pressure_task の起動に失敗
pub const ADC_READ: u8 = 32; // ADC値取得エラー（タイムアウトを含む）

// 4x: 表示 (OLED・RingLED)
pub const SPAWN_UI: u8 = 41; // ui_task の起動に失敗
pub const OLED_INIT: u8 = 42; // OLED初期化エラー
pub const OLED_FLUSH: u8 = 43; // OLED転送エラー（タイムアウトを含む）
pub const SPAWN_RINGLED: u8 = 44; // ringled_task の起動に失敗
pub const RINGLED_WRITE_TIMEOUT: u8 = 45; // RingLEDへの書き込みのタイムアウト

// 5x: システム
pub const SPAWN_STATUS_LED: u8 = 51; // status_led_task の起動に失敗（LED では表示できない。OLED の診断ページで確認する）
// panic ハンドラが書き込む。LED を点滅させる status_led_task は Core0 にあるため、
// Core0 で panic したときは LED では表示できない（Core1 の panic は表示できる）
pub const PANIC: u8 = 55;

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
