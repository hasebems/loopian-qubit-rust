//  Created by Hasebe Masahiko on 2026/02/15.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
use crate::constants::*;
use crate::{ANY_TOUCH, DEBUG_VALUE, TOUCH0, TOUCH1, TOUCH2, TOUCH3};
use portable_atomic::Ordering;

// =========================================================
//      Touch Constants
// =========================================================
pub const MAX_PADS: u16 = TOTAL_QT_KEYS as u16; // MAX_SENS;
pub const TOUCH_THRESHOLD: u16 = 40; // Threshold for touch point detection
pub const CLOSE_RANGE: f32 = 3.0; // 同じタッチと見做される 10msec あたりの片側変化量
pub const FINGER_RANGE: usize = 3; // Maximum serial numbers of one touch point
pub const HISTERESIS: f32 = 0.7; // Hysteresis value for touch point detection

type NewLocationFn = fn(u8, f32) -> Result<u8, u8>;

const INIT_VAL: f32 = 100.0; // Invalid location initially
const RELEASE_WAITING_TIME: u32 = 5; // Number of cycles to wait before considering a touch point released
const CANDIDATE_MERGE_RANGE: f32 = 1.6; // Merge close candidates in the same cycle
const HISTORY_SIZE: usize = 128; // Number of past locations
const TOUCH_SAMPLE_PERIOD_SEC: f32 = 0.01; // 10ms tick
const FEW_HZ_MIN: f32 = 1.5;
const FEW_HZ_MAX: f32 = 8.0;
const OSC_MIN_PEAK_TO_PEAK: f32 = 0.3;
const OSC_DEADBAND: f32 = 0.1;
const SINGLE_PAD_SPIKE_RATIO_PERMILLE: i16 = 70; // 周辺の何%以上の強度があれば、単発パルスと見なすか
const SINGLE_PAD_SPIKE_NEIGHBOR_DIV: i16 = 4; // 周辺の最大強度が中心の何分の1以下なら、単発パルスと見なすか

const NEW_NOTE: u8 = 0xff;
const TOUCH_POINT_ERROR: u8 = 0xfe;

// =========================================================
//      Pad Class
// =========================================================
#[derive(Copy, Clone, Debug)]
pub struct Pad {
    mv_avg_value: u16, // 移動平均(MAX_MOVING_AVERAGEで割らない)
    past_value: [u16; Pad::MAX_MOVING_AVERAGE],
    past_index: usize,
}
impl Pad {
    const MAX_MOVING_AVERAGE: usize = 4; // Number of samples for moving average}

    fn new() -> Self {
        Pad {
            mv_avg_value: 0,
            past_value: [0; Pad::MAX_MOVING_AVERAGE],
            past_index: 0,
        }
    }
    /// 移動平均を更新するために、現在の値をセットする
    fn set_crnt(&mut self, value: u16) {
        let crnt_value = value;
        self.past_value[self.past_index] = crnt_value;
        self.past_index = (self.past_index + 1) % Self::MAX_MOVING_AVERAGE;
        self.mv_avg_value = self.past_value.iter().sum();
    }
    fn get_crnt(&self) -> u16 {
        self.mv_avg_value
    }
    fn diff_from_before(&mut self, value_before: u16) -> i16 {
        value_before as i16 - self.mv_avg_value as i16
    }
}

// =========================================================
//      TouchPoint Class
// =========================================================
// センサーの生値から、実際にどのあたりをタッチしているかを判断し、保持する
#[derive(Clone, Copy, Debug)]
pub struct TouchPoint<F>
where
    F: Fn(u8, u8, u8, f32) + Clone, // status, note, velocity, location
{
    id: usize,
    center_location: f32,
    intensity: i16,
    real_crnt_note: u8, // MIDI Note number
    is_updated: bool,
    is_touched: bool,
    touching_time: u32,
    no_update_time: u32,
    offset_note: u8, // MIDI Note offset
    new_location: NewLocationFn,
    midi_callback: Option<F>,              // MIDI callback function
    history_idx: usize,                    // Store the last index for each touch point
    location_history: [f32; HISTORY_SIZE], // Store past locations for each possible note
}
impl<F> TouchPoint<F>
where
    F: Fn(u8, u8, u8, f32) + Clone,
{
    /// Constructor は起動時に最大数分呼ばれる
    fn new(id: usize) -> Self {
        TouchPoint {
            id,
            center_location: INIT_VAL, // Invalid location initially
            intensity: 0,
            real_crnt_note: 0, // Initialize to 0, will be set when a touch is detected
            is_updated: false,
            is_touched: false,
            touching_time: 0,
            no_update_time: 0,
            offset_note: PIANO_OFFSET,
            new_location: Self::new_location_piano,
            midi_callback: None,
            history_idx: 0,
            location_history: [0f32; HISTORY_SIZE], // Store past locations for each possible note
        }
    }

    /// 新しいタッチポイントを作成する
    fn new_touch(
        &mut self,
        location: f32,
        intensity: i16,
        callback: F,
        work_mode: WorkMode,
        velocity_query: impl Fn(u8, i16) -> u8,
    ) {
        if work_mode == WorkMode::Violin {
            self.new_location = Self::new_location_violin;
            self.offset_note = VIOLIN_OFFSET;
        } else {
            self.new_location = Self::new_location_piano;
            self.offset_note = PIANO_OFFSET;
        }
        let new_note = (self.new_location)(NEW_NOTE, location);
        if let Ok(crnt_note) = new_note {
            self.center_location = location;
            self.location_history[self.history_idx] = location; // Store the initial location in history
            self.history_idx = (self.history_idx + 1) % HISTORY_SIZE; // Update the history index
            self.real_crnt_note = crnt_note; // Set the current note
            self.intensity = intensity;
            self.is_updated = true;
            self.is_touched = true;
            self.touching_time = 0; // Reset the touching time
            self.midi_callback = Some(callback);
            // MIDI Note On
            if let Some(ref midi_callback) = self.midi_callback {
                let note = self.real_crnt_note.saturating_add(self.offset_note);
                let note_on_velocity = velocity_query(note, self.intensity);
                midi_callback(
                    RINGLED_CMD_TX_ON | self.id as u8,
                    note,
                    note_on_velocity,
                    self.center_location,
                );
            }
        }
    }
    /// タッチポイントが近いかどうかを判断する
    fn is_near_here(&self, location: f32) -> bool {
        if !self.is_touched {
            return false;
        }
        (self.center_location >= location - CLOSE_RANGE)
            && (self.center_location <= location + CLOSE_RANGE)
    }
    /// タッチポイントを更新する
    fn update_touch(
        &mut self,
        location: f32,
        intensity: u16,
        velocity_query: impl Fn(u8, i16) -> u8,
    ) {
        self.center_location = location;
        self.intensity = intensity as i16;
        self.is_updated = true;
        self.is_touched = true;
        self.location_history[self.history_idx] = location; // Store the updated location in history
        self.history_idx = (self.history_idx + 1) % HISTORY_SIZE; // Update the history index
        if let Ok(updated_note) = (self.new_location)(self.real_crnt_note, location) {
            // MIDI Note On & Off
            if let Some(ref midi_callback) = self.midi_callback
                && updated_note != self.real_crnt_note
            {
                let note = updated_note.saturating_add(self.offset_note);
                let note_on_velocity = velocity_query(note, self.intensity);
                midi_callback(
                    RINGLED_CMD_TX_ON | self.id as u8,
                    note,
                    note_on_velocity,
                    self.center_location,
                );
                midi_callback(
                    RINGLED_CMD_TX_MOVED | self.id as u8, // Note Off と同じ
                    self.real_crnt_note + self.offset_note,
                    0x40,
                    self.center_location,
                );
                self.real_crnt_note = updated_note; // Update the current note
            }
        }
    }
    /// タッチポイントが離れたときの処理
    fn maybe_released(&mut self) {
        if self.no_update_time + RELEASE_WAITING_TIME > self.touching_time {
            // RELEASE_WAITING_TIME 回まで、更新のないタッチポイントは、まだ離れたと見なさない
            self.touching_time = self.touching_time.wrapping_add(1);
            return;
        }
        // MIDI Note Off
        if let Some(ref midi_callback) = self.midi_callback {
            midi_callback(
                RINGLED_CMD_TX_OFF | self.id as u8,
                self.real_crnt_note + self.offset_note,
                0x40,
                self.center_location,
            );
        }
        self.is_touched = false;
        self.center_location = INIT_VAL;
        self.intensity = 0;
    }
    fn is_touched(&self) -> bool {
        self.is_touched
    }
    fn is_updated(&self) -> bool {
        self.is_updated
    }
    fn get_location(&self) -> f32 {
        self.center_location
    }
    fn get_intensity(&self) -> i16 {
        self.intensity
    }
    /// タッチされたタッチポイントの処理が終了したので、時間更新して更新フラグを下ろす
    fn clear_updated_flag(&mut self) {
        self.touching_time = self.touching_time.wrapping_add(1);
        self.no_update_time = self.touching_time;
        self.is_updated = false;
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
    //private:
    /// crnt_note : 0-(MAX_SENS-1) 現在の位置、NEW_NOTE は新規ノート
    fn new_location_piano(crnt_note: u8, location: f32) -> Result<u8, u8> {
        // Manual round implementation for no_std
        fn round(x: f32) -> f32 {
            if x >= 0.0 {
                (x + 0.5) as i32 as f32
            } else {
                (x - 0.5) as i32 as f32
            }
        }

        let location = location.clamp(0.0, (MAX_PADS - 1) as f32); // Clamp location to valid range
        if crnt_note == NEW_NOTE {
            Ok(round(location) as u8) // Round to nearest integer for MIDI note
        } else if crnt_note < MAX_PADS as u8 {
            if (location > (crnt_note as f32 + HISTERESIS))
                || (location < (crnt_note as f32 - HISTERESIS))
            {
                // histeresis
                Ok(round(location) as u8)
            } else {
                Ok(crnt_note) // No change in note
            }
        } else {
            // Invalid note number, return TOUCH_POINT_ERROR
            Err(TOUCH_POINT_ERROR)
        }
    }
    /// crnt_note : 0-(MAX_SENS-1) 現在の位置、NEW_NOTE は新規ノート
    fn new_location_violin(crnt_note: u8, location: f32) -> Result<u8, u8> {
        // Manual round implementation for no_std
        fn round(x: f32) -> u8 {
            (x + 0.5) as u8
        }

        let location = location.clamp(0.0, (MAX_PADS - 1) as f32) / 2.0;
        if crnt_note == NEW_NOTE {
            Ok(round(location)) // Round to nearest integer for MIDI note
        } else if (0..(MAX_PADS as u8 / 2)).contains(&crnt_note) {
            if (location > (crnt_note as f32 + HISTERESIS))
                || (location < (crnt_note as f32 - HISTERESIS))
            {
                // histeresis
                Ok(round(location))
            } else {
                Ok(crnt_note) // No change in note
            }
        } else {
            // Invalid note number, return TOUCH_POINT_ERROR
            Err(TOUCH_POINT_ERROR)
        }
    }
}
// =========================================================
//      QubitTouch Class
// =========================================================
// Qubit 全体のタッチを管理するクラス
#[derive(Clone, Copy, Debug)]
pub struct QubitTouch<F>
where
    F: Fn(u8, u8, u8, f32) + Clone,
{
    pads: [Pad; MAX_PADS as usize], // パッドの状態を保持する配列
    touch_points: [TouchPoint<F>; MAX_TOUCH_POINTS], // Store detected touch points
    midi_callback: F,               // MIDI callback function
    touch_count: usize,             // Current number of touch points
    last_note: u8,                  // 最後に送信したMIDIノート番号
    on_time: u32,                   // 最後のタッチが開始してから離されるまでの時間
    // タッチ中の TouchPoint は、最後の TouchPoint の touching_time
    off_time: u32, // 最後のタッチが離されてからの時間
    vibrato: u8,   // ビブラートの強さ (0-127)
    _debug: i16,
}
impl<F> QubitTouch<F>
where
    F: Fn(u8, u8, u8, f32) + Clone,
{
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
        // 10ms tick前提: 1秒=100, 3秒=300
        const BASE_VELOCITY: i16 = 64;
        const ONE_SEC_TICKS: u32 = 100;
        const THREE_SEC_TICKS: u32 = 300;

        // on_time補正:
        // - 1秒未満: 0..+32 を線形加算
        // - 1秒超〜3秒: 0..-32 を線形減算
        let on_adjust = if on_time < ONE_SEC_TICKS {
            on_time as i16 * 32 / ONE_SEC_TICKS as i16
        } else {
            let over = (on_time - ONE_SEC_TICKS).min(THREE_SEC_TICKS - ONE_SEC_TICKS);
            -(over as i16 * 32 / (THREE_SEC_TICKS - ONE_SEC_TICKS) as i16)
        };

        // off_time補正:
        // - 0: 補正なし
        // - 0超〜1秒未満: +24..0 を線形加算
        // - 1秒以上: 補正なし
        let off_adjust = if off_time == 0 {
            0
        } else if off_time < ONE_SEC_TICKS {
            (((ONE_SEC_TICKS - off_time) as i16) * 24 / (ONE_SEC_TICKS as i16 - 1)).max(0)
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
    // デフォルトの Note On ベロシティ計算: 強度に基づいて 100..255 の範囲で線形に決定
    fn default_note_on_velocity(intensity: i16) -> u8 {
        if intensity < 0 {
            0
        } else if intensity > 255 {
            255
        } else {
            (100 + (intensity >> 4)) as u8
        }
    }

    pub fn new(cb: F) -> Self {
        QubitTouch {
            pads: [Pad::new(); MAX_PADS as usize],
            touch_points: core::array::from_fn(|i| TouchPoint::<F>::new(i)),
            midi_callback: cb,
            touch_count: 0,
            last_note: 0,
            on_time: 0,
            off_time: 0,
            vibrato: 0,
            _debug: 0,
        }
    }
    /// タッチポイントの数を取得する
    pub fn _deb_val(&self) -> i16 {
        self._debug
    }
    /// パッドの値を設定する
    pub fn set_value(&mut self, pad_num: usize, value: u16) {
        if let Some(pad) = self.pads.get_mut(pad_num) {
            pad.set_crnt(value);
        }
    }
    /// パッドの値を取得する
    pub fn _get_value(&self, pad_num: usize) -> u16 {
        self.pads.get(pad_num).map(|p| p.get_crnt()).unwrap_or(0)
    }
    /// タッチポイントの数を取得する
    pub fn _get_touch_count(&self) -> usize {
        self.touch_count
    }
    /// タッチポイントの参照を取得する（非const版）
    pub fn get_touch_point(&mut self, index: usize) -> Option<&mut TouchPoint<F>> {
        self.touch_points.get_mut(index)
    }
    /// タッチポイントのconst参照を取得する（const版）
    pub fn _touch_point(&self, index: usize) -> Option<&TouchPoint<F>> {
        self.touch_points.get(index)
    }
    /// 指定されたパッドの参照を取得する(マイナス値からMAX_PADSを超えた値を考慮)
    pub fn proper_pad(&mut self, pad_num: i32) -> &mut Pad {
        let index = pad_num.rem_euclid(MAX_PADS as i32) as usize;
        &mut self.pads[index]
    }
    fn display_location(&mut self, id: usize) {
        let loc = if let Some(tp) = self.get_touch_point(id) {
            (tp.get_location() * 100.0) as i32
        } else {
            10000
        };
        match id {
            0 => TOUCH0.store(loc, core::sync::atomic::Ordering::Relaxed),
            1 => TOUCH1.store(loc, core::sync::atomic::Ordering::Relaxed),
            2 => TOUCH2.store(loc, core::sync::atomic::Ordering::Relaxed),
            3 => TOUCH3.store(loc, core::sync::atomic::Ordering::Relaxed),
            _ => {}
        }
    }
    /// 差分の符号が変化した時、その位置の値がある一定の値以上なら、そこをタッチポイントとする
    pub fn seek_and_update_touch_point(&mut self, work_mode: WorkMode) {
        let mut temp_touch_point: [(f32, f32, i16); MAX_TOUCH_POINTS] =
            [(INIT_VAL, INIT_VAL, 0); MAX_TOUCH_POINTS];
        let mut temp_index = 0;

        // 1: 全パッドを走査し、差分の符号が変化した箇所をタッチポイントとみなし、temp_touch_point に保存
        self.scan_pads(&mut temp_touch_point, &mut temp_index);

        // 2: タッチポイントの前後のパッドの値を足し、平均をとってパッドの位置と強度を確定する
        self.decide_touch_point(&mut temp_touch_point, &mut temp_index);

        // 2.5: 同一周期内の近接候補を統合し、1つのタッチ候補として扱う
        self.merge_close_candidates(&mut temp_touch_point, &mut temp_index);

        // 3: 前回値と比較し、近いものを紐付け、タッチポイントを更新または追加する
        self.collate_touch_point(&temp_touch_point, temp_index, work_mode);

        // 4: 更新のなかったタッチポイントを削除する
        self.erase_touch_point();

        // 5: 時間計測とビブラート捕捉
        self.update_durations_and_vibrato(work_mode);
    }
    fn scan_pads(
        &mut self,
        temp_touch_point: &mut [(f32, f32, i16); MAX_TOUCH_POINTS],
        temp_index: &mut usize,
    ) {
        let mut diff_before: i16 = 0;
        for i in 0..=MAX_PADS {
            // Get previous pad value first
            let prev_value = self.proper_pad(i as i32 - 1).get_crnt();
            // Now get current pad and set diff
            let diff_after = self.proper_pad(i as i32).diff_from_before(prev_value);
            if (diff_after > 0) && (diff_before < 0) {
                // - -> + 変化時
                let value = prev_value; // Note the top flag
                if value > TOUCH_THRESHOLD {
                    // Example threshold for touch point
                    temp_touch_point[*temp_index] = (
                        (if i >= 1 { i - 1 } else { i - 1 + MAX_PADS }) as f32,
                        INIT_VAL,
                        0,
                    );
                    *temp_index += 1;
                    if *temp_index >= MAX_TOUCH_POINTS {
                        break; // Prevent overflow of touch points
                    }
                }
            }
            diff_before = diff_after;
        }
        self.touch_count = *temp_index; // Update the touch count
    }
    fn decide_touch_point(
        &mut self,
        temp_touch_point: &mut [(f32, f32, i16); MAX_TOUCH_POINTS],
        temp_index: &mut usize,
    ) {
        for tp in temp_touch_point.iter_mut().take(*temp_index) {
            let tp_idx = tp.0 as i32;
            let mut sum: i16 = 0;
            let mut locate: f32 = 0.0;
            let mut center_value: i16 = 0;
            let mut max_neighbor_value: i16 = 0;

            for j in 0..(FINGER_RANGE * 2 + 1) {
                let window_idx = j as i32 - FINGER_RANGE as i32;
                let neighbor_pad = self.proper_pad(tp_idx + window_idx);
                let tp_value = neighbor_pad.get_crnt() as i16;
                sum += tp_value;
                locate += (tp_idx + window_idx) as f32 * tp_value as f32; // Wrap around to ensure valid index
                if window_idx == 0 {
                    center_value = tp_value;
                } else if tp_value > max_neighbor_value {
                    max_neighbor_value = tp_value;
                }
            }

            if sum > 0 {
                // 単発パルス対策: 中心1電極が過度に支配的で近傍が弱い候補は無効化する
                let center_dominant = center_value * 100 >= sum * SINGLE_PAD_SPIKE_RATIO_PERMILLE;
                let weak_neighbors = max_neighbor_value * SINGLE_PAD_SPIKE_NEIGHBOR_DIV <= center_value;
                if center_dominant && weak_neighbors {
                    *tp = (tp.0, INIT_VAL, 0);
                    continue;
                }

                locate /= sum as f32; // Calculate the average location based on intensity
                *tp = (tp.0, locate, sum);
            } else {
                // 無効なタッチポイント（sum=0だった場合）は初期値のままにする
                *tp = (tp.0, INIT_VAL, 0);
            }
        }
    }
    /// 同一周期内で近接した候補を1つに統合する
    fn merge_close_candidates(
        &mut self,
        temp_touch_point: &mut [(f32, f32, i16); MAX_TOUCH_POINTS],
        temp_index: &mut usize,
    ) {
        let mut merged: [(f32, f32, i16); MAX_TOUCH_POINTS] =
            [(INIT_VAL, INIT_VAL, 0); MAX_TOUCH_POINTS];
        let mut merged_count = 0usize;
        let mut used = [false; MAX_TOUCH_POINTS];

        for i in 0..*temp_index {
            if used[i] {
                continue;
            }
            let seed = temp_touch_point[i];
            if seed.1 == INIT_VAL {
                used[i] = true;
                continue;
            }
            used[i] = true;

            // 同一クラスタ内は「最も強い候補」を代表として残す
            let mut best = seed;
            let seed_loc = seed.1;
            for j in (i + 1)..*temp_index {
                if used[j] {
                    continue;
                }
                let cand = temp_touch_point[j];
                if cand.1 == INIT_VAL {
                    used[j] = true;
                    continue;
                }
                if (cand.1 - seed_loc).abs() <= CANDIDATE_MERGE_RANGE {
                    used[j] = true;
                    if cand.2 > best.2 {
                        best = cand;
                    }
                }
            }

            if merged_count < MAX_TOUCH_POINTS {
                merged[merged_count] = best;
                merged_count += 1;
            }
        }

        temp_touch_point[..merged_count].copy_from_slice(&merged[..merged_count]);
        for tp in temp_touch_point.iter_mut().skip(merged_count) {
            *tp = (INIT_VAL, INIT_VAL, 0);
        }
        *temp_index = merged_count;
        self.touch_count = merged_count;
    }
    fn collate_touch_point(
        &mut self,
        temp_touch_point: &[(f32, f32, i16); MAX_TOUCH_POINTS],
        temp_index: usize,
        work_mode: WorkMode,
    ) {
        let mut display_index: [bool; MAX_TOUCH_POINTS] = [false; MAX_TOUCH_POINTS];
        let mut latest_note: Option<u8> = None;
        let velocity_ctx = (work_mode, self.last_note, self.on_time, self.off_time);
        for tp in temp_touch_point.iter().take(temp_index) {
            let location = tp.1;
            let intensity = tp.2;
            // 無効なタッチポイント（sum=0だった場合）はスキップ
            if location == INIT_VAL {
                continue;
            }

            // 現在のタッチポイントで近いものがあれば、タッチポイントがそこから移動したとみなす
            let mut nearest = MAX_PADS as f32;
            let mut nearest_tp: Option<&mut TouchPoint<F>> = None;

            for tp in self.touch_points.iter_mut() {
                if !tp.is_touched() || tp.is_updated() {
                    continue; // Skip if the touch point is not touched
                }
                let diff = (tp.get_location() - location).abs();
                if diff < nearest {
                    // 近いものがあれば、タッチポイントを更新する
                    nearest = diff;
                    nearest_tp = Some(tp);
                }
            }

            if let Some(nearest_tp) = nearest_tp
                && nearest_tp.is_near_here(location)
            {
                // 一番近いタッチポイントが、現在のタッチポイントに近い場合
                let before_note = nearest_tp.real_crnt_note;
                nearest_tp.update_touch(location, intensity as u16, |note, intensity| {
                    Self::note_on_velocity_from_context(
                        velocity_ctx.0,
                        velocity_ctx.1,
                        velocity_ctx.2,
                        velocity_ctx.3,
                        note,
                        intensity,
                    )
                });
                if nearest_tp.real_crnt_note != before_note {
                    latest_note = Some(
                        nearest_tp
                            .real_crnt_note
                            .saturating_add(nearest_tp.offset_note),
                    );
                }
                display_index[nearest_tp.id] = true; // Mark this touch point for display update
                continue; // Move to the next temp touch point
            }
            // 近いタッチポイントがない場合は、新しいタッチポイントを作成する
            let _ = self.new_touch_point(location, intensity as u16, work_mode);
        }

        if let Some(note) = latest_note {
            self.last_note = note;
        }

        // RingLEDの表示を更新する必要のあるタッチポイントのIDを収集し、まとめて表示を更新する
        display_index.iter().enumerate().for_each(|(id, &update)| {
            if update {
                self.display_location(id);
            }
        });
    }
    /// LEDを点灯させるためのコールバック関数をコールする
    pub fn lighten_leds<G>(&self, led_callback: G)
    where
        G: Fn(f32, i16),
    {
        let mut empty = true;
        for tp in self.touch_points.iter() {
            if tp.is_touched() {
                let location = tp.get_location();
                let intensity = tp.get_intensity();
                led_callback(location, intensity);
                empty = false;
            }
        }
        if empty {
            // Call the callback with default values if no touch points are active
            led_callback(-1.0, 0);
        }
    }
    fn new_touch_point(
        &mut self,
        location: f32,
        intensity: u16,
        work_mode: WorkMode,
    ) -> Option<u8> {
        let velocity_ctx = (work_mode, self.last_note, self.on_time, self.off_time);

        let touched = self
            .touch_points
            .iter_mut()
            .find(|tp| !tp.is_touched())
            .map(|tp| {
                tp.new_touch(
                    location,
                    intensity as i16,
                    self.midi_callback.clone(),
                    work_mode,
                    |note, intensity| {
                        Self::note_on_velocity_from_context(
                            velocity_ctx.0,
                            velocity_ctx.1,
                            velocity_ctx.2,
                            velocity_ctx.3,
                            note,
                            intensity,
                        )
                    },
                );
                (tp.id, tp.real_crnt_note.saturating_add(tp.offset_note))
            });
        if let Some((id, note)) = touched {
            self.last_note = note;
            self.display_location(id); // Update the display for this touch point
            Some(note)
        } else {
            None
        }
    }
    fn erase_touch_point(&mut self) {
        let mut display_ids: [Option<usize>; MAX_TOUCH_POINTS] = [None; MAX_TOUCH_POINTS];
        let mut display_count = 0;
        let mut released_note: Option<u8> = None;
        let mut released_on_time: Option<u32> = None;

        for tp in self.touch_points.iter_mut() {
            // タッチされていないポイントは処理不要
            if tp.is_touched() {
                if !tp.is_updated() {
                    let current_note = tp.real_crnt_note.saturating_add(tp.offset_note);
                    let current_on_time = tp.touching_time;
                    tp.maybe_released();
                    if !tp.is_touched() {
                        released_note = Some(current_note);
                        released_on_time = Some(current_on_time);
                    }
                    display_ids[display_count] = Some(tp.id);
                    display_count += 1;
                } else {
                    tp.clear_updated_flag(); // Clear the updated flag for the next cycle
                }
            }
        }

        if let Some(note) = released_note {
            self.last_note = note;
        }
        if let Some(on_time) = released_on_time {
            self.on_time = on_time;
        }

        for id in display_ids.iter().take(display_count).flatten() {
            self.display_location(*id); // Update the display for this touch point
        }
    }

    fn update_durations_and_vibrato(&mut self, work_mode: WorkMode) {
        let mut touched = false;
        let mut latest_on_time = self.on_time;
        let mut few_hz_detected: Option<f32> = None;

        for tp in self.touch_points.iter() {
            if tp.is_touched() {
                touched = true;
                latest_on_time = tp.touching_time;
                if work_mode == WorkMode::Violin && few_hz_detected.is_none() {
                    few_hz_detected = tp.detect_few_hz_round_trip_hz();
                }
            }
        }

        if touched {
            self.on_time = latest_on_time;
            self.off_time = 0;
            ANY_TOUCH.store(true, Ordering::Relaxed);
            let vib = few_hz_detected.map_or(0, |hz| (hz * 10.0) as u32).min(127) as u8;
            if vib != self.vibrato {
                let dpt = (vib * 2).min(48);
                (self.midi_callback)(
                    MIDI_CC, 1,   // CC number for vibrato
                    dpt, // CC value for pitch bend (centered at 64)
                    0.0, // location is not relevant for vibrato command
                );
                (self.midi_callback)(
                    MIDI_CC, 19, // CC number for vibrato
                    vib, 0.0, // location is not relevant for vibrato command
                );
                self.vibrato = vib;
            }
            DEBUG_VALUE.store(vib as u32, Ordering::Relaxed);
        } else {
            self.off_time = self.off_time.wrapping_add(1);
            ANY_TOUCH.store(false, Ordering::Relaxed);
            DEBUG_VALUE.store(0, Ordering::Relaxed);
        }
    }

    /// Violinモード向けのMIDI NoteOn velocity算出
    ///
    /// 算出に使用する値:
    /// - last_note: 直前に送信したノート
    /// - on_time:   最後のタッチ継続時間(10ms単位)
    /// - off_time:  最後に離してからの経過時間(10ms単位)
    #[allow(dead_code)]
    pub fn calc_violin_note_on_velocity(&self, note: u8) -> u8 {
        Self::calc_violin_note_on_velocity_from(self.last_note, self.on_time, self.off_time, note)
    }
}
