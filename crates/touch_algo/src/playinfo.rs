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

use crate::touch::{Layout, MAX_TOUCH_POINTS, TouchTracker, Track};

// =========================================================
//      Constants
// =========================================================
pub const HISTERESIS: f32 = 0.7; // Hysteresis value for touch point detection

// MIDI
pub const KEYBD_LO: u8 = 21; // A0
pub const PIANO_OFFSET: u8 = KEYBD_LO - 4;
pub const VIOLIN_OFFSET: u8 = 55; // バイオリンモードのMIDIノートオフセット

// コールバックの status（上位 4 ビット。下位 4 ビットは TouchPoint の id、CC では 0）
pub const STATUS_NOTE_ON: u8 = 0x90; // Note On
pub const STATUS_NOTE_OFF: u8 = 0x80; // Note Off
pub const STATUS_NOTE_MOVED: u8 = 0xa0; // ノートが変わったときの古いノートの Note Off
pub const STATUS_CC: u8 = 0xb0; // Control Change

const HISTORY_SIZE: usize = 128; // Number of past locations
const TOUCH_SAMPLE_PERIOD_SEC: f32 = 0.01; // 10ms tick
const FEW_HZ_MIN: f32 = 1.5;
const FEW_HZ_MAX: f32 = 8.0;
const OSC_MIN_PEAK_TO_PEAK: f32 = 0.3;
const OSC_DEADBAND: f32 = 0.1;

const NEW_NOTE: u8 = 0xff;
const TOUCH_POINT_ERROR: u8 = 0xfe;

/// 動作モード
#[repr(u8)]
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum WorkMode {
    Piano = 0,
    Violin = 1,
}
// u8 から WorkMode への変換を定義
impl TryFrom<u8> for WorkMode {
    type Error = ();

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(WorkMode::Piano),
            1 => Ok(WorkMode::Violin),
            _ => Err(()), // 定義外の数値の場合はエラー
        }
    }
}

// =========================================================
//      position → ノート
// =========================================================
/// 現在のノート・位置・電極の並びから、新しいノートを返す
type NewLocationFn = fn(u8, f32, Layout) -> Result<u8, u8>;

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

/// crnt_note : 0-(max_pads-1) 現在の位置、NEW_NOTE は新規ノート。電極 1 つが 1 ノート
fn note_piano(crnt_note: u8, location: f32, layout: Layout) -> Result<u8, u8> {
    note_from_position(crnt_note, location, layout.pads, layout.ring)
}
/// crnt_note : 0-(max_pads/2-1) 現在の位置、NEW_NOTE は新規ノート。電極 2 つが 1 ノート
fn note_violin(crnt_note: u8, location: f32, layout: Layout) -> Result<u8, u8> {
    let location = layout.normalize(location) / 2.0;
    note_from_position(crnt_note, location, layout.pads / 2, layout.ring)
}

// =========================================================
//      Voice: 指 1 本の演奏情報
// =========================================================
/// 指 1 本の演奏情報（鳴らしているノートと、ビブラートを見るための位置の履歴）。添字は TouchPoint の id と同じ
#[derive(Clone, Copy, Debug)]
struct Voice {
    note: Option<u8>, // 鳴らしているノート（0〜slots−1、offset_note を足す前）。None は鳴らしていない
    offset_note: u8,  // MIDI Note offset
    slots: u16,       // ノートの数（Piano は電極数、Violin は電極数 / 2）
    new_location: NewLocationFn,
    history_idx: usize, // Store the last index for each touch point
    location_history: [f32; HISTORY_SIZE], // Store past locations for each possible note
}

impl Voice {
    const INITIAL: Self = Voice {
        note: None,
        offset_note: PIANO_OFFSET,
        slots: 0,
        new_location: note_piano,
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
    /// location_history(128サンプル)から往復(半周期ごとの符号反転)を検出し、数Hzなら周波数を返す
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
    last_note: u8,                     // 最後に送信したMIDIノート番号
    on_time: u32,                      // 最後のタッチが開始してから離されるまでの時間
    // タッチ中の TouchPoint は、最後の TouchPoint の touching_time
    off_time: u32, // 最後のタッチが離されてからの時間
    vibrato: u8,   // ビブラートの強さ (0-127)
}

impl<F> PlayInfo<F>
where
    F: Fn(u8, u8, u8, f32),
{
    pub fn new(cb: F) -> Self {
        PlayInfo {
            midi_callback: cb,
            voices: [Voice::INITIAL; MAX_TOUCH_POINTS],
            last_note: 0,
            on_time: 0,
            off_time: 0,
            vibrato: 0,
        }
    }
    /// ビブラートの強さ（0-127）。最後に検出した値で、触れていないときも残る
    pub fn vibrato(&self) -> u8 {
        self.vibrato
    }
    /// Violinモード向けのMIDI NoteOn velocity算出
    ///
    /// 算出に使用する値:
    /// - last_note: 直前に送信したノート
    /// - on_time:   最後のタッチ継続時間(10ms単位)
    /// - off_time:  最後に離してからの経過時間(10ms単位)
    pub fn calc_violin_note_on_velocity(&self, note: u8) -> u8 {
        Self::calc_violin_note_on_velocity_from(self.last_note, self.on_time, self.off_time, note)
    }

    /// 1 周期分。`TouchTracker::update` の後に呼ぶ。指毎の `Track` から Note On/Off を出し、
    /// 続けて継続時間とビブラートを更新する
    pub fn update<P>(&mut self, touch: &TouchTracker<P>, work_mode: WorkMode)
    where
        P: AsRef<[u16]> + AsMut<[u16]>,
    {
        let layout = touch.layout();
        if layout.pads == 0 {
            return;
        }
        self.perform_notes(touch, layout, work_mode);
        self.update_durations_and_vibrato(touch, layout, work_mode);
    }

    /// 追跡の結果（Track）から Note On/Off を出す。始まり・動きを先に、離れたのを後に処理する
    fn perform_notes<P>(&mut self, touch: &TouchTracker<P>, layout: Layout, work_mode: WorkMode)
    where
        P: AsRef<[u16]> + AsMut<[u16]>,
    {
        let tps = touch.touch_points();
        let mut latest_note: Option<u8> = None;
        for (i, tp) in tps.iter().enumerate() {
            match tp.track() {
                Track::Started => {
                    self.voices[i].reset_history(tp.get_location());
                    self.note_start(i, tp.get_location(), tp.get_intensity(), layout, work_mode);
                }
                Track::Followed => {
                    self.voices[i].push_history(tp.get_location());
                    if let Some(note) = self.note_follow(
                        i,
                        tp.get_location(),
                        tp.get_intensity(),
                        layout,
                        work_mode,
                    ) {
                        latest_note = Some(note);
                    }
                }
                Track::None | Track::Released => {}
            }
        }
        if let Some(note) = latest_note {
            self.last_note = note;
        }
        for (i, tp) in tps.iter().enumerate() {
            if tp.track() == Track::Released {
                self.note_release(i, tp.last_location(), tp.touching_time());
            }
        }
    }
    /// 新しいタッチ: ノートを決めて Note On
    fn note_start(
        &mut self,
        i: usize,
        location: f32,
        intensity: i16,
        layout: Layout,
        work_mode: WorkMode,
    ) {
        let (offset_note, slots, new_location): (u8, u16, NewLocationFn) =
            if work_mode == WorkMode::Violin {
                (VIOLIN_OFFSET, layout.pads / 2, note_violin)
            } else {
                (PIANO_OFFSET, layout.pads, note_piano)
            };
        let note = new_location(NEW_NOTE, location, layout).ok();
        let voice = &mut self.voices[i];
        voice.note = note;
        voice.offset_note = offset_note;
        voice.slots = slots;
        voice.new_location = new_location;
        if let Some(note) = note {
            let midi_note = note.saturating_add(offset_note);
            self.send_note_on(i, midi_note, intensity, location, work_mode);
            self.last_note = midi_note;
        }
    }
    /// 同じ指が続いている: ノートが変わったら、Move かつなぎ目を越えたかで分ける。
    /// ノートが変わったときは、新しいノート（offset を足したもの）を返す
    fn note_follow(
        &mut self,
        i: usize,
        location: f32,
        intensity: i16,
        layout: Layout,
        work_mode: WorkMode,
    ) -> Option<u8> {
        let voice = self.voices[i];
        let crnt = voice.note?;
        let updated = (voice.new_location)(crnt, location, layout).ok()?;
        if updated == crnt {
            return None;
        }
        let from = crnt.saturating_add(voice.offset_note);
        let to = updated.saturating_add(voice.offset_note);
        // つなぎ目をまたぐ: ノートの番号が半周より大きく変わった（95 → 0、0 → 95 など）
        let crossed =
            layout.ring && (updated as i32 - crnt as i32).unsigned_abs() > voice.slots as u32 / 2;
        if crossed {
            self.cross_seam(i, from, to, intensity, location, work_mode);
        } else {
            self.move_note(i, from, to, intensity, location, work_mode);
        }
        self.voices[i].note = Some(updated);
        Some(to)
    }
    /// Move: 次のノートの Note On を出して、前のノートの Note Off（MOVED）を出す
    fn move_note(
        &self,
        i: usize,
        from: u8,
        to: u8,
        intensity: i16,
        location: f32,
        work_mode: WorkMode,
    ) {
        self.send_note_on(i, to, intensity, location, work_mode);
        (self.midi_callback)(STATUS_NOTE_MOVED | i as u8, from, 0x40, location);
    }
    /// つなぎ目を越えた（doc/touch_analysis.md §4.1）: Move はせず、前のノートの Note Off を出してから、
    /// 越えた先のノートの Note On を出す。今は出るイベントが Move とほぼ同じだが、
    /// Move に処理を足すことがあるので分けておく
    fn cross_seam(
        &self,
        i: usize,
        from: u8,
        to: u8,
        intensity: i16,
        location: f32,
        work_mode: WorkMode,
    ) {
        (self.midi_callback)(STATUS_NOTE_OFF | i as u8, from, 0x40, location);
        self.send_note_on(i, to, intensity, location, work_mode);
    }
    /// 離れた: Note Off
    fn note_release(&mut self, i: usize, location: f32, touching_time: u32) {
        let voice = self.voices[i];
        if let Some(note) = voice.note {
            let midi_note = note.saturating_add(voice.offset_note);
            (self.midi_callback)(STATUS_NOTE_OFF | i as u8, midi_note, 0x40, location);
            self.last_note = midi_note;
        }
        self.on_time = touching_time;
        self.voices[i].note = None;
    }
    /// Note On を出す。ベロシティは動作モードと今までの演奏から決める
    fn send_note_on(
        &self,
        i: usize,
        midi_note: u8,
        intensity: i16,
        location: f32,
        work_mode: WorkMode,
    ) {
        let velocity = Self::note_on_velocity_from_context(
            work_mode,
            self.last_note,
            self.on_time,
            self.off_time,
            midi_note,
            intensity,
        );
        (self.midi_callback)(STATUS_NOTE_ON | i as u8, midi_note, velocity, location);
    }

    /// 時間計測とビブラート。ビブラートは、リングのつなぎ目の上（position が電極数 − 1 以上。
    /// 96 電極では 95 以上）に指がある間は、検出した値を出さない（doc/touch_analysis.md §4.1）
    fn update_durations_and_vibrato<P>(
        &mut self,
        touch: &TouchTracker<P>,
        layout: Layout,
        work_mode: WorkMode,
    ) where
        P: AsRef<[u16]> + AsMut<[u16]>,
    {
        let seam = (layout.pads - 1) as f32;
        let mut touched = false;
        let mut on_seam = false;
        let mut latest_on_time = self.on_time;
        let mut few_hz_detected: Option<f32> = None;

        for (tp, voice) in touch.touch_points().iter().zip(self.voices.iter()) {
            if tp.is_touched() {
                touched = true;
                latest_on_time = tp.touching_time();
                if layout.ring && tp.get_location() >= seam {
                    on_seam = true;
                }
                if work_mode == WorkMode::Violin && few_hz_detected.is_none() {
                    few_hz_detected = voice.detect_few_hz_round_trip_hz();
                }
            }
        }

        if touched {
            self.on_time = latest_on_time;
            self.off_time = 0;
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
        } else {
            self.off_time = self.off_time.wrapping_add(1);
        }
    }

    //　タッチポイントの状況から、Note On のベロシティを決定する
    fn note_on_velocity_from_context(
        work_mode: WorkMode,
        last_note: u8,
        on_time: u32,
        off_time: u32,
        note: u8,
        intensity: i16,
    ) -> u8 {
        if work_mode == WorkMode::Violin {
            Self::calc_violin_note_on_velocity_from(last_note, on_time, off_time, note)
        } else {
            Self::default_note_on_velocity(intensity)
        }
    }
    // Violin モードの Note On ベロシティ計算:
    fn calc_violin_note_on_velocity_from(
        last_note: u8,
        on_time: u32,
        off_time: u32,
        note: u8,
    ) -> u8 {
        // 10ms tick前提
        const BASE_VELOCITY: i16 = 64;
        const SMALLER_SEC: u32 = 50; // 500ms
        const MAX_SEC: u32 = 300; // 3秒

        // on_time補正:
        // - SMALLER_SEC未満: 0..+56 を線形加算（速いほど大きい）
        // - SMALLER_SEC超〜MAX_SEC: 0..-32 を線形減算（遅いほど小さい）
        let on_adjust = if on_time < SMALLER_SEC {
            ((SMALLER_SEC - on_time) * 56 / SMALLER_SEC) as i16
        } else {
            let over = (on_time - SMALLER_SEC).min(MAX_SEC - SMALLER_SEC);
            -((over * 32 / (MAX_SEC - SMALLER_SEC)) as i16)
        };

        // off_time(staccato)補正:
        // - 0: 補正なし
        // - 0超〜SMALLER_SEC未満: +24..0 を線形加算
        // - SMALLER_SEC以上: 補正なし
        let off_adjust = if off_time == 0 {
            0
        } else if off_time < SMALLER_SEC {
            (((SMALLER_SEC - off_time) as i16) * 24 / (SMALLER_SEC as i16 - 1)).max(0)
        } else {
            0
        };

        // 音程距離補正:
        // - distance <= 3: 最大-12 (distance=3で0、distance=0で-12)
        // - distance > 3 : distance=12まで最大+12 を線形加算
        let note_distance = note.abs_diff(last_note) as i16;
        let distance_adjust = if note_distance <= 3 {
            -((3 - note_distance) * 12 / 3)
        } else {
            let d = (note_distance - 3).min(9);
            d * 12 / 9
        };

        let mut velocity = BASE_VELOCITY + on_adjust + off_adjust + distance_adjust;
        velocity = velocity.clamp(32, 112);
        velocity as u8
    }
    // デフォルトの Note On ベロシティ計算: 強度に基づいて 100..115 の範囲で線形に決める。
    // 強度が 63 を超えたときの値（Arduino 版では 255）は MIDI のデータバイト（0..127）を超えるので、
    // 127 で頭打ちにする（doc/touch_analysis.md §6.3）。
    // Pad の移動平均（4 サンプルの合計）を外して強度が 1/4 になったので、
    // 値が以前（> 255 で上限、>> 4）と同じになるよう、> 63 と >> 2 にしている
    fn default_note_on_velocity(intensity: i16) -> u8 {
        let velocity: u16 = if intensity < 0 {
            0
        } else if intensity > 63 {
            255
        } else {
            100 + (intensity as u16 >> 2)
        };
        velocity.min(127) as u8
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
    fn step<F, P>(
        tracker: &mut TouchTracker<P>,
        play: &mut PlayInfo<F>,
        values: &[(usize, u16)],
        mode: WorkMode,
    ) where
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
        play.update(tracker, mode);
    }

    fn logger(log: &Log) -> impl Fn(u8, u8, u8, f32) + '_ {
        |status, note, velocity, _location| log.borrow_mut().push((status, note, velocity))
    }

    /// 電極 4〜6 に 1 本の指を置くと、電極 5 の Note On が出て、離すと Note Off が出る
    #[test]
    fn one_finger_note_on_off() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        let finger = [(4, 10), (5, 30), (6, 10)];
        step(&mut tracker, &mut play, &finger, WorkMode::Piano);
        // ノート = 5 + PIANO_OFFSET、ベロシティ = 100 + depth(50) / 4
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 5 + PIANO_OFFSET, 112)]
        );

        // 離すと、RELEASE_WAITING_TIME の後に Note Off が 1 回だけ出る
        for _ in 0..10 {
            step(&mut tracker, &mut play, &[], WorkMode::Piano);
        }
        assert_eq!(
            log.borrow().as_slice(),
            &[
                (STATUS_NOTE_ON, 5 + PIANO_OFFSET, 112),
                (STATUS_NOTE_OFF, 5 + PIANO_OFFSET, 0x40)
            ]
        );
    }

    /// 指を動かして position がヒステリシスを越えると、新しいノートの Note On と古いノートの MOVED が出る
    #[test]
    fn moving_finger_changes_note() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        let at = |c: usize| [(c - 1, 10), (c, 30), (c + 1, 10)];
        step(&mut tracker, &mut play, &at(5), WorkMode::Piano);
        step(&mut tracker, &mut play, &at(6), WorkMode::Piano);
        assert_eq!(
            log.borrow().as_slice(),
            &[
                (STATUS_NOTE_ON, 5 + PIANO_OFFSET, 112),
                (STATUS_NOTE_ON, 6 + PIANO_OFFSET, 112),
                (STATUS_NOTE_MOVED, 5 + PIANO_OFFSET, 0x40)
            ]
        );
    }

    /// Piano のベロシティは 100..115、強度が 63 を超えたら 127（MIDI のデータバイトの範囲に収める）
    #[test]
    fn piano_velocity_is_within_midi_range() {
        type Play = PlayInfo<fn(u8, u8, u8, f32)>;
        assert_eq!(Play::default_note_on_velocity(0), 100);
        assert_eq!(Play::default_note_on_velocity(63), 115);
        assert_eq!(Play::default_note_on_velocity(64), 127);
        assert_eq!(Play::default_note_on_velocity(i16::MAX), 127);
        assert_eq!(Play::default_note_on_velocity(-1), 0);
    }

    /// リングでつなぎ目を越えたら、Move せず、Note Off → Note On でノートが変わる（§4.1）
    #[test]
    fn ring_seam_crossing_is_note_off_then_on() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], true);
        let mut play = PlayInfo::new(logger(&log));
        step(
            &mut tracker,
            &mut play,
            &[(10, 10), (11, 30), (0, 10)],
            WorkMode::Piano,
        );
        step(
            &mut tracker,
            &mut play,
            &[(11, 10), (0, 30), (1, 10)],
            WorkMode::Piano,
        );
        assert_eq!(
            log.borrow().as_slice(),
            &[
                (STATUS_NOTE_ON, 11 + PIANO_OFFSET, 112),
                (STATUS_NOTE_OFF, 11 + PIANO_OFFSET, 0x40),
                (STATUS_NOTE_ON, PIANO_OFFSET, 112),
            ]
        );
        // つなぎ目を越えないノートの変化は Move のまま
        step(
            &mut tracker,
            &mut play,
            &[(0, 10), (1, 30), (2, 10)],
            WorkMode::Piano,
        );
        assert_eq!(
            log.borrow()[3..],
            [
                (STATUS_NOTE_ON, 1 + PIANO_OFFSET, 112),
                (STATUS_NOTE_MOVED, PIANO_OFFSET, 0x40)
            ]
        );
    }

    /// リングの Violin（電極 2 つで 1 ノート）でも、ノートはつなぎ目で巡回する
    #[test]
    fn ring_violin_note_wraps() {
        let layout = Layout {
            pads: 96,
            ring: true,
        };
        assert_eq!(note_violin(NEW_NOTE, 95.2, layout), Ok(0));
        assert_eq!(note_violin(NEW_NOTE, 94.6, layout), Ok(47));
        // 94.4 / 2 = 47.2 は、今のノート 47 からヒステリシスの中
        assert_eq!(note_violin(47, 94.4, layout), Ok(47));
        // 今のノート 0 から、つなぎ目の向こうに 0.4 ノート（95.2 / 2 = 47.6）はヒステリシスの中
        assert_eq!(note_violin(0, 95.2, layout), Ok(0));
    }

    /// ビブラートは、つなぎ目の上（position が電極数 − 1 以上）では検出した値を出さない（§4.1）
    #[test]
    fn vibrato_is_held_on_seam() {
        // 3Hz・振幅 0.5 の揺れを位置の履歴に入れてから、1 周期解析する
        fn run(center: usize) -> (u8, usize) {
            let log: Log = RefCell::new(Vec::new());
            let mut tracker = TouchTracker::new([0u16; 12], true);
            let mut play = PlayInfo::new(logger(&log));
            let at = |c: usize| [((c + 11) % 12, 10), (c, 30), ((c + 1) % 12, 10)];
            step(&mut tracker, &mut play, &at(center), WorkMode::Violin);
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
            step(&mut tracker, &mut play, &at(center), WorkMode::Violin);
            let cc = log.borrow().iter().filter(|e| e.0 == STATUS_CC).count();
            (play.vibrato(), cc)
        }
        // つなぎ目から離れた所では検出して CC を出す
        let (vib, cc) = run(5);
        assert!(vib > 0);
        assert_eq!(cc, 2);
        // 電極 11 の上（position 11.0 = 電極数 − 1）では出さない
        assert_eq!(run(11), (0, 0));
    }

    /// 新しいタッチでは、前のタッチの位置の履歴が残らない（§6.3 の 3）
    #[test]
    fn new_touch_clears_location_history() {
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(|_, _, _, _| {});
        let at = |c: usize| [(c - 1, 10), (c, 30), (c + 1, 10)];
        step(&mut tracker, &mut play, &at(2), WorkMode::Violin);
        for _ in 0..10 {
            step(&mut tracker, &mut play, &[], WorkMode::Violin);
        }
        step(&mut tracker, &mut play, &at(8), WorkMode::Violin);
        assert!(play.voices[0].location_history.iter().all(|&l| l == 8.0));
    }
}
