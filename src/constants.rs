//  Created by Hasebe Masahiko on 2026/02/15.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//

pub const CORE1_STACK_SIZE: usize = 8192; // コア1のスタックサイズ

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
