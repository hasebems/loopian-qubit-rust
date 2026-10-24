//! 受信（または再生）したデータの保持と、統計の計算
//!
//! 実機からの受信と記録の再生は、どちらもパケットを `Store::ingest` に渡す（同じ経路）
use std::collections::VecDeque;

use crate::protocol::{Event, Info, Packet, TimeExtender};

/// 保持するフレームの上限。2ms 周期で約 33 分。超えたら古いものから捨てる
pub const MAX_FRAMES: usize = 1_000_000;
const MAX_EVENTS: usize = 100_000;
const MAX_TEXTS: usize = 50;

#[derive(Default)]
pub struct Store {
    pub nkeys: usize,
    pub time_us: VecDeque<u64>, // 起動からの µs（一周を伸ばした値）
    pub seq: VecDeque<u32>,
    pub valid: VecDeque<u128>,
    pub raw: Vec<VecDeque<u16>>, // キー毎の生値
    pub events: VecDeque<(u64, Event)>,
    pub info: Option<Info>,
    pub texts: VecDeque<String>, // ファームからの TEXT（新しいものが後ろ）

    // 累計の数（接続・読み込みのたびにリセット）
    pub frames_total: u64,
    pub lost_frames: u64, // seq の欠け（取りこぼし）
    last_seq: Option<u32>,
    time: TimeExtender,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn latest_time_us(&self) -> Option<u64> {
        self.time_us.back().copied()
    }

    pub fn ingest(&mut self, packet: Packet) {
        match packet {
            Packet::Frame(frame) => {
                let nkeys = frame.raw.len();
                if nkeys != self.nkeys {
                    // キー数が変わった（別の構成のファーム）。それまでのフレームは捨てる
                    self.reset_frames(nkeys);
                }
                let t = self.time.extend(frame.time_us);
                if let Some(last) = self.last_seq {
                    let gap = frame.seq.wrapping_sub(last).wrapping_sub(1);
                    // 大きく戻ったとき（ファームの再起動など）は欠けに数えない
                    if gap < 1_000_000 {
                        self.lost_frames += gap as u64;
                    }
                }
                self.last_seq = Some(frame.seq);
                self.frames_total += 1;

                if self.time_us.len() >= MAX_FRAMES {
                    self.time_us.pop_front();
                    self.seq.pop_front();
                    self.valid.pop_front();
                    for k in 0..self.nkeys {
                        self.raw[k].pop_front();
                    }
                }
                self.time_us.push_back(t);
                self.seq.push_back(frame.seq);
                self.valid.push_back(frame.valid);
                for (k, &raw) in frame.raw.iter().enumerate() {
                    self.raw[k].push_back(raw);
                }
            }
            Packet::Event(event) => {
                let t = self.time.extend(event.time_us);
                if self.events.len() >= MAX_EVENTS {
                    self.events.pop_front();
                }
                self.events.push_back((t, event));
            }
            Packet::Info(info) => self.info = Some(info),
            Packet::Text(text) => {
                if self.texts.len() >= MAX_TEXTS {
                    self.texts.pop_front();
                }
                self.texts.push_back(text);
            }
            Packet::Unknown(_) => {}
        }
    }

    fn reset_frames(&mut self, nkeys: usize) {
        self.nkeys = nkeys;
        self.time_us.clear();
        self.seq.clear();
        self.valid.clear();
        self.raw = vec![VecDeque::new(); nkeys];
        self.last_seq = None;
    }

    /// 時刻 [t0, t1] に入るフレームの添字の範囲
    pub fn range(&self, t0: u64, t1: u64) -> std::ops::Range<usize> {
        let start = self.time_us.partition_point(|&t| t < t0);
        let end = self.time_us.partition_point(|&t| t <= t1);
        start..end.max(start)
    }

    pub fn is_valid(&self, index: usize, key: usize) -> bool {
        self.valid[index] & (1u128 << key) != 0
    }

    /// 時刻 [t0, t1] の統計
    pub fn stats(&self, t0: u64, t1: u64) -> Stats {
        let range = self.range(t0, t1);
        let mut stats = Stats {
            frames: range.len(),
            ..Default::default()
        };

        // キー毎のノイズ（標準偏差・p-p）と、値が変わる間隔
        for (k, values) in self.raw.iter().enumerate() {
            let mut n = 0u64;
            let (mut sum, mut sum2) = (0f64, 0f64);
            let (mut min, mut max) = (u16::MAX, 0u16);
            for i in range.clone() {
                if !self.is_valid(i, k) {
                    continue;
                }
                let v = values[i];
                n += 1;
                sum += v as f64;
                sum2 += (v as f64) * (v as f64);
                min = min.min(v);
                max = max.max(v);
            }
            let mean = if n > 0 { sum / n as f64 } else { 0.0 };
            let var = if n > 1 {
                (sum2 - sum * sum / n as f64) / (n - 1) as f64
            } else {
                0.0
            };
            stats.keys.push(KeyStats {
                samples: n,
                mean,
                std: var.max(0.0).sqrt(),
                peak_to_peak: if n > 0 { max - min } else { 0 },
            });
        }

        // サンプル間隔（seq が連続している組だけ）
        let mut intervals = Vec::new();
        for i in range.clone().skip(1) {
            if self.seq[i].wrapping_sub(self.seq[i - 1]) == 1 {
                intervals.push(self.time_us[i] - self.time_us[i - 1]);
            }
        }
        stats.interval_us = Summary::of(&intervals);
        stats.interval_hist = histogram(&intervals, INTERVAL_BIN_US);

        // seq の欠け
        stats.lost_frames = range
            .clone()
            .skip(1)
            .map(|i| {
                let gap = self.seq[i].wrapping_sub(self.seq[i - 1]).wrapping_sub(1);
                if gap < 1_000_000 { gap as u64 } else { 0 }
            })
            .sum();
        stats
    }

    /// キー k の生値が変わる間隔（µs）。チップが実際に値を更新する周期を見るため
    pub fn change_intervals(&self, t0: u64, t1: u64, key: usize) -> Vec<u64> {
        let mut out = Vec::new();
        let mut last: Option<(u64, u16)> = None; // 最後に値が変わった時刻と、その値
        for i in self.range(t0, t1) {
            if !self.is_valid(i, key) {
                continue;
            }
            let (t, v) = (self.time_us[i], self.raw[key][i]);
            match last {
                Some((t_prev, v_prev)) if v != v_prev => {
                    out.push(t - t_prev);
                    last = Some((t, v));
                }
                None => last = Some((t, v)),
                _ => {}
            }
        }
        out
    }
}

pub const INTERVAL_BIN_US: u64 = 250;

/// 幅 bin_us のビンで数えたヒストグラム（ビンの下端 µs, 数）
pub fn histogram(values: &[u64], bin_us: u64) -> Vec<(u64, u32)> {
    let mut bins: std::collections::BTreeMap<u64, u32> = Default::default();
    for &v in values {
        *bins.entry(v / bin_us * bin_us).or_default() += 1;
    }
    bins.into_iter().collect()
}

#[derive(Default)]
pub struct Stats {
    pub frames: usize,
    pub lost_frames: u64,
    pub keys: Vec<KeyStats>,
    pub interval_us: Summary,
    pub interval_hist: Vec<(u64, u32)>,
}

#[derive(Default)]
pub struct KeyStats {
    pub samples: u64,
    pub mean: f64,
    pub std: f64,
    pub peak_to_peak: u16,
}

#[derive(Default, Clone, Copy)]
pub struct Summary {
    pub count: usize,
    pub min: u64,
    pub mean: f64,
    pub max: u64,
}

impl Summary {
    pub fn of(values: &[u64]) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        Self {
            count: values.len(),
            min: *values.iter().min().unwrap(),
            mean: values.iter().sum::<u64>() as f64 / values.len() as f64,
            max: *values.iter().max().unwrap(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Frame;

    fn frame(seq: u32, time_us: u32, raw: Vec<u16>) -> Packet {
        Packet::Frame(Frame {
            seq,
            time_us,
            valid: (1u128 << raw.len()) - 1,
            raw,
        })
    }

    #[test]
    fn counts_lost_frames() {
        let mut store = Store::new();
        store.ingest(frame(0, 0, vec![500, 500]));
        store.ingest(frame(1, 2000, vec![500, 502]));
        store.ingest(frame(4, 8000, vec![500, 505])); // seq 2, 3 が欠け
        assert_eq!(store.lost_frames, 2);

        let stats = store.stats(0, 10_000);
        assert_eq!(stats.frames, 3);
        assert_eq!(stats.lost_frames, 2);
        assert_eq!(stats.interval_us.count, 1); // seq が連続しているのは 0→1 だけ
        assert_eq!(stats.interval_us.min, 2000);
    }

    #[test]
    fn change_intervals_follow_value_updates() {
        let mut store = Store::new();
        // 2ms 毎に読み、値は 8ms 毎に変わる
        for i in 0..9u32 {
            store.ingest(frame(i, i * 2000, vec![100 + (i / 4) as u16]));
        }
        assert_eq!(store.change_intervals(0, 100_000, 0), vec![8000, 8000]);
    }
}
