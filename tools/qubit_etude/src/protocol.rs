//! ファーム → PC のバイナリパケットの解釈（doc/debug_env.md §5.1）
//!
//! 形式: `0xA5 0x5A type len(u16) payload crc8`。すべてリトルエンディアン。
//! crc8 は type・len・payload にかける（CRC-8/ATM）。ファーム側は `src/debug_protocol.rs`

pub const SYNC: [u8; 2] = [0xa5, 0x5a];
pub const TYPE_FRAME: u8 = 0x01;
pub const TYPE_EVENT: u8 = 0x10;
pub const TYPE_INFO: u8 = 0x20;
pub const TYPE_TEXT: u8 = 0x7f;

pub const EVENT_NOTE_ON: u8 = 0x01;
pub const EVENT_NOTE_OFF: u8 = 0x02;
pub const EVENT_NOTE_MOVED: u8 = 0x03;
pub const EVENT_MARKER: u8 = 0x30;

const HEADER_LEN: usize = 5; // sync(2) + type(1) + len(2)
// これより長い len は同期の取り違えとみなす（96 キーの FRAME でも 220 バイト程度）
const MAX_PAYLOAD_LEN: usize = 4096;

/// 1 回のスキャンの生値
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub seq: u32,
    pub time_us: u32,
    pub valid: u128, // 読み取り成功フラグ（1 bit / キー）
    pub raw: Vec<u16>,
}

impl Frame {
    pub fn is_valid(&self, key: usize) -> bool {
        self.valid & (1u128 << key) != 0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub time_us: u32,
    pub kind: u8,
    pub data: [u8; 4],
}

impl Event {
    /// Note の位置（キー位置。ファームは ×100 の u16 で送る）
    pub fn location(&self) -> f32 {
        u16::from_le_bytes([self.data[2], self.data[3]]) as f32 / 100.0
    }

    pub fn marker_number(&self) -> u32 {
        u32::from_le_bytes(self.data)
    }

    /// 表示・CSV 用の短い説明
    pub fn describe(&self) -> String {
        let note = |name: &str| {
            format!(
                "{name} note={} vel={} loc={:.2}",
                self.data[0],
                self.data[1],
                self.location()
            )
        };
        match self.kind {
            EVENT_NOTE_ON => note("note_on"),
            EVENT_NOTE_OFF => note("note_off"),
            EVENT_NOTE_MOVED => note("note_moved"),
            EVENT_MARKER => format!("marker {}", self.marker_number()),
            kind => format!("event kind=0x{kind:02x} data={:02x?}", self.data),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub version: String,
    pub build_date: String,
    pub nkeys: u8,
    pub scan_period_us: u32,
    pub dropped: u32, // ファーム側のキュー溢れの累計
}

#[derive(Clone, Debug, PartialEq)]
pub enum Packet {
    Frame(Frame),
    Event(Event),
    Info(Info),
    Text(String),
    /// 知らない type（新しいファームなど）。読み飛ばす
    Unknown(u8),
}

/// 受信したバイト列からパケットを切り出す。途中から読み始めても同期を探して合わせる
#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    pub crc_errors: u64,    // crc が合わなかったパケット
    pub malformed: u64,     // crc は合ったが payload の長さが合わないパケット
    pub skipped_bytes: u64, // 同期を探すために読み飛ばしたバイト
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// バイト列を加え、切り出せたパケットを out に追加する
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Packet>) {
        self.buf.extend_from_slice(bytes);
        let mut pos = 0;
        loop {
            // 同期を探す
            let Some(offset) = self.buf[pos..].windows(2).position(|w| w == SYNC) else {
                // 残りの最後の 1 バイトは同期の前半かもしれないので残す
                let rest = &self.buf[pos..];
                let keep = usize::from(rest.last() == Some(&SYNC[0]));
                let skip = rest.len() - keep;
                self.skipped_bytes += skip as u64;
                pos += skip;
                break;
            };
            self.skipped_bytes += offset as u64;
            pos += offset;

            let rest = &self.buf[pos..];
            if rest.len() < HEADER_LEN {
                break;
            }
            let len = u16::from_le_bytes([rest[3], rest[4]]) as usize;
            if len > MAX_PAYLOAD_LEN {
                self.crc_errors += 1;
                pos += 1;
                continue;
            }
            let total = HEADER_LEN + len + 1;
            if rest.len() < total {
                break;
            }
            if crc8(&rest[2..HEADER_LEN + len]) != rest[HEADER_LEN + len] {
                // 同期の取り違えか、データの破損。1 バイト進めて同期を探し直す
                self.crc_errors += 1;
                pos += 1;
                continue;
            }
            match decode(rest[2], &rest[HEADER_LEN..HEADER_LEN + len]) {
                Some(packet) => out.push(packet),
                None => self.malformed += 1,
            }
            pos += total;
        }
        self.buf.drain(..pos);
    }
}

fn decode(packet_type: u8, p: &[u8]) -> Option<Packet> {
    let u32_at = |i: usize| u32::from_le_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]);
    match packet_type {
        TYPE_FRAME => {
            if p.len() < 9 {
                return None;
            }
            let nkeys = p[8] as usize;
            let valid_bytes = nkeys.div_ceil(8);
            if nkeys > 128 || p.len() != 9 + valid_bytes + 2 * nkeys {
                return None;
            }
            let mut valid_le = [0u8; 16];
            valid_le[..valid_bytes].copy_from_slice(&p[9..9 + valid_bytes]);
            let raw = p[9 + valid_bytes..]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Some(Packet::Frame(Frame {
                seq: u32_at(0),
                time_us: u32_at(4),
                valid: u128::from_le_bytes(valid_le),
                raw,
            }))
        }
        TYPE_EVENT => {
            if p.len() != 9 {
                return None;
            }
            Some(Packet::Event(Event {
                time_us: u32_at(0),
                kind: p[4],
                data: [p[5], p[6], p[7], p[8]],
            }))
        }
        TYPE_INFO => {
            if p.len() != 33 {
                return None;
            }
            Some(Packet::Info(Info {
                version: fixed_str(&p[0..8]),
                build_date: fixed_str(&p[8..24]),
                nkeys: p[24],
                scan_period_us: u32_at(25),
                dropped: u32_at(29),
            }))
        }
        TYPE_TEXT => Some(Packet::Text(String::from_utf8_lossy(p).into_owned())),
        other => Some(Packet::Unknown(other)),
    }
}

/// 0 で埋めた固定長の文字列
fn fixed_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// CRC-8/ATM（多項式 0x07、初期値 0、反転なし）。ファームと同じ
pub fn crc8(data: &[u8]) -> u8 {
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

/// u32 の µs（約 71 分で一周）を、一周を検出して u64 に伸ばす
#[derive(Default)]
pub struct TimeExtender {
    last: Option<u32>,
    wraps: u64,
}

impl TimeExtender {
    pub fn extend(&mut self, t: u32) -> u64 {
        const HALF: u32 = 1 << 31;
        let Some(last) = self.last else {
            self.last = Some(t);
            return t as u64;
        };
        if t < last && last - t > HALF {
            // 一周した
            self.wraps += 1;
            self.last = Some(t);
        } else if t > last && t - last > HALF {
            // 一周する前の、遅れて届いたパケット（FRAME と EVENT は別のキューから送られる）
            return ((self.wraps.saturating_sub(1)) << 32) | t as u64;
        } else if t > last {
            self.last = Some(t);
        }
        (self.wraps << 32) | t as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ファームの Packet::finish と同じ手順でパケットを作る
    fn packet(packet_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = SYNC.to_vec();
        out.push(packet_type);
        out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        out.extend_from_slice(payload);
        out.push(crc8(&out[2..]));
        out
    }

    fn frame_payload(seq: u32, time_us: u32, valid: u8, raw: &[u16]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&seq.to_le_bytes());
        p.extend_from_slice(&time_us.to_le_bytes());
        p.push(raw.len() as u8);
        p.push(valid);
        for r in raw {
            p.extend_from_slice(&r.to_le_bytes());
        }
        p
    }

    #[test]
    fn crc8_check_value() {
        // CRC-8 (poly 0x07, init 0) の検査値
        assert_eq!(crc8(b"123456789"), 0xf4);
    }

    #[test]
    fn parse_frame_split_and_garbage() {
        let raw = [100, 200, 300, 400, 500, 65535];
        let mut bytes = vec![0x00, 0xa5, 0x12]; // 途中から読み始めたときのごみ
        bytes.extend(packet(TYPE_FRAME, &frame_payload(7, 1234, 0b10_1111, &raw)));
        bytes.extend(packet(TYPE_TEXT, b"ok: start"));

        let mut parser = Parser::new();
        let mut out = Vec::new();
        // 1 バイトずつ渡しても切り出せること
        for b in &bytes {
            parser.push(&[*b], &mut out);
        }
        assert_eq!(out.len(), 2);
        let Packet::Frame(f) = &out[0] else { panic!() };
        assert_eq!((f.seq, f.time_us), (7, 1234));
        assert_eq!(f.raw, raw);
        assert!(f.is_valid(0) && !f.is_valid(4) && f.is_valid(5));
        assert_eq!(out[1], Packet::Text("ok: start".into()));
        assert_eq!(parser.crc_errors, 0);
    }

    #[test]
    fn parse_info_and_event() {
        let mut p = Vec::new();
        p.extend_from_slice(b"v0.3.0\0\0");
        p.extend_from_slice(b"26-09-30\0\0\0\0\0\0\0\0");
        p.push(6);
        p.extend_from_slice(&2000u32.to_le_bytes());
        p.extend_from_slice(&3u32.to_le_bytes());
        let mut bytes = packet(TYPE_INFO, &p);
        let mut e = 5000u32.to_le_bytes().to_vec();
        e.extend_from_slice(&[EVENT_NOTE_ON, 60, 100]);
        e.extend_from_slice(&1234u16.to_le_bytes());
        bytes.extend(packet(TYPE_EVENT, &e));

        let mut out = Vec::new();
        Parser::new().push(&bytes, &mut out);
        let Packet::Info(info) = &out[0] else {
            panic!()
        };
        assert_eq!(info.version, "v0.3.0");
        assert_eq!(info.build_date, "26-09-30");
        assert_eq!(
            (info.nkeys, info.scan_period_us, info.dropped),
            (6, 2000, 3)
        );
        let Packet::Event(ev) = &out[1] else { panic!() };
        assert_eq!(ev.describe(), "note_on note=60 vel=100 loc=12.34");
    }

    #[test]
    fn packet_ending_with_sync_byte() {
        // crc がちょうど 0xA5 になるパケットを探し、その直後で受信が区切れた場合
        let bytes = (0u8..=255)
            .map(|b| packet(TYPE_TEXT, &[b]))
            .find(|p| p.last() == Some(&SYNC[0]))
            .unwrap();
        let mut out = Vec::new();
        let mut parser = Parser::new();
        parser.push(&bytes, &mut out);
        parser.push(&packet(TYPE_TEXT, b"ok"), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(parser.skipped_bytes, 0);
    }

    #[test]
    fn crc_error_resyncs() {
        let mut bad = packet(TYPE_TEXT, b"abc");
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        bad.extend(packet(TYPE_TEXT, b"ok"));
        let mut out = Vec::new();
        let mut parser = Parser::new();
        parser.push(&bad, &mut out);
        assert_eq!(out, vec![Packet::Text("ok".into())]);
        assert_eq!(parser.crc_errors, 1);
    }

    #[test]
    fn time_wraps() {
        let mut ext = TimeExtender::default();
        assert_eq!(ext.extend(u32::MAX - 10), (u32::MAX - 10) as u64);
        assert_eq!(ext.extend(5), (1u64 << 32) + 5);
        // 一周する前の遅れたパケット
        assert_eq!(ext.extend(u32::MAX - 5), (u32::MAX - 5) as u64);
        assert_eq!(ext.extend(10), (1u64 << 32) + 10);
    }
}
