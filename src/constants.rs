//  Created by Hasebe Masahiko on 2026/02/15.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//

pub const CORE1_STACK_SIZE: usize = 8192; // コア1のスタックサイズ

// Message for Ringled
pub const RINGLED_MESSAGE_SIZE: usize = 16; // ringled_task でやりとりするメッセージのサイズ
pub const RINGLED_CMD_NONE: u8 = 0x00; // コマンドなし
pub const RINGLED_CMD_TX_ON: u8 = 0x90; // 送信用Note Onコマンド
pub const RINGLED_CMD_TX_OFF: u8 = 0x80; // 送信用Note Offコマンド
pub const RINGLED_CMD_TX_MOVED: u8 = 0xa0; // 送信用Note Moveコマンド(NoteOff)
pub const RINGLED_CMD_RX_ON: u8 = 0x9f; // 受信用Note Onコマンド
pub const RINGLED_CMD_RX_OFF: u8 = 0x8f; // 受信用Note Offコマンド

pub const PCA9544_NUM_CHANNELS: u8 = 4; // PCA9544のチャネル数
pub const PCA9544_NUM_DEVICES: u8 = 4; // PCA9544の台数
pub const AT42QT_KEYS_PER_DEVICE: u8 = 6; // AT42QT1070

pub const TOTAL_CH: usize = (PCA9544_NUM_CHANNELS * PCA9544_NUM_DEVICES) as usize;
pub const TOTAL_QT_KEYS: usize = TOTAL_CH * (AT42QT_KEYS_PER_DEVICE as usize);

// MIDI Note Number
pub const KEYBD_LO: u8 = 21; // A0

pub const MAX_TOUCH_POINTS: usize = 4; // Maximum number of touch points to track
