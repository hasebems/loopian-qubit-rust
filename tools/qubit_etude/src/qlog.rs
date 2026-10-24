//! 記録ファイル（.qlog）の読み書き（doc/debug_env.md §4.3）
//!
//! 先頭に固定長のヘッダ（HEADER_LEN バイト）、その後に受信したバイト列をそのまま続ける。
//! 受信したまま保存するので、後でプロトコルの解釈を直しても読み直せる。
//!
//! ヘッダ（リトルエンディアン）:
//! | offset | 型 | 内容 |
//! |---|---|---|
//! | 0 | `[u8; 4]` | `"QLOG"` |
//! | 4 | `u16` | 形式の版（1） |
//! | 6 | `u16` | ヘッダの長さ（64）。読むときはこの長さだけ読み飛ばす |
//! | 8 | `i64` | 記録を始めた日時（Unix 時刻の ms） |
//! | 16 | `[u8; 8]` | ファームのバージョン（INFO の version。不明なら 0） |
//! | 24 | `[u8; 16]` | ファームのビルド日（INFO の build_date） |
//! | 40 | `u8` | キー数（INFO の nkeys） |
//! | 41 | `[u8; 3]` | 予約（0） |
//! | 44 | `u32` | 記録を始めたときのスキャン周期（µs） |
//! | 48 | `[u8; 16]` | 予約（0） |
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use crate::protocol::Info;

const MAGIC: &[u8; 4] = b"QLOG";
const FORMAT_VERSION: u16 = 1;
pub const HEADER_LEN: usize = 64;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Header {
    pub recorded_at_ms: i64,
    pub version: String,
    pub build_date: String,
    pub nkeys: u8,
    pub scan_period_us: u32,
}

impl Header {
    /// 今の時刻と、分かっていればファームの INFO から作る
    pub fn now(info: Option<&Info>) -> Self {
        let mut header = Header {
            recorded_at_ms: chrono::Local::now().timestamp_millis(),
            ..Default::default()
        };
        if let Some(info) = info {
            header.version = info.version.clone();
            header.build_date = info.build_date.clone();
            header.nkeys = info.nkeys;
            header.scan_period_us = info.scan_period_us;
        }
        header
    }

    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(MAGIC);
        b[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        b[6..8].copy_from_slice(&(HEADER_LEN as u16).to_le_bytes());
        b[8..16].copy_from_slice(&self.recorded_at_ms.to_le_bytes());
        put_str(&mut b[16..24], &self.version);
        put_str(&mut b[24..40], &self.build_date);
        b[40] = self.nkeys;
        b[44..48].copy_from_slice(&self.scan_period_us.to_le_bytes());
        b
    }

    /// ヘッダを読み、ヘッダの長さも返す
    pub fn from_bytes(b: &[u8]) -> io::Result<(Self, usize)> {
        let bad = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_string());
        if b.len() < 48 || &b[0..4] != MAGIC {
            return Err(bad("qlog ファイルではありません"));
        }
        let version = u16::from_le_bytes([b[4], b[5]]);
        if version != FORMAT_VERSION {
            return Err(bad(&format!("対応していない qlog の版です: {version}")));
        }
        let header_len = u16::from_le_bytes([b[6], b[7]]) as usize;
        if header_len < 48 || b.len() < header_len {
            return Err(bad("qlog のヘッダが壊れています"));
        }
        let header = Header {
            recorded_at_ms: i64::from_le_bytes(b[8..16].try_into().unwrap()),
            version: get_str(&b[16..24]),
            build_date: get_str(&b[24..40]),
            nkeys: b[40],
            scan_period_us: u32::from_le_bytes(b[44..48].try_into().unwrap()),
        };
        Ok((header, header_len))
    }

    pub fn recorded_at_text(&self) -> String {
        chrono::DateTime::from_timestamp_millis(self.recorded_at_ms)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default()
    }
}

fn put_str(dst: &mut [u8], s: &str) {
    let n = s.len().min(dst.len());
    dst[..n].copy_from_slice(&s.as_bytes()[..n]);
}

fn get_str(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// 記録中のファイル。受信したバイト列をそのまま書き足す
pub struct Writer {
    file: io::BufWriter<File>,
    pub bytes: u64,
}

impl Writer {
    pub fn create(path: &Path, header: &Header) -> io::Result<Self> {
        let mut file = io::BufWriter::new(File::create(path)?);
        file.write_all(&header.to_bytes())?;
        Ok(Self { file, bytes: 0 })
    }

    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.bytes += data.len() as u64;
        self.file.write_all(data)
    }

    pub fn finish(mut self) -> io::Result<u64> {
        self.file.flush()?;
        Ok(self.bytes)
    }
}

/// ファイル全体を読み、ヘッダと、受信したバイト列を返す
pub fn read(path: &Path) -> io::Result<(Header, Vec<u8>)> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    let (header, header_len) = Header::from_bytes(&bytes)?;
    bytes.drain(..header_len);
    Ok((header, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let header = Header {
            recorded_at_ms: 1_790_000_000_000,
            version: "v0.3.0".into(),
            build_date: "26-09-30".into(),
            nkeys: 6,
            scan_period_us: 2000,
        };
        let bytes = header.to_bytes();
        assert_eq!(Header::from_bytes(&bytes).unwrap(), (header, HEADER_LEN));
    }

    #[test]
    fn rejects_other_files() {
        assert!(Header::from_bytes(&[0u8; 64]).is_err());
    }
}
