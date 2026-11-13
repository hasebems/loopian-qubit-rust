//! touch_algo（ノイズ除去・基準値と、タッチの解析）を、受信・再生したフレームに通す（doc/debug_env.md §4.6）
//!
//! - Store のフレームを、間引いてから ElectrodeSignal::process に 1 フレームずつ通し、結果を時刻と一緒にためる
//! - 同じフレームで、その output を TouchTracker（touch_algo::touch）→ PlayInfo（touch_algo::playinfo）の
//!   順に通し、タッチポイント毎の position・depth と、出てきたイベント（Note On/Off/Moved）をためる
//!   （doc/touch_analysis.md §5・§7.1）。
//!   ファームは信号処理と解析を 10ms 毎に行うので、同じ条件にするには間引きを 10ms にする。動作モードは無い（2026-11-13。Violin の形）
//! - パラメータ・間引き・キー数が変わったら、Store の先頭から通し直す
//! - 校正のスイッチは PC のアルゴリズムだけのもので、ファームの SETTING_MODE とは連動しない。
//!   操作した時刻（そのとき最後に受け取ったフレームの時刻）を覚えておき、通し直すときも同じ時刻で切り替える
use std::cell::RefCell;
use std::collections::VecDeque;

use touch_algo::electrode::{ElectrodeSignal, ElectrodeState, Params, State};
use touch_algo::playinfo::{
    PlayInfo, STATUS_CC, STATUS_NOTE_MOVED, STATUS_NOTE_OFF, STATUS_NOTE_ON,
};
use touch_algo::touch::{MAX_TOUCH_POINTS, TouchTracker};

use crate::store::{MAX_FRAMES, Store};

/// 間引きの周期（µs）。0 は間引かない
pub const DECIMATIONS_US: [u32; 3] = [0, 8_000, 10_000];
/// 間引いた周期を前後このくらいずれても通す（2ms 周期のフレームの揺れを吸収する）
const DECIMATION_TOLERANCE_US: u64 = 1_000;
/// 解析のイベントをためる数の上限（古いものから捨てる）
const MAX_TOUCH_EVENTS: usize = 10_000;
/// 電極がリングに並ぶ（最後の電極と電極 0 が隣り合う）キー数。96 電極の本番の基板だけで、
/// etude（12）・no_pca9544（6）の試験用の基板は一列（ファームの TOUCH_RING と同じ）
const RING_KEYS: usize = 96;

/// 解析 1 回分の、タッチポイント毎の (position, depth)。触れていなければ None
pub type TouchSnapshot = [Option<(f32, i16)>; MAX_TOUCH_POINTS];

/// 解析（PlayInfo）が出したイベント
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchEvent {
    pub time_us: u64,
    pub status: u8, // 上位 4 ビットが STATUS_*、下位 4 ビットはタッチポイントの id
    pub note: u8,
    pub velocity: u8,
    pub location: f32,
}

impl TouchEvent {
    /// 表示用の短い説明（ファームの EVENT の `Event::describe` と同じ形）
    pub fn describe(&self) -> String {
        let note = |name: &str| {
            format!(
                "{name} note={} vel={} loc={:.2}",
                self.note, self.velocity, self.location
            )
        };
        match self.status & 0xf0 {
            STATUS_NOTE_ON => note("note_on"),
            STATUS_NOTE_OFF => note("note_off"),
            STATUS_NOTE_MOVED => note("note_moved"),
            STATUS_CC => format!("cc {}={}", self.note, self.velocity),
            s => format!("status=0x{s:02x} {} {}", self.note, self.velocity),
        }
    }
}

/// PlayInfo のコールバック。AlgoRunner が PlayInfo を持つので、コールバックは AlgoRunner を借りられない。
/// 何も持たない fn にして、イベントはスレッドローカルに置き、解析の後に取り出す
type TouchCallback = fn(u8, u8, u8, f32);

thread_local! {
    static PENDING_EVENTS: RefCell<Vec<(u8, u8, u8, f32)>> = const { RefCell::new(Vec::new()) };
}

fn on_touch_event(status: u8, note: u8, velocity: u8, location: f32) {
    PENDING_EVENTS.with_borrow_mut(|e| e.push((status, note, velocity, location)));
}

pub struct AlgoRunner {
    pub params: Params,
    pub decimation_us: u32,
    signal: Option<ElectrodeSignal<Vec<ElectrodeState>>>,
    touch: Option<TouchTracker<Vec<u16>>>, // depth・position の層
    play: Option<PlayInfo<TouchCallback>>, // 演奏情報の層
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
    pub noise_floor: Vec<u16>, // 最後のフレームでのキー毎の noise_floor
    pub touches: VecDeque<TouchSnapshot>, // 解析の結果（time_us と同じ添字）
    pub touch_events: VecDeque<TouchEvent>, // 解析が出したイベント（時刻の順）
}

impl AlgoRunner {
    pub fn new() -> Self {
        Self {
            params: Params::DEFAULT,
            decimation_us: 8_000,
            signal: None,
            touch: None,
            play: None,
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
            noise_floor: Vec::new(),
            touches: VecDeque::new(),
            touch_events: VecDeque::new(),
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
            if let (Some(touch), Some(play)) = (self.touch.as_mut(), self.play.as_mut()) {
                for (k, key) in signal.keys().iter().enumerate() {
                    touch.set_value(k, key.output());
                }
                touch.update(t as u32);
                play.update(touch);
            }
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
        self.signal = Some(ElectrodeSignal::new(
            vec![ElectrodeState::INITIAL; nkeys],
            self.params,
        ));
        self.touch = Some(TouchTracker::new(vec![0; nkeys], nkeys == RING_KEYS));
        self.play = Some(PlayInfo::new(on_touch_event as TouchCallback));
        PENDING_EVENTS.with_borrow_mut(|e| e.clear());
        self.next_due_us = None;
        self.time_us.clear();
        self.filtered = vec![VecDeque::new(); nkeys];
        self.baseline = vec![VecDeque::new(); nkeys];
        self.delta = vec![VecDeque::new(); nkeys];
        self.output = vec![VecDeque::new(); nkeys];
        self.noise_floor = vec![0; nkeys];
        self.touches.clear();
        self.touch_events.clear();
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
            self.touches.pop_front();
        }
        self.time_us.push_back(t);
        for (k, key) in signal.keys().iter().enumerate() {
            self.filtered[k].push_back(key.filtered());
            self.baseline[k].push_back(key.baseline());
            self.delta[k].push_back(key.delta());
            self.output[k].push_back(key.output());
            self.noise_floor[k] = key.noise_floor();
        }

        // 解析: タッチポイント毎の position・depth と、この解析で出たイベント
        let snapshot: TouchSnapshot = core::array::from_fn(|id| {
            self.touch
                .as_ref()
                .and_then(|touch| touch.touch_point(id))
                .filter(|tp| tp.is_touched())
                .map(|tp| (tp.get_location(), tp.get_intensity()))
        });
        self.touches.push_back(snapshot);
        let pending = PENDING_EVENTS.with_borrow_mut(std::mem::take);
        for (status, note, velocity, location) in pending {
            if self.touch_events.len() >= MAX_TOUCH_EVENTS {
                self.touch_events.pop_front();
            }
            self.touch_events.push_back(TouchEvent {
                time_us: t,
                status,
                note,
                velocity,
                location,
            });
        }
    }

    /// 時刻 [t0, t1] に入る結果の添字の範囲
    pub fn range(&self, t0: u64, t1: u64) -> std::ops::Range<usize> {
        let start = self.time_us.partition_point(|&t| t < t0);
        let end = self.time_us.partition_point(|&t| t <= t1);
        start..end.max(start)
    }

    /// 時刻 [t0, t1] の、キー k の filtered の p-p（固着判定の STUCK_VARIATION と比べるため）。
    /// 結果が無ければ None
    pub fn filtered_peak_to_peak(&self, t0: u64, t1: u64, k: usize) -> Option<u16> {
        let values = self.filtered.get(k)?;
        let range = self.range(t0, t1);
        let min = range.clone().map(|i| values[i]).min()?;
        let max = range.map(|i| values[i]).max()?;
        Some(max - min)
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
    fn filtered_peak_to_peak_in_range() {
        // 8ms 毎に 900 → 0.4 秒後から 906（移動平均 2 なので 903 を経て 906 になる）
        let frames: Vec<(u32, u16)> = (0..100)
            .map(|i| (i * 8000, if i < 50 { 900 } else { 906 }))
            .collect();
        let store = store_with(&frames);
        let mut runner = AlgoRunner::new();
        runner.update(&store);
        assert_eq!(runner.filtered_peak_to_peak(0, 300_000, 0), Some(0));
        assert_eq!(runner.filtered_peak_to_peak(0, 800_000, 0), Some(6));
        assert_eq!(runner.filtered_peak_to_peak(0, 800_000, 5), None); // 無いキー
        assert_eq!(runner.filtered_peak_to_peak(900_000, 950_000, 0), None); // 範囲外
    }

    #[test]
    fn touch_analysis_runs_on_output() {
        // 起動直後（初期取得・自動計測の約 3.3 秒）は output が 0 なので、4 秒後から 3 キーに触れる
        let mut store = Store::new();
        for i in 0..800u32 {
            let t = i * 10_000;
            let touched = (400..450).contains(&i);
            let raw = (0..12)
                .map(|k| match (touched, k) {
                    (true, 4 | 6) => 940,
                    (true, 5) => 980,
                    _ => 900,
                })
                .collect();
            store.ingest(Packet::Frame(Frame {
                seq: i,
                time_us: t,
                valid: (1 << 12) - 1,
                raw,
            }));
        }
        let mut runner = AlgoRunner::new();
        runner.decimation_us = 10_000;
        runner.update(&store);
        assert_eq!(runner.touches.len(), runner.time_us.len());
        let kinds: Vec<u8> = runner
            .touch_events
            .iter()
            .map(|e| e.status & 0xf0)
            .collect();
        assert_eq!(kinds, vec![STATUS_NOTE_ON, STATUS_NOTE_OFF]);
        let on = runner.touch_events[0];
        // 電極 2 つで 1 ノート: position 5.0 / 2 = 2.5 → ノート 3（四捨五入）
        assert_eq!(on.note, 3 + touch_algo::playinfo::NOTE_OFFSET);
        assert!((on.location - 5.0).abs() < 0.5);
        // 触れている間はタッチポイント 0 に position がある
        let i = runner.time_us.partition_point(|&t| t < 4_200_000);
        assert!(runner.touches[i][0].is_some());
        assert!(runner.touches.back().unwrap()[0].is_none());
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
