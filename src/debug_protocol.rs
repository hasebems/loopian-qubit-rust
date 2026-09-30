//  Created by Hasebe Masahiko on 2026/09/30.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! デバッグ用の通信プロトコル（ファーム → PC のバイナリパケットの組み立て）
//!
//! 形式は doc/debug_env.md §5.1。すべてリトルエンディアン。
//! `0xA5 0x5A type len(u16) payload crc8`。crc8 は type・len・payload にかける（CRC-8/ATM）
use crate::constants::TOTAL_QT_KEYS;
use crate::shared::DebugFrame;

pub const TYPE_FRAME: u8 = 0x01;
pub const TYPE_INFO: u8 = 0x20;
pub const TYPE_TEXT: u8 = 0x7f;

const SYNC: [u8; 2] = [0xa5, 0x5a];
const HEADER_LEN: usize = 5; // sync(2) + type(1) + len(2)
const VALID_BYTES: usize = TOTAL_QT_KEYS.div_ceil(8);
// FRAME の payload: seq(4) + time_us(4) + nkeys(1) + valid + raw
const FRAME_PAYLOAD_LEN: usize = 9 + VALID_BYTES + 2 * TOTAL_QT_KEYS;
const INFO_PAYLOAD_LEN: usize = 8 + 16 + 1 + 4 + 4;
pub const TEXT_MAX_LEN: usize = 64; // TEXT の payload の最大長（超えた分は切り捨てる）
const PAYLOAD_MAX_LEN: usize = max(max(FRAME_PAYLOAD_LEN, INFO_PAYLOAD_LEN), TEXT_MAX_LEN);
const PACKET_MAX_LEN: usize = HEADER_LEN + PAYLOAD_MAX_LEN + 1;
const _: () = assert!(TOTAL_QT_KEYS <= 128); // valid は u128

const fn max(a: usize, b: usize) -> usize {
    if a > b { a } else { b }
}

/// 1 つのパケットを組み立てるバッファ
pub struct Packet {
    buf: [u8; PACKET_MAX_LEN],
    len: usize,
}

impl Packet {
    pub fn new() -> Self {
        Self::start(TYPE_TEXT)
    }

    fn start(packet_type: u8) -> Self {
        let mut buf = [0u8; PACKET_MAX_LEN];
        buf[..2].copy_from_slice(&SYNC);
        buf[2] = packet_type;
        Self {
            buf,
            len: HEADER_LEN,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    /// len と crc8 を書き込んで、送るバイト列を返す
    fn finish(&mut self) -> &[u8] {
        let payload_len = (self.len - HEADER_LEN) as u16;
        self.buf[3..5].copy_from_slice(&payload_len.to_le_bytes());
        let crc = crc8(&self.buf[2..self.len]);
        self.buf[self.len] = crc;
        self.len += 1;
        &self.buf[..self.len]
    }

    /// FRAME: seq, time_us, nkeys, valid (ceil(nkeys/8) バイト), raw (u16 × nkeys)
    pub fn frame(&mut self, frame: &DebugFrame) -> &[u8] {
        *self = Self::start(TYPE_FRAME);
        self.push(&frame.seq.to_le_bytes());
        self.push(&frame.time_us.to_le_bytes());
        self.push(&[TOTAL_QT_KEYS as u8]);
        self.push(&frame.valid.to_le_bytes()[..VALID_BYTES]);
        for raw in frame.raw.iter() {
            self.push(&raw.to_le_bytes());
        }
        self.finish()
    }

    /// INFO: version [u8; 8], build_date [u8; 16], nkeys, scan_period_us, dropped
    pub fn info(&mut self, scan_period_us: u32, dropped: u32) -> &[u8] {
        *self = Self::start(TYPE_INFO);
        self.push(&fixed_str::<8>(env!("BUILD_VERSION")));
        self.push(&fixed_str::<16>(env!("BUILD_DATE")));
        self.push(&[TOTAL_QT_KEYS as u8]);
        self.push(&scan_period_us.to_le_bytes());
        self.push(&dropped.to_le_bytes());
        self.finish()
    }

    /// TEXT: UTF-8 の文字列（TEXT_MAX_LEN バイトを超えた分は切り捨てる）
    pub fn text(&mut self, parts: &[&str]) -> &[u8] {
        *self = Self::start(TYPE_TEXT);
        for part in parts {
            let room = HEADER_LEN + TEXT_MAX_LEN - self.len;
            let bytes = part.as_bytes();
            self.push(&bytes[..bytes.len().min(room)]);
        }
        self.finish()
    }
}

/// 文字列を N バイトの固定長にする（余りは 0 で埋め、長ければ切り捨てる）
fn fixed_str<const N: usize>(s: &str) -> [u8; N] {
    let mut out = [0u8; N];
    let len = s.len().min(N);
    out[..len].copy_from_slice(&s.as_bytes()[..len]);
    out
}

/// CRC-8/ATM（多項式 0x07、初期値 0、反転なし）
fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}
