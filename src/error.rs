//  Created by Hasebe Masahiko on 2026/02/11.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
use portable_atomic::AtomicU8;

// エラーコード（0: 正常）。内蔵LEDが十の位→一の位の回数だけ点滅する。panic時は255
pub static ERROR_CODE: AtomicU8 = AtomicU8::new(0);

// ERROR CODE 一覧(一の位も十の位も1-9の範囲)
// 11: （廃止）BUFFER_FROM_DISPLAYの初期投入に失敗
// 12: （廃止）BUFFER_FROM_DISPLAYの初期投入に失敗（2回目）
// 13: ADC値取得エラー
// 14: Touch Sensor初期化タイムアウト
// 21: （廃止）Core1 LED Taskの起動に失敗 → 37 へ
// 22: Touch Task (Core1) の起動に失敗
// 23: （廃止）Core1 OLED UI Taskの起動に失敗 → 36 へ
// 31: （廃止）QubitTouch Taskの起動に失敗 → 22 (Touch Task) へ
// 32: USB Taskの起動に失敗
// 33: MIDI RX Taskの起動に失敗
// 34: RingLED Taskの起動に失敗
// 35: Pressure Task (ADC) の起動に失敗
// 36: UI Taskの起動に失敗
// 37: Status LED Taskの起動に失敗
// 38: MIDI TX Taskの起動に失敗
// 41: MIDI送信キュー (MIDI_TX) のあふれ
// 42: MIDIイベントの送信失敗（タイムアウト。USB未接続など）
// 43: （廃止）MIDIイベントのバッファオーバーフロー → 41 へ
// 44: RingLEDへの書き込みのタイムアウト
// 51: OLED初期化エラー
// 52: OLED転送エラー（タイムアウトを含む）
// 53: （廃止）描画バッファ返却エラー
// 54: MIDIイベントの受信エラー
