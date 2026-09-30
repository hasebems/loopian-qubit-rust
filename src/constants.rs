//  Created by Hasebe Masahiko on 2026/02/15.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//

// コア1のスタックサイズ。Core1 の touch_task は QubitTouch (約3.7KB) と ReadTouch (約0.6KB) を持ち、
// 初期化時に一時的にスタックに置かれることがあるので、余裕を持たせる
pub const CORE1_STACK_SIZE: usize = 16384;

// タッチのスキャン周期と解析周期
// QubitTouch の時間に関する定数は 10ms 毎に呼ばれる前提なので、解析は 10ms 毎に行う。
// スキャン周期を短くする場合は、ANALYSIS_DIVIDER フレームに 1 回解析する（10 の約数に限る）
pub const SCAN_PERIOD_MS: u64 = 10;
pub const ANALYSIS_PERIOD_MS: u64 = 10;
pub const ANALYSIS_DIVIDER: u32 = (ANALYSIS_PERIOD_MS / SCAN_PERIOD_MS) as u32;
const _: () = assert!(ANALYSIS_PERIOD_MS.is_multiple_of(SCAN_PERIOD_MS));
// 起動後、この時間はスキャンだけを行い、解析（ノートの生成）をしない。
// read_touch はチップの基準値を最初のスキャンの後に初めて読むため、最初のフレームは基準値 0 で全キーが大きな値になる。
// また AT42QT1070 自身も電源投入から 230ms 未満で基準値を校正する（データシート §6.5 TD）。
// 基準値は 12 スキャン (120ms) 毎に読み直されるので、何度か更新されて落ち着くまで待つ
pub const TOUCH_STARTUP_SETTLE_MS: u64 = 500;

pub const MIDI_NOTE_ON: u8 = 0x90;
pub const MIDI_NOTE_OFF: u8 = 0x80;
pub const MIDI_CC: u8 = 0xb0;

// Message for Ringled / touch callback status
pub const RINGLED_CMD_TX_ON: u8 = MIDI_NOTE_ON; // 送信用Note Onコマンド
pub const RINGLED_CMD_TX_OFF: u8 = MIDI_NOTE_OFF; // 送信用Note Offコマンド
pub const RINGLED_CMD_TX_MOVED: u8 = 0xa0; // 送信用Note Moveコマンド(NoteOff)

// チェック用
#[cfg(not(feature = "test_mode"))]
pub const PCA9544_NUM_CHANNELS: u8 = 4; // PCA9544のチャネル数
#[cfg(feature = "test_mode")]
pub const PCA9544_NUM_CHANNELS: u8 = 1; // PCA9544のチャネル数 (テストモード)

#[cfg(not(feature = "test_mode"))]
pub const PCA9544_NUM_DEVICES: u8 = 4; // PCA9544の台数
#[cfg(feature = "test_mode")]
pub const PCA9544_NUM_DEVICES: u8 = 1; // PCA9544の台数 (テストモード)
pub const AT42QT_KEYS_PER_DEVICE: usize = 6; // AT42QT1070

pub const TOTAL_CH: usize = (PCA9544_NUM_CHANNELS * PCA9544_NUM_DEVICES) as usize;
pub const TOTAL_QT_KEYS: usize = TOTAL_CH * AT42QT_KEYS_PER_DEVICE;
// AT42の読み取りインデックスnを (n + TOUCH_INDEX_SHIFT) % TOTAL_QT_KEYS に再配置する
// 96キー構成では「6 -> 0」「0 -> 90」となる
pub const TOUCH_INDEX_SHIFT: usize = TOTAL_QT_KEYS - 6;
pub const NUM_LEDS: usize = TOTAL_QT_KEYS;
pub const RINGLED_RX_WORDS: usize = NUM_LEDS.div_ceil(32); // 受信ノート表示のビット列(1bit/LED)を格納する u32 の個数

pub const MAX_TOUCH_POINTS: usize = 4; // Maximum number of touch points to track
pub const MAX_ADC_CHANNELS: usize = 3; // ADCのチャンネル数

// MIDI
pub const KEYBD_LO: u8 = 21; // A0
pub const _MIDI_CH_PIANO: u8 = 0; // ピアノ用MIDIチャンネル
pub const MIDI_CH_VIOLIN: u8 = 1; // バイオリン用MIDIチャンネル
pub const MIDI_CH_FLOW: u8 = 12;
pub const PIANO_OFFSET: u8 = KEYBD_LO - 4;
pub const VIOLIN_OFFSET: u8 = 55; // バイオリンモードのMIDIノートオフセット

pub const MIDI_TX_TIMEOUT_MS: u64 = 20;

// Work mode
#[repr(u8)]
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum WorkMode {
    Piano = 0,
    Violin = 1,
}
// u8 から WorkMode への変換を定義
impl TryFrom<u8> for WorkMode {
    type Error = ();

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(WorkMode::Piano),
            1 => Ok(WorkMode::Violin),
            _ => Err(()), // 定義外の数値の場合はエラー
        }
    }
}
