//  Created by Hasebe Masahiko on 2026/02/11.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
use portable_atomic::AtomicU8;

// エラーコード（0: 正常）。内蔵LEDが十の位→一の位の回数だけ点滅する。panic時は255
pub static ERROR_CODE: AtomicU8 = AtomicU8::new(0);

// ERROR CODE 一覧(一の位も十の位も1-9の範囲)
// 11: BUFFER_FROM_DISPLAYの初期投入に失敗
// 12: BUFFER_FROM_DISPLAYの初期投入に失敗（2回目）
// 13: ADC値取得エラー
// 14: Touch Sensor初期化タイムアウト
// 21: Core1 LED Taskの起動に失敗
// 22: Core1 I2C Taskの起動に失敗
// 23: Core1 OLED UI Taskの起動に失敗
// 31: QubitTouch Taskの起動に失敗
// 32: USB Taskの起動に失敗
// 33: MIDI RX Taskの起動に失敗
// 34: RingLED Taskの起動に失敗
// 35: ADC Taskの起動に失敗
// 41: タッチイベントのバッファオーバーフロー
// 42: MIDIイベントの送信失敗（USB未接続など）
// 43: MIDIイベントのバッファオーバーフロー
// 44: RingLEDへの書き込みのタイムアウト
// 51: OLED初期化エラー
// 52: 描画バッファ受信エラー
// 53: 描画バッファ返却エラー
// 54: MIDIイベントの受信エラー
