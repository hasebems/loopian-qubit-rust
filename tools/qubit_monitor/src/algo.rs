//! touch_algo（ノイズ除去・基準値・onset）を、受信・再生したフレームに通す（doc/debug_env.md §4.6）
//!
//! - Store のフレームを、間引いてから TouchSignal::process に 1 フレームずつ通し、結果を時刻と一緒にためる
//! - パラメータ・間引き・キー数が変わったら、Store の先頭から通し直す
//! - 校正のスイッチは PC のアルゴリズムだけのもので、ファームの SETTING_MODE とは連動しない。
//!   操作した時刻（そのとき最後に受け取ったフレームの時刻）を覚えておき、通し直すときも同じ時刻で切り替える
use std::collections::VecDeque;

use touch_algo::{KeyState, Params, State, TouchSignal};

use crate::store::{MAX_FRAMES, Store};

/// 間引きの周期（µs）。0 は間引かない
pub const DECIMATIONS_US: [u32; 3] = [0, 8_000, 10_000];
/// 間引いた周期を前後このくらいずれても通す（2ms 周期のフレームの揺れを吸収する）
const DECIMATION_TOLERANCE_US: u64 = 1_000;
const MAX_ONSETS: usize = 100_000;

pub struct AlgoRunner {
    pub params: Params,
    pub decimation_us: u32,
    signal: Option<TouchSignal<Vec<KeyState>>>,
    nkeys: usize,
    next_abs: u64,            // 次に通すフレームの通し番号（Store に入った順の番号）
    next_due_us: Option<u64>, // 間引き: 次に通すフレームの時刻
    calib_toggles: Vec<(u64, bool)>, // 校正のスイッチを操作した時刻と、操作後の状態
    pub calibrating: bool,

    // 結果（通したフレームだけ。時刻の順）
    pub time_us: VecDeque<u64>,
    pub filtered: Vec<VecDeque<u16>>,
    pub baseline: Vec<VecDeque<u16>>,
    pub delta: Vec<VecDeque<i32>>,
    pub output: Vec<VecDeque<u16>>,
    pub onsets: VecDeque<(u64, usize)>, // onset を記録した時刻とキー
    pub noise_floor: Vec<u16>,          // 最後のフレームでのキー毎の noise_floor
}

impl AlgoRunner {
    pub fn new() -> Self {
        Self {
            params: Params::DEFAULT,
            decimation_us: 8_000,
            signal: None,
            nkeys: 0,
            next_abs: 0,
            next_due_us: None,
            calib_toggles: Vec::new(),
            calibrating: false,
            time_us: VecDeque::new(),
            filtered: Vec::new(),
            baseline: Vec::new(),
            delta: Vec::new(),
            output: Vec::new(),
            onsets: VecDeque::new(),
            noise_floor: Vec::new(),
        }
    }

    pub fn state(&self) -> Option<State> {
        self.signal.as_ref().map(|s| s.state())
    }

    /// 結果を捨て、次の update で Store の先頭から通し直す（パラメータ・間引きを変えたとき）
    pub fn invalidate(&mut self) {
        self.signal = None;
    }

    /// 新しいデータ（接続・記録を開いた）: 結果も校正のスイッチの記録も捨てる
    pub fn clear(&mut self) {
        self.invalidate();
        self.calib_toggles.clear();
        self.calibrating = false;
    }

    /// 校正のスイッチを切り替える。時刻は Store の最後のフレームの時刻にする
    pub fn set_calibrating(&mut self, on: bool, store: &Store) {
        if on == self.calibrating {
            return;
        }
        self.calibrating = on;
        let t = store.latest_time_us().unwrap_or(0);
        self.calib_toggles.push((t, on));
    }

    /// その時刻の校正のスイッチの状態（操作した時刻より後のフレームから切り替わる）
    fn calibrating_at(&self, t: u64) -> bool {
        self.calib_toggles
            .iter()
            .rev()
            .find(|(when, _)| *when < t)
            .is_some_and(|(_, on)| *on)
    }

    /// Store に新しく来たフレームを通す。必要なら先頭から通し直す
    pub fn update(&mut self, store: &Store) {
        let first_abs = store.frames_total - store.time_us.len() as u64;
        let restart = self.signal.is_none()
            || self.nkeys != store.nkeys
            || self.next_abs > store.frames_total; // Store が作り直された
        if restart {
            self.restart(store.nkeys);
            self.next_abs = first_abs;
        }
        // 通す前に Store から捨てられたフレームは飛ばす
        self.next_abs = self.next_abs.max(first_abs);

        while self.next_abs < store.frames_total {
            let i = (self.next_abs - first_abs) as usize;
            self.next_abs += 1;
            let t = store.time_us[i];
            if !self.due(t) {
                continue;
            }
            let calibrating = self.calibrating_at(t);
            let raw: Vec<u16> = (0..self.nkeys).map(|k| store.raw[k][i]).collect();
            let Some(signal) = self.signal.as_mut() else {
                return;
            };
            signal.process(t as u32, &raw, store.valid[i], calibrating);
            self.push_result(t);
        }
    }

    /// 間引き: このフレームを通すか
    fn due(&mut self, t: u64) -> bool {
        if self.decimation_us == 0 {
            return true;
        }
        let period = self.decimation_us as u64;
        match self.next_due_us {
            Some(due) if t + DECIMATION_TOLERANCE_US < due => false,
            Some(due) => {
                // 遅れたときは、今のフレームから数え直す
                self.next_due_us = Some(if t > due + period {
                    t + period
                } else {
                    due + period
                });
                true
            }
            None => {
                self.next_due_us = Some(t + period);
                true
            }
        }
    }

    fn restart(&mut self, nkeys: usize) {
        self.nkeys = nkeys;
        self.signal = Some(TouchSignal::new(
            vec![KeyState::INITIAL; nkeys],
            self.params,
        ));
        self.next_due_us = None;
        self.time_us.clear();
        self.filtered = vec![VecDeque::new(); nkeys];
        self.baseline = vec![VecDeque::new(); nkeys];
        self.delta = vec![VecDeque::new(); nkeys];
        self.output = vec![VecDeque::new(); nkeys];
        self.onsets.clear();
        self.noise_floor = vec![0; nkeys];
    }

    fn push_result(&mut self, t: u64) {
        let Some(signal) = &self.signal else {
            return;
        };
        if self.time_us.len() >= MAX_FRAMES {
            self.time_us.pop_front();
            for k in 0..self.nkeys {
                self.filtered[k].pop_front();
                self.baseline[k].pop_front();
                self.delta[k].pop_front();
                self.output[k].pop_front();
            }
        }
        self.time_us.push_back(t);
        for (k, key) in signal.keys().iter().enumerate() {
            self.filtered[k].push_back(key.filtered());
            self.baseline[k].push_back(key.baseline());
            self.delta[k].push_back(key.delta());
            self.output[k].push_back(key.output());
            self.noise_floor[k] = key.noise_floor();
            if key.onset_us() == Some(t as u32) {
                if self.onsets.len() >= MAX_ONSETS {
                    self.onsets.pop_front();
                }
                self.onsets.push_back((t, k));
            }
        }
    }

    /// 時刻 [t0, t1] に入る結果の添字の範囲
    pub fn range(&self, t0: u64, t1: u64) -> std::ops::Range<usize> {
        let start = self.time_us.partition_point(|&t| t < t0);
        let end = self.time_us.partition_point(|&t| t <= t1);
        start..end.max(start)
    }
}

impl Default for AlgoRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Frame, Packet};

    fn store_with(frames: &[(u32, u16)]) -> Store {
        let mut store = Store::new();
        for (seq, &(t, v)) in frames.iter().enumerate() {
            store.ingest(Packet::Frame(Frame {
                seq: seq as u32,
                time_us: t,
                valid: 0b11,
                raw: vec![v, v],
            }));
        }
        store
    }

    #[test]
    fn decimates_2ms_frames_to_8ms() {
        let frames: Vec<(u32, u16)> = (0..41).map(|i| (i * 2000, 900)).collect();
        let store = store_with(&frames);
        let mut runner = AlgoRunner::new();
        runner.update(&store);
        let times: Vec<u64> = runner.time_us.iter().copied().collect();
        assert_eq!(
            times,
            vec![
                0, 8000, 16000, 24000, 32000, 40000, 48000, 56000, 64000, 72000, 80000
            ]
        );
    }

    #[test]
    fn processes_incrementally_and_restarts_on_new_store() {
        let mut store = store_with(&[(0, 900), (8000, 900)]);
        let mut runner = AlgoRunner::new();
        runner.update(&store);
        assert_eq!(runner.time_us.len(), 2);
        store.ingest(Packet::Frame(Frame {
            seq: 2,
            time_us: 16000,
            valid: 0b11,
            raw: vec![900, 900],
        }));
        runner.update(&store);
        assert_eq!(runner.time_us.len(), 3);

        let store = store_with(&[(0, 900)]);
        runner.update(&store);
        assert_eq!(runner.time_us.len(), 1);
    }

    #[test]
    fn calibration_switch_is_replayed_at_the_same_time() {
        let frames: Vec<(u32, u16)> = (0..200).map(|i| (i * 8000, 900)).collect();
        let store = store_with(&frames[..100]);
        let mut runner = AlgoRunner::new();
        runner.update(&store);
        runner.set_calibrating(true, &store); // 時刻 792000 で校正に入る
        let store = store_with(&frames);
        runner.update(&store);
        assert_eq!(runner.state(), Some(State::Calibrating));
        // パラメータを変えて通し直しても、同じ時刻で校正に入る
        runner.invalidate();
        runner.update(&store);
        assert_eq!(runner.state(), Some(State::Calibrating));
    }
}
