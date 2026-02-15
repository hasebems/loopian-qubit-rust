//  Created by Hasebe Masahiko on 2026/02/15.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//

pub const CORE1_STACK_SIZE: usize = 8192; // コア1のスタックサイズ

pub const PCA9544_NUM_CHANNELS: u8 = 4; // PCA9544のチャネル数
pub const PCA9544_NUM_DEVICES: u8 = 4; // PCA9544の台数
pub const AT42QT_KEYS_PER_DEVICE: u8 = 6; // AT42QT1070

pub const TOTAL_QT_KEYS: usize = (PCA9544_NUM_CHANNELS as usize)
    * (PCA9544_NUM_DEVICES as usize)
    * (AT42QT_KEYS_PER_DEVICE as usize);

// MIDI Note Number
pub const KEYBD_LO: u8 = 21; // A0
