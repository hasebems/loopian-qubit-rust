//! 記録ファイル（.qlog）の再生（doc/debug_env.md §4.3）
//!
//! 記録したバイト列を受信と同じ Parser にかけ、パケットを時刻に合わせて Store::ingest に流す
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::protocol::{Packet, Parser, TimeExtender};
use crate::qlog;
use crate::store::Store;

/// 1 回の update で流すパケットの上限（最高速のときに UI が固まらないように）
const MAX_PACKETS_PER_UPDATE: usize = 20_000;

pub const SPEEDS: [f64; 6] = [0.25, 1.0, 2.0, 4.0, 16.0, 64.0];

pub struct Player {
    pub path: PathBuf,
    pub header: qlog::Header,
    packets: Vec<(Option<u64>, Packet)>, // (時刻 µs, パケット)。INFO・TEXT は時刻なし
    pub crc_errors: u64,
    next: usize,
    pub playing: bool,
    pub speed: f64,
    // 再生の基準: この壁時計の時刻に、記録の時刻 anchor_us を流していた
    anchor: Option<(Instant, u64)>,
    current_us: u64, // 最後に流したパケットの時刻
    first_us: u64,
    last_us: u64,
}

impl Player {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let (header, bytes) = qlog::read(path)?;
        let mut parser = Parser::new();
        let mut parsed = Vec::new();
        parser.push(&bytes, &mut parsed);
        let mut time = TimeExtender::default();
        let packets: Vec<_> = parsed
            .into_iter()
            .map(|p| {
                let t = match &p {
                    Packet::Frame(f) => Some(time.extend(f.time_us)),
                    Packet::Event(e) => Some(time.extend(e.time_us)),
                    _ => None,
                };
                (t, p)
            })
            .collect();
        let times = packets.iter().filter_map(|(t, _)| *t);
        let first_us = times.clone().min().unwrap_or(0);
        let last_us = times.max().unwrap_or(0);
        Ok(Self {
            path: path.to_path_buf(),
            header,
            packets,
            crc_errors: parser.crc_errors,
            next: 0,
            playing: false,
            speed: 1.0,
            anchor: None,
            current_us: first_us,
            first_us,
            last_us,
        })
    }

    pub fn duration_us(&self) -> u64 {
        self.last_us - self.first_us
    }

    /// 再生した位置（記録の先頭からの µs）
    pub fn position_us(&self) -> u64 {
        self.current_us.saturating_sub(self.first_us)
    }

    pub fn is_finished(&self) -> bool {
        self.next >= self.packets.len()
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        self.anchor = None; // 再開したときは今の位置から数え直す
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
        self.anchor = None;
    }

    /// 最初から再生し直す（Store は呼び出し側で作り直す）
    pub fn rewind(&mut self) {
        self.next = 0;
        self.current_us = self.first_us;
        self.anchor = None;
    }

    /// 再生中なら、壁時計の経過 × 速さの分だけパケットを流す
    pub fn update(&mut self, store: &mut Store) {
        if !self.playing {
            return;
        }
        let now = Instant::now();
        let (anchor_wall, anchor_us) = *self.anchor.get_or_insert((now, self.current_us));
        let target_us =
            anchor_us + (now.duration_since(anchor_wall).as_secs_f64() * 1e6 * self.speed) as u64;
        self.feed_until(store, |t| t <= target_us);
        if self.is_finished() {
            self.playing = false;
        }
    }

    /// コマ送り: 次のフレームを 1 つ流す（その前のイベントなども流す）
    pub fn step(&mut self, store: &mut Store) {
        self.playing = false;
        while let Some((t, packet)) = self.packets.get(self.next) {
            let is_frame = matches!(packet, Packet::Frame(_));
            if let Some(t) = t {
                self.current_us = self.current_us.max(*t);
            }
            store.ingest(packet.clone());
            self.next += 1;
            if is_frame {
                break;
            }
        }
    }

    fn feed_until(&mut self, store: &mut Store, due: impl Fn(u64) -> bool) {
        let mut fed = 0;
        while let Some((t, packet)) = self.packets.get(self.next) {
            if let Some(t) = t {
                if !due(*t) {
                    break;
                }
                self.current_us = self.current_us.max(*t);
            }
            store.ingest(packet.clone());
            self.next += 1;
            fed += 1;
            if fed >= MAX_PACKETS_PER_UPDATE {
                // 追いつけないときは基準を今に合わせ直し、早送りしすぎないようにする
                self.anchor = None;
                break;
            }
        }
    }
}
