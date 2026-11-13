//  Created by Hasebe Masahiko on 2026/02/15.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! 演奏情報の層（doc/touch_analysis.md §4）
//!
//! `touch`（depth・position の層）の結果（指毎の position・depth・`Track`）から、
//! Note On/Off・Move・ビブラートなどの演奏情報を作り、コールバックで出す（`PlayInfo`）。
//! 電極の output は見ない。リングのつなぎ目での扱いはここで決める（§4.1）
//!
//! - 10ms 毎に、`TouchTracker::update` の後で `update` を呼ぶ前提（`TOUCH_SAMPLE_PERIOD_SEC` など）
//! - 共有状態（ファームの `TOUCH0-3`・`ANY_TOUCH` など）には書かない。呼び出し側が読んで書く
//! - 動作モード（Piano / Violin）は 2026-11-13 に無くした。今は Violin の形（電極 2 つで 1 ノート）だけ。
//!   Piano の処理は後で足す

use crate::touch::{Layout, MAX_TOUCH_POINTS, TouchTracker, Track};

// =========================================================
//      Constants
// =========================================================
pub const HISTERESIS: f32 = 0.7; // Hysteresis value for touch point detection

// MIDI
/// 電極この数で 1 ノート（Violin の形。指を電極 2 つ分動かすと半音 1 つ変わる）
pub const ELECTRODES_PER_NOTE: u16 = 2;
/// ノート 0 の MIDI ノート番号（Violin の形）
pub const NOTE_OFFSET: u8 = 55;

// コールバックの status（上位 4 ビット。下位 4 ビットは TouchPoint の id、CC では 0）
pub const STATUS_NOTE_ON: u8 = 0x90; // Note On
pub const STATUS_NOTE_OFF: u8 = 0x80; // Note Off
pub const STATUS_NOTE_MOVED: u8 = 0xa0; // ノートが変わったときの古いノートの Note Off
pub const STATUS_CC: u8 = 0xb0; // Control Change

// Note On の出し方（doc/touch_analysis.md §4.2「Note On の出し方」）。2026-11-13、数値の見当はつけずに、まず試す値。
// ベロシティで変えたいのは音量ではなく、音の立ち上がり。速い側は頭打ちにし、遅い側の違いを取る
/// depth がこれを超えた周期に Note On を出す。ベロシティは、指を検出してからここまでにかかった時間で決める
pub const NOTE_ON_DEPTH: i16 = 70;
/// NOTE_ON_DEPTH に届かずに depth が下がったとき、それまでの最大の depth がこれを超えていれば「はじいた」として
/// 速いベロシティで Note On を出す。超えていなければ（かすっただけ）、消えても何も出さない
pub const FLICK_DEPTH: i16 = 50;
/// NOTE_ON_DEPTH に届かず、下がりもしないまま（軽く触れたまま）この周期数が過ぎたら、遅いベロシティで Note On（300ms）
pub const NOTE_ON_TIMEOUT_CYCLES: u32 = 30;
/// 指を検出した周期（経過 0）のベロシティ。はじいたときもこれ
pub const VELOCITY_FAST: u8 = 100;
/// NOTE_ON_TIMEOUT_CYCLES のベロシティ。その間は時間で線形に補間する
pub const VELOCITY_SLOW: u8 = 30;
/// Move（指が動いてノートが変わった）の Note On のベロシティ。固定（2026-11-13、ユーザー）
pub const VELOCITY_MOVE: u8 = 50;

const HISTORY_SIZE: usize = 128; // Number of past locations
const TOUCH_SAMPLE_PERIOD_SEC: f32 = 0.01; // 10ms tick
const FEW_HZ_MIN: f32 = 1.5;
const FEW_HZ_MAX: f32 = 8.0;
const OSC_MIN_PEAK_TO_PEAK: f32 = 0.3;
const OSC_DEADBAND: f32 = 0.1;

const NEW_NOTE: u8 = 0xff;
const TOUCH_POINT_ERROR: u8 = 0xfe;

// =========================================================
//      position → ノート、時間 → ベロシティ
// =========================================================
/// position からノート（0〜slots−1 の番号）を決める。crnt は今のノート（NEW_NOTE は新しいタッチ）。
/// 今のノートから HISTERESIS を越えて離れたときだけ変える。リングでは距離を近い向きに測り、
/// 四捨五入で slots になったら 0 にする
fn note_from_position(crnt: u8, position: f32, slots: u16, ring: bool) -> Result<u8, u8> {
    if slots == 0 {
        return Err(TOUCH_POINT_ERROR);
    }
    let layout = Layout { pads: slots, ring };
    let position = layout.normalize(position);
    let round = |x: f32| {
        let r = (x + 0.5) as u16; // x は 0 以上
        (if ring { r % slots } else { r.min(slots - 1) }) as u8
    };
    if crnt == NEW_NOTE {
        Ok(round(position))
    } else if (crnt as u16) < slots {
        if layout.distance(position, crnt as f32) > HISTERESIS {
            Ok(round(position))
        } else {
            Ok(crnt) // No change in note
        }
    } else {
        // Invalid note number, return TOUCH_POINT_ERROR
        Err(TOUCH_POINT_ERROR)
    }
}

/// ノートの数（電極数 / ELECTRODES_PER_NOTE）
fn note_slots(layout: Layout) -> u16 {
    layout.pads / ELECTRODES_PER_NOTE
}

/// crnt_note : 0〜(ノートの数 − 1) の今のノート、NEW_NOTE は新規ノート。電極 ELECTRODES_PER_NOTE 個が 1 ノート
fn note_from_location(crnt_note: u8, location: f32, layout: Layout) -> Result<u8, u8> {
    let location = layout.normalize(location) / ELECTRODES_PER_NOTE as f32;
    note_from_position(crnt_note, location, note_slots(layout), layout.ring)
}

/// 指を検出してからの周期数から、Note On のベロシティを決める（§4.2）。
/// 0 で VELOCITY_FAST、NOTE_ON_TIMEOUT_CYCLES 以上で VELOCITY_SLOW、その間は線形（四捨五入）
fn onset_velocity(elapsed: u32) -> u8 {
    let t = elapsed.min(NOTE_ON_TIMEOUT_CYCLES);
    let span = (VELOCITY_FAST - VELOCITY_SLOW) as u32;
    let down = (span * t + NOTE_ON_TIMEOUT_CYCLES / 2) / NOTE_ON_TIMEOUT_CYCLES;
    VELOCITY_FAST - down as u8
}

// =========================================================
//      Voice: 指 1 本の演奏情報
// =========================================================
/// 指 1 本の演奏の状態（doc/touch_analysis.md §4.2）。タッチの層の `Track` を受けて進む
#[derive(Clone, Copy, Debug, PartialEq)]
enum VoiceState {
    Idle,     // 待機: 指が無い
    Onset, // 測定中: 指はあるが、まだ Note On を出していない（depth が NOTE_ON_DEPTH に届くのを待つ）
    Sounding, // 発音中: Note On を出した
}

/// 指 1 本の演奏情報（状態、鳴らしているノート、ビブラートを見るための位置の履歴）。添字は TouchPoint の id と同じ
#[derive(Clone, Copy, Debug)]
struct Voice {
    note: Option<u8>, // 鳴らしているノート（0〜ノートの数−1、NOTE_OFFSET を足す前）。None は鳴らしていない
    state: VoiceState,
    elapsed: u32, // 指を検出してからの周期数（測定中だけ数える。保持している周期も含む）
    max_depth: i16, // 測定中の、それまでの最大の depth
    prev_depth: i16, // 測定中の、前の周期の depth（下がったかを見る）
    velocity: u8, // Note On のベロシティ（つなぎ目を越えたときの Note On も同じ値を使う）
    history_idx: usize, // Store the last index for each touch point
    location_history: [f32; HISTORY_SIZE], // Store past locations for each possible note
}

impl Voice {
    const INITIAL: Self = Voice {
        note: None,
        state: VoiceState::Idle,
        elapsed: 0,
        max_depth: 0,
        prev_depth: 0,
        velocity: VELOCITY_FAST,
        history_idx: 0,
        location_history: [0f32; HISTORY_SIZE],
    };
    /// 新しいタッチで、履歴を今の位置で埋める。Voice は使い回すので、前のタッチの位置が残らないようにする
    /// （doc/touch_analysis.md §6.3 の 3）。履歴が平らなので、動くまではビブラートにならない
    fn reset_history(&mut self, location: f32) {
        self.location_history = [location; HISTORY_SIZE];
        self.history_idx = 0;
    }
    fn push_history(&mut self, location: f32) {
        self.location_history[self.history_idx] = location;
        self.history_idx = (self.history_idx + 1) % HISTORY_SIZE;
    }
    /// 新しい指: 測定中から始める
    fn start_onset(&mut self, location: f32, depth: i16) {
        self.reset_history(location);
        self.state = VoiceState::Onset;
        self.elapsed = 0;
        self.max_depth = depth;
        self.prev_depth = depth;
    }
    /// 測定中に、この周期に測った depth で Note On を出すか決める（doc/touch_analysis.md §4.2）。
    /// 出すならベロシティを返す
    /// - depth が NOTE_ON_DEPTH を超えた: かかった時間で決める（`onset_velocity`）
    /// - NOTE_ON_TIMEOUT_CYCLES が過ぎた（軽く触れたまま）: 同じく時間で決める（VELOCITY_SLOW になる）
    /// - 前の周期より下がり、それまでの最大が FLICK_DEPTH を超えている（はじいた）: VELOCITY_FAST
    fn onset(&mut self, depth: i16) -> Option<u8> {
        let fell = depth < self.prev_depth;
        self.prev_depth = depth;
        self.max_depth = self.max_depth.max(depth);
        if depth > NOTE_ON_DEPTH || self.elapsed >= NOTE_ON_TIMEOUT_CYCLES {
            Some(onset_velocity(self.elapsed))
        } else if fell && self.max_depth > FLICK_DEPTH {
            Some(VELOCITY_FAST)
        } else {
            None
        }
    }
    /// 測定中に指を見失った周期（depth は分からないが、区間が消えたので下がったとみなす）。
    /// はじいた（それまでの最大が FLICK_DEPTH を超えている）なら VELOCITY_FAST を返す
    fn onset_lost(&mut self) -> Option<u8> {
        (self.max_depth > FLICK_DEPTH).then_some(VELOCITY_FAST)
    }
    fn detect_few_hz_round_trip_hz(&self) -> Option<f32> {
        // 新規起動直後の0埋めや静止状態を除外
        let mut min_v = f32::INFINITY;
        let mut max_v = f32::NEG_INFINITY;
        let mut sum = 0.0;
        // location_history全体を走査して、最小値・最大値・平均値を計算する
        for i in 0..HISTORY_SIZE {
            let idx = (self.history_idx + i) % HISTORY_SIZE;
            let v = self.location_history[idx];
            min_v = min_v.min(v);
            max_v = max_v.max(v);
            sum += v;
        }

        let peak_to_peak = max_v - min_v;
        if !peak_to_peak.is_finite() || peak_to_peak < OSC_MIN_PEAK_TO_PEAK {
            return None;
        }

        let mean = sum / HISTORY_SIZE as f32;
        let deadband = OSC_DEADBAND.max(peak_to_peak * 0.08);
        let mut state: i8 = 0; // -1: below mean, +1: above mean
        let mut crossings: u16 = 0;

        for i in 0..HISTORY_SIZE {
            let idx = (self.history_idx + i) % HISTORY_SIZE;
            let centered = self.location_history[idx] - mean;
            let next_state = if centered > deadband {
                1
            } else if centered < -deadband {
                -1
            } else {
                state
            };

            // state が0から非0に、あるいは非0から反転する crossing を数える
            if state != 0 && next_state != state {
                crossings = crossings.saturating_add(1);
            }
            state = next_state;
        }

        if crossings < 2 {
            return None;
        }

        let duration_sec = (HISTORY_SIZE as f32 - 1.0) * TOUCH_SAMPLE_PERIOD_SEC;
        if duration_sec <= 0.0 {
            return None;
        }
        let freq_hz = crossings as f32 / (2.0 * duration_sec);
        if (FEW_HZ_MIN..=FEW_HZ_MAX).contains(&freq_hz) {
            Some(freq_hz)
        } else {
            None
        }
    }
}

// =========================================================
//      PlayInfo: 演奏情報を作る
// =========================================================
/// F は演奏情報のコールバック（status, note, velocity, location）
#[derive(Clone, Copy, Debug)]
pub struct PlayInfo<F>
where
    F: Fn(u8, u8, u8, f32),
{
    midi_callback: F,                  // MIDI callback function
    voices: [Voice; MAX_TOUCH_POINTS], // 指毎の演奏情報
    vibrato: u8,                       // ビブラートの強さ (0-127)
    vibrato_enabled: bool, // ビブラートを検出して CC を出すか。2026-11-13 から既定では止めている（コードは残す）
}

impl<F> PlayInfo<F>
where
    F: Fn(u8, u8, u8, f32),
{
    pub fn new(cb: F) -> Self {
        PlayInfo {
            midi_callback: cb,
            voices: [Voice::INITIAL; MAX_TOUCH_POINTS],
            vibrato: 0,
            vibrato_enabled: false,
        }
    }
    /// ビブラートの強さ（0-127）。最後に検出した値で、触れていないときも残る
    pub fn vibrato(&self) -> u8 {
        self.vibrato
    }
    /// ビブラートの検出（CC1・CC19）を動かすか。既定は止めている
    pub fn set_vibrato_enabled(&mut self, enabled: bool) {
        self.vibrato_enabled = enabled;
    }

    /// 1 周期分。`TouchTracker::update` の後に呼ぶ。指毎の `Track` で状態を進めて Note On/Off を出し、
    /// 続けてビブラートを更新する
    pub fn update<P>(&mut self, touch: &TouchTracker<P>)
    where
        P: AsRef<[u16]> + AsMut<[u16]>,
    {
        let layout = touch.layout();
        if note_slots(layout) == 0 {
            return;
        }
        self.perform_notes(touch, layout);
        if self.vibrato_enabled {
            self.update_vibrato(touch, layout);
        }
    }

    /// 追跡の結果（Track）で、指毎の状態（VoiceState）を進め、Note On/Off を出す（doc/touch_analysis.md §4.2）。
    /// 始まり・動きを先に、離れたのを後に処理する
    ///
    /// | 状態 | Track | すること |
    /// |---|---|---|
    /// | 待機 | Started | 測定中へ。続けて、測定中の Followed と同じ判断をする |
    /// | 測定中 | Followed | `Voice::onset` で Note On を出すか決める。出したら発音中へ |
    /// | 測定中 | None（保持） | 指を見失った: 下がったとみなし、はじいたなら Note On |
    /// | 測定中 | Released | 何も出さずに待機へ（かすっただけ） |
    /// | 発音中 | Followed | ノートが変わったら Move（つなぎ目では cross_seam） |
    /// | 発音中 | Released | Note Off → 待機 |
    fn perform_notes<P>(&mut self, touch: &TouchTracker<P>, layout: Layout)
    where
        P: AsRef<[u16]> + AsMut<[u16]>,
    {
        let tps = touch.touch_points();
        for (i, tp) in tps.iter().enumerate() {
            let location = tp.get_location();
            let voice = &mut self.voices[i];
            let velocity = match (tp.track(), voice.state) {
                (Track::Started, _) => {
                    voice.start_onset(location, tp.get_intensity());
                    voice.onset(tp.get_intensity())
                }
                (Track::Followed, VoiceState::Onset) => {
                    voice.push_history(location);
                    voice.elapsed = voice.elapsed.saturating_add(1);
                    voice.onset(tp.get_intensity())
                }
                (Track::Followed, VoiceState::Sounding) => {
                    voice.push_history(location);
                    self.note_follow(i, location, layout);
                    None
                }
                (Track::None, VoiceState::Onset) if tp.is_touched() => {
                    voice.elapsed = voice.elapsed.saturating_add(1);
                    voice.onset_lost()
                }
                _ => None,
            };
            if let Some(velocity) = velocity {
                self.note_start(i, location, velocity, layout);
            }
        }
        for (i, tp) in tps.iter().enumerate() {
            if tp.track() != Track::Released {
                continue;
            }
            if self.voices[i].state == VoiceState::Sounding {
                self.note_release(i, tp.last_location());
            }
            self.voices[i].state = VoiceState::Idle;
        }
    }
    /// ノートを決めて Note On を出し、発音中にする
    fn note_start(&mut self, i: usize, location: f32, velocity: u8, layout: Layout) {
        let note = note_from_location(NEW_NOTE, location, layout).ok();
        let voice = &mut self.voices[i];
        voice.note = note;
        voice.velocity = velocity;
        voice.state = VoiceState::Sounding;
        if let Some(note) = note {
            self.send_note_on(i, note.saturating_add(NOTE_OFFSET), velocity, location);
        }
    }
    /// 同じ指が続いている: ノートが変わったら、Move かつなぎ目を越えたかで分ける
    fn note_follow(&mut self, i: usize, location: f32, layout: Layout) {
        let Some(crnt) = self.voices[i].note else {
            return;
        };
        let Ok(updated) = note_from_location(crnt, location, layout) else {
            return;
        };
        if updated == crnt {
            return;
        }
        let from = crnt.saturating_add(NOTE_OFFSET);
        let to = updated.saturating_add(NOTE_OFFSET);
        // つなぎ目をまたぐ: ノートの番号が半周より大きく変わった（47 → 0、0 → 47 など）
        let crossed = layout.ring
            && (updated as i32 - crnt as i32).unsigned_abs() > note_slots(layout) as u32 / 2;
        if crossed {
            self.cross_seam(i, from, to, location);
        } else {
            self.move_note(i, from, to, location);
        }
        self.voices[i].note = Some(updated);
    }
    /// Move: 次のノートの Note On（ベロシティは VELOCITY_MOVE で固定）を出して、前のノートの Note Off（MOVED）を出す
    fn move_note(&self, i: usize, from: u8, to: u8, location: f32) {
        self.send_note_on(i, to, VELOCITY_MOVE, location);
        (self.midi_callback)(STATUS_NOTE_MOVED | i as u8, from, 0x40, location);
    }
    /// つなぎ目を越えた（doc/touch_analysis.md §4.1）: Move はせず、前のノートの Note Off を出してから、
    /// 越えた先のノートの Note On を、その指の Note On のベロシティで出す。
    /// Move に処理を足すことがあるので分けておく
    fn cross_seam(&self, i: usize, from: u8, to: u8, location: f32) {
        (self.midi_callback)(STATUS_NOTE_OFF | i as u8, from, 0x40, location);
        self.send_note_on(i, to, self.voices[i].velocity, location);
    }
    /// 離れた: Note Off
    fn note_release(&mut self, i: usize, location: f32) {
        if let Some(note) = self.voices[i].note {
            let midi_note = note.saturating_add(NOTE_OFFSET);
            (self.midi_callback)(STATUS_NOTE_OFF | i as u8, midi_note, 0x40, location);
        }
        self.voices[i].note = None;
    }
    fn send_note_on(&self, i: usize, midi_note: u8, velocity: u8, location: f32) {
        (self.midi_callback)(STATUS_NOTE_ON | i as u8, midi_note, velocity, location);
    }

    /// ビブラート。リングのつなぎ目の上（position が電極数 − 1 以上。96 電極では 95 以上）に指がある間は、
    /// 検出した値を出さない（doc/touch_analysis.md §4.1）
    fn update_vibrato<P>(&mut self, touch: &TouchTracker<P>, layout: Layout)
    where
        P: AsRef<[u16]> + AsMut<[u16]>,
    {
        let seam = (layout.pads - 1) as f32;
        let mut touched = false;
        let mut on_seam = false;
        let mut few_hz_detected: Option<f32> = None;

        for (tp, voice) in touch.touch_points().iter().zip(self.voices.iter()) {
            if tp.is_touched() {
                touched = true;
                if layout.ring && tp.get_location() >= seam {
                    on_seam = true;
                }
                if few_hz_detected.is_none() {
                    few_hz_detected = voice.detect_few_hz_round_trip_hz();
                }
            }
        }

        if touched {
            let vib = few_hz_detected.map_or(0, |hz| (hz * 10.0) as u32).min(127) as u8;
            if !on_seam && vib != self.vibrato {
                let dpt = (vib * 2).min(48);
                (self.midi_callback)(
                    STATUS_CC, 1,   // CC number for vibrato
                    dpt, // CC value for pitch bend (centered at 64)
                    0.0, // location is not relevant for vibrato command
                );
                (self.midi_callback)(
                    STATUS_CC, 19, // CC number for vibrato
                    vib, 0.0, // location is not relevant for vibrato command
                );
                self.vibrato = vib;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use std::vec::Vec;

    type Log = RefCell<Vec<(u8, u8, u8)>>; // (status, note, velocity)

    /// 1 周期分の output を置いて、depth・position の層 → 演奏情報の層の順に解析する
    fn step<F, P>(tracker: &mut TouchTracker<P>, play: &mut PlayInfo<F>, values: &[(usize, u16)])
    where
        F: Fn(u8, u8, u8, f32),
        P: AsRef<[u16]> + AsMut<[u16]>,
    {
        for k in 0..tracker.layout().pads as usize {
            tracker.set_value(k, 0);
        }
        for &(k, v) in values {
            tracker.set_value(k, v);
        }
        tracker.update();
        play.update(tracker);
    }

    fn logger(log: &Log) -> impl Fn(u8, u8, u8, f32) + '_ {
        |status, note, velocity, _location| log.borrow_mut().push((status, note, velocity))
    }

    /// 電極 c を中心にした指（depth 100。NOTE_ON_DEPTH を超える）
    fn finger(c: usize, pads: usize) -> [(usize, u16); 3] {
        [((c + pads - 1) % pads, 20), (c, 60), ((c + 1) % pads, 20)]
    }

    /// 速く押す: depth が初めから NOTE_ON_DEPTH を超えていれば、検出した周期に VELOCITY_FAST で Note On。
    /// 電極 4 は 4 / 2 = ノート 2。離すと Note Off が 1 回だけ出る
    #[test]
    fn fast_press_note_on_off() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        step(&mut tracker, &mut play, &finger(4, 12));
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_FAST)]
        );
        for _ in 0..10 {
            step(&mut tracker, &mut play, &[]);
        }
        assert_eq!(
            log.borrow().as_slice(),
            &[
                (STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_FAST),
                (STATUS_NOTE_OFF, 2 + NOTE_OFFSET, 0x40)
            ]
        );
    }

    /// ゆっくり押す: NOTE_ON_DEPTH を超えた周期に、かかった時間のベロシティで Note On
    #[test]
    fn slow_press_velocity_by_time() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for t in 0..15u16 {
            step(&mut tracker, &mut play, &[(4, 20 + t * 3)]); // 20 → 62
        }
        assert!(log.borrow().is_empty());
        step(&mut tracker, &mut play, &[(4, 80)]); // 経過 15 周期
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, onset_velocity(15))]
        );
        assert_eq!(onset_velocity(15), 65);
    }

    /// 軽く触れたまま: NOTE_ON_DEPTH に届かず下がりもしなければ、NOTE_ON_TIMEOUT_CYCLES で VELOCITY_SLOW
    #[test]
    fn light_touch_times_out_slow() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for _ in 0..NOTE_ON_TIMEOUT_CYCLES {
            step(&mut tracker, &mut play, &[(4, 30)]);
        }
        assert!(log.borrow().is_empty());
        step(&mut tracker, &mut play, &[(4, 30)]);
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_SLOW)]
        );
    }

    /// はじく: NOTE_ON_DEPTH に届かずに下がっても、最大が FLICK_DEPTH を超えていれば、
    /// 下がった周期に VELOCITY_FAST で Note On
    #[test]
    fn flick_sounds_fast() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for v in [20, 40, 60] {
            step(&mut tracker, &mut play, &[(4, v)]);
        }
        assert!(log.borrow().is_empty());
        step(&mut tracker, &mut play, &[(4, 40)]);
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_FAST)]
        );
        // 区間が消えたら、そのまま見失った周期にも Note On を出す（下がったとみなす）
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for v in [20, 60] {
            step(&mut tracker, &mut play, &[(4, v)]);
        }
        step(&mut tracker, &mut play, &[]);
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_FAST)]
        );
        for _ in 0..10 {
            step(&mut tracker, &mut play, &[]);
        }
        assert_eq!(log.borrow()[1], (STATUS_NOTE_OFF, 2 + NOTE_OFFSET, 0x40));
    }

    /// かすっただけ: FLICK_DEPTH を超えずに消えたら、Note On も Note Off も出さない
    #[test]
    fn graze_is_silent() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for v in [20, 40, 30] {
            step(&mut tracker, &mut play, &[(4, v)]);
        }
        for _ in 0..10 {
            step(&mut tracker, &mut play, &[]);
        }
        assert!(log.borrow().is_empty());
        // 次のタッチは、また初めから測る
        step(&mut tracker, &mut play, &finger(4, 12));
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_FAST)]
        );
    }

    /// 指を動かしてノートが変わると、新しいノートの Note On（VELOCITY_MOVE）と古いノートの MOVED が出る
    #[test]
    fn moving_finger_changes_note() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        step(&mut tracker, &mut play, &finger(4, 12));
        step(&mut tracker, &mut play, &finger(6, 12));
        assert_eq!(
            log.borrow().as_slice(),
            &[
                (STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_FAST),
                (STATUS_NOTE_ON, 3 + NOTE_OFFSET, VELOCITY_MOVE),
                (STATUS_NOTE_MOVED, 2 + NOTE_OFFSET, 0x40)
            ]
        );
    }

    /// ベロシティは、経過 0 で VELOCITY_FAST、NOTE_ON_TIMEOUT_CYCLES 以上で VELOCITY_SLOW、その間は線形
    #[test]
    fn onset_velocity_is_linear() {
        assert_eq!(onset_velocity(0), VELOCITY_FAST);
        assert_eq!(onset_velocity(15), 65);
        assert_eq!(onset_velocity(NOTE_ON_TIMEOUT_CYCLES), VELOCITY_SLOW);
        assert_eq!(onset_velocity(1000), VELOCITY_SLOW);
    }

    /// リングでつなぎ目を越えたら、Move せず、Note Off → Note On でノートが変わる（§4.1）
    #[test]
    fn ring_seam_crossing_is_note_off_then_on() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], true);
        let mut play = PlayInfo::new(logger(&log));
        // 電極 10 = ノート 5（ノートの数は 6）
        step(&mut tracker, &mut play, &finger(10, 12));
        // 電極 0 = ノート 0: つなぎ目を越えた
        step(&mut tracker, &mut play, &finger(0, 12));
        assert_eq!(
            log.borrow().as_slice(),
            &[
                (STATUS_NOTE_ON, 5 + NOTE_OFFSET, VELOCITY_FAST),
                (STATUS_NOTE_OFF, 5 + NOTE_OFFSET, 0x40),
                (STATUS_NOTE_ON, NOTE_OFFSET, VELOCITY_FAST),
            ]
        );
        // つなぎ目を越えないノートの変化は Move のまま
        step(&mut tracker, &mut play, &finger(2, 12));
        assert_eq!(
            log.borrow()[3..],
            [
                (STATUS_NOTE_ON, 1 + NOTE_OFFSET, VELOCITY_MOVE),
                (STATUS_NOTE_MOVED, NOTE_OFFSET, 0x40)
            ]
        );
    }

    /// リング（電極 2 つで 1 ノート）でも、ノートはつなぎ目で巡回する
    #[test]
    fn ring_note_wraps() {
        let layout = Layout {
            pads: 96,
            ring: true,
        };
        assert_eq!(note_from_location(NEW_NOTE, 95.2, layout), Ok(0));
        assert_eq!(note_from_location(NEW_NOTE, 94.6, layout), Ok(47));
        // 94.4 / 2 = 47.2 は、今のノート 47 からヒステリシスの中
        assert_eq!(note_from_location(47, 94.4, layout), Ok(47));
        // 今のノート 0 から、つなぎ目の向こうに 0.4 ノート（95.2 / 2 = 47.6）はヒステリシスの中
        assert_eq!(note_from_location(0, 95.2, layout), Ok(0));
    }

    /// ビブラート: 既定では止めている。動かすと、つなぎ目の上（position が電極数 − 1 以上）では
    /// 検出した値を出さない（§4.1）
    #[test]
    fn vibrato_is_held_on_seam() {
        // 3Hz・振幅 0.5 の揺れを位置の履歴に入れてから、1 周期解析する
        fn run(center: usize, enabled: bool) -> (u8, usize) {
            let log: Log = RefCell::new(Vec::new());
            let mut tracker = TouchTracker::new([0u16; 12], true);
            let mut play = PlayInfo::new(logger(&log));
            play.set_vibrato_enabled(enabled);
            step(&mut tracker, &mut play, &finger(center, 12));
            let base = tracker.touch_point(0).unwrap().get_location();
            for (k, h) in play.voices[0].location_history.iter_mut().enumerate() {
                // no_std の f32 に sin が無いので、三角波で代わりにする
                let tri = (k as f32 * TOUCH_SAMPLE_PERIOD_SEC * 3.0 * 2.0) % 2.0;
                *h = base
                    + 0.5
                        * (if tri < 1.0 {
                            tri * 2.0 - 1.0
                        } else {
                            3.0 - tri * 2.0
                        });
            }
            step(&mut tracker, &mut play, &finger(center, 12));
            let cc = log.borrow().iter().filter(|e| e.0 == STATUS_CC).count();
            (play.vibrato(), cc)
        }
        // 止めている間は出さない
        assert_eq!(run(5, false), (0, 0));
        // つなぎ目から離れた所では検出して CC を出す
        let (vib, cc) = run(5, true);
        assert!(vib > 0);
        assert_eq!(cc, 2);
        // 電極 11 の上（position 11.0 = 電極数 − 1）では出さない
        assert_eq!(run(11, true), (0, 0));
    }

    /// 新しいタッチでは、前のタッチの位置の履歴が残らない（§6.3 の 3）
    #[test]
    fn new_touch_clears_location_history() {
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(|_, _, _, _| {});
        step(&mut tracker, &mut play, &finger(2, 12));
        for _ in 0..10 {
            step(&mut tracker, &mut play, &[]);
        }
        step(&mut tracker, &mut play, &finger(8, 12));
        assert!(play.voices[0].location_history.iter().all(|&l| l == 8.0));
    }
}
