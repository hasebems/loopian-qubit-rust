//  Created by Hasebe Masahiko on 2026/02/15.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! 指 1 本のタッチの解析（doc/touch_analysis.md）
//!
//! 電極毎の output（`electrode`）から、タッチの位置（position）と強さ（depth）を求めて追跡し、
//! Note On/Off とビブラートの CC をコールバックで出す。
//! ファームの `src/touch/qtouch.rs` から移したもの（2026-11-08、doc/touch_analysis.md §7）。処理は移す前と同じ
//!
//! - 電極の output は、呼び出し側が用意した `[u16]`（ファームは配列、PC は Vec）に持つ
//! - 10ms 毎に `seek_and_update_touch_point` を呼ぶ前提（`TOUCH_SAMPLE_PERIOD_SEC` など）
//! - 共有状態（ファームの `TOUCH0-3`・`ANY_TOUCH` など）には書かない。呼び出し側が
//!   `touch_point()`・`is_any_touched()`・`vibrato()` を読んで書く

// =========================================================
//      Touch Constants
// =========================================================
pub const MAX_TOUCH_POINTS: usize = 4; // Maximum number of touch points to track
// Threshold for touch point detection。Pad の移動平均（4 サンプルの合計）を外したので、
// 以前の 32 を 1/4 にして同じ感度にしている（doc/touch_baseline.md §4.6）
pub const TOUCH_THRESHOLD: u16 = 8;
pub const CLOSE_RANGE: f32 = 3.0; // 同じタッチと見做される 10msec あたりの片側変化量
pub const FINGER_RANGE: usize = 3; // Maximum serial numbers of one touch point
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

/// 現在のノート・位置・電極数から、新しいノートを返す
type NewLocationFn = fn(u8, f32, u16) -> Result<u8, u8>;

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
        max_pads: u16,
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
        let new_note = (self.new_location)(NEW_NOTE, location, max_pads);
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
                    STATUS_NOTE_ON | self.id as u8,
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
        max_pads: u16,
        velocity_query: impl Fn(u8, i16) -> u8,
    ) {
        self.center_location = location;
        self.intensity = intensity as i16;
        self.is_updated = true;
        self.is_touched = true;
        self.location_history[self.history_idx] = location; // Store the updated location in history
        self.history_idx = (self.history_idx + 1) % HISTORY_SIZE; // Update the history index
        if let Ok(updated_note) = (self.new_location)(self.real_crnt_note, location, max_pads) {
            // MIDI Note On & Off
            if let Some(ref midi_callback) = self.midi_callback
                && updated_note != self.real_crnt_note
            {
                let note = updated_note.saturating_add(self.offset_note);
                let note_on_velocity = velocity_query(note, self.intensity);
                midi_callback(
                    STATUS_NOTE_ON | self.id as u8,
                    note,
                    note_on_velocity,
                    self.center_location,
                );
                midi_callback(
                    STATUS_NOTE_MOVED | self.id as u8, // Note Off と同じ
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
                STATUS_NOTE_OFF | self.id as u8,
                self.real_crnt_note + self.offset_note,
                0x40,
                self.center_location,
            );
        }
        self.is_touched = false;
        self.center_location = INIT_VAL;
        self.intensity = 0;
    }
    /// 触れているか（離れるのを待っている間も true）
    pub fn is_touched(&self) -> bool {
        self.is_touched
    }
    fn is_updated(&self) -> bool {
        self.is_updated
    }
    /// position（電極番号を単位とする実数）。触れていないときは 100.0
    pub fn get_location(&self) -> f32 {
        self.center_location
    }
    /// depth（doc/touch_analysis.md §3.2）
    pub fn get_intensity(&self) -> i16 {
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
    /// crnt_note : 0-(max_pads-1) 現在の位置、NEW_NOTE は新規ノート
    fn new_location_piano(crnt_note: u8, location: f32, max_pads: u16) -> Result<u8, u8> {
        // Manual round implementation for no_std
        fn round(x: f32) -> f32 {
            if x >= 0.0 {
                (x + 0.5) as i32 as f32
            } else {
                (x - 0.5) as i32 as f32
            }
        }

        let location = location.clamp(0.0, (max_pads - 1) as f32); // Clamp location to valid range
        if crnt_note == NEW_NOTE {
            Ok(round(location) as u8) // Round to nearest integer for MIDI note
        } else if crnt_note < max_pads as u8 {
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
    /// crnt_note : 0-(max_pads-1) 現在の位置、NEW_NOTE は新規ノート
    fn new_location_violin(crnt_note: u8, location: f32, max_pads: u16) -> Result<u8, u8> {
        // Manual round implementation for no_std
        fn round(x: f32) -> u8 {
            (x + 0.5) as u8
        }

        let location = location.clamp(0.0, (max_pads - 1) as f32) / 2.0;
        if crnt_note == NEW_NOTE {
            Ok(round(location)) // Round to nearest integer for MIDI note
        } else if (0..(max_pads as u8 / 2)).contains(&crnt_note) {
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
// Qubit 全体のタッチを管理するクラス。P は電極毎の output の置き場所（`[u16; N]` や `Vec<u16>`）
#[derive(Clone, Copy, Debug)]
pub struct QubitTouch<F, P>
where
    F: Fn(u8, u8, u8, f32) + Clone,
{
    pads: P,                                         // 電極毎の output
    touch_points: [TouchPoint<F>; MAX_TOUCH_POINTS], // Store detected touch points
    midi_callback: F,                                // MIDI callback function
    touch_count: usize,                              // Current number of touch points
    last_note: u8,                                   // 最後に送信したMIDIノート番号
    on_time: u32,                                    // 最後のタッチが開始してから離されるまでの時間
    // タッチ中の TouchPoint は、最後の TouchPoint の touching_time
    off_time: u32,     // 最後のタッチが離されてからの時間
    vibrato: u8,       // ビブラートの強さ (0-127)
    any_touched: bool, // 最後の解析で、どれかのタッチポイントが触れていたか
}
impl<F, P> QubitTouch<F, P>
where
    F: Fn(u8, u8, u8, f32) + Clone,
    P: AsRef<[u16]> + AsMut<[u16]>,
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
    // デフォルトの Note On ベロシティ計算: 強度に基づいて 100..255 の範囲で線形に決め、
    // それを半分にして 127 で頭打ちにする（MIDI のデータバイトは 0..127。doc/touch_analysis.md §6.3）。
    // Pad の移動平均（4 サンプルの合計）を外して強度が 1/4 になったので、
    // 半分にする前の値が以前（> 255 で上限、>> 4）と同じになるよう、> 63 と >> 2 にしている
    fn default_note_on_velocity(intensity: i16) -> u8 {
        let velocity: u16 = if intensity < 0 {
            0
        } else if intensity > 63 {
            255
        } else {
            100 + (intensity as u16 >> 2)
        };
        (velocity / 2).min(127) as u8
    }

    /// pads は全電極分を 0 で埋めて渡す
    pub fn new(cb: F, pads: P) -> Self {
        QubitTouch {
            pads,
            touch_points: core::array::from_fn(|i| TouchPoint::<F>::new(i)),
            midi_callback: cb,
            touch_count: 0,
            last_note: 0,
            on_time: 0,
            off_time: 0,
            vibrato: 0,
            any_touched: false,
        }
    }
    /// 電極数
    fn max_pads(&self) -> u16 {
        self.pads.as_ref().len() as u16
    }
    /// 電極の output を設定する
    pub fn set_value(&mut self, pad_num: usize, value: u16) {
        if let Some(pad) = self.pads.as_mut().get_mut(pad_num) {
            *pad = value;
        }
    }
    /// 電極の output を取得する
    pub fn value(&self, pad_num: usize) -> u16 {
        self.pads.as_ref().get(pad_num).copied().unwrap_or(0)
    }
    /// タッチポイントの数を取得する
    pub fn touch_count(&self) -> usize {
        self.touch_count
    }
    /// タッチポイントの参照を取得する（id は 0〜MAX_TOUCH_POINTS-1）
    pub fn touch_point(&self, index: usize) -> Option<&TouchPoint<F>> {
        self.touch_points.get(index)
    }
    /// 最後の解析で、どれかのタッチポイントが触れていたか（離れるのを待っている間も含む）
    pub fn is_any_touched(&self) -> bool {
        self.any_touched
    }
    /// ビブラートの強さ（0-127）。最後に検出した値で、触れていないときも残る
    pub fn vibrato(&self) -> u8 {
        self.vibrato
    }
    /// 指定された電極の output を取得する(マイナス値からMAX_PADSを超えた値を考慮)
    fn pad(&self, pad_num: i32) -> u16 {
        let pads = self.pads.as_ref();
        let index = pad_num.rem_euclid(pads.len() as i32) as usize;
        pads[index]
    }
    /// 差分の符号が変化した時、その位置の値がある一定の値以上なら、そこをタッチポイントとする
    pub fn seek_and_update_touch_point(&mut self, work_mode: WorkMode) {
        if self.pads.as_ref().is_empty() {
            return;
        }
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
        let max_pads = self.max_pads();
        let mut diff_before: i16 = 0;
        for i in 0..=max_pads {
            // Get previous pad value first
            let prev_value = self.pad(i as i32 - 1);
            // Now get current pad and set diff
            let diff_after = prev_value as i16 - self.pad(i as i32) as i16;
            if (diff_after > 0) && (diff_before < 0) {
                // - -> + 変化時
                let value = prev_value; // Note the top flag
                if value > TOUCH_THRESHOLD {
                    // Example threshold for touch point
                    temp_touch_point[*temp_index] = (
                        (if i >= 1 { i - 1 } else { i - 1 + max_pads }) as f32,
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
                let tp_value = self.pad(tp_idx + window_idx) as i16;
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
                let weak_neighbors =
                    max_neighbor_value * SINGLE_PAD_SPIKE_NEIGHBOR_DIV <= center_value;
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
        let max_pads = self.max_pads();
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
            let mut nearest = max_pads as f32;
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
                nearest_tp.update_touch(location, intensity as u16, max_pads, |note, intensity| {
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
                continue; // Move to the next temp touch point
            }
            // 近いタッチポイントがない場合は、新しいタッチポイントを作成する
            let _ = self.new_touch_point(location, intensity as u16, work_mode);
        }

        if let Some(note) = latest_note {
            self.last_note = note;
        }
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
        let max_pads = self.max_pads();
        let velocity_ctx = (work_mode, self.last_note, self.on_time, self.off_time);

        let touched = self
            .touch_points
            .iter_mut()
            .find(|tp| !tp.is_touched())
            .map(|tp| {
                tp.new_touch(
                    location,
                    intensity as i16,
                    max_pads,
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
                tp.real_crnt_note.saturating_add(tp.offset_note)
            });
        if let Some(note) = touched {
            self.last_note = note;
            Some(note)
        } else {
            None
        }
    }
    fn erase_touch_point(&mut self) {
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

        self.any_touched = touched;
        if touched {
            self.on_time = latest_on_time;
            self.off_time = 0;
            let vib = few_hz_detected.map_or(0, |hz| (hz * 10.0) as u32).min(127) as u8;
            if vib != self.vibrato {
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

    /// Violinモード向けのMIDI NoteOn velocity算出
    ///
    /// 算出に使用する値:
    /// - last_note: 直前に送信したノート
    /// - on_time:   最後のタッチ継続時間(10ms単位)
    /// - off_time:  最後に離してからの経過時間(10ms単位)
    pub fn calc_violin_note_on_velocity(&self, note: u8) -> u8 {
        Self::calc_violin_note_on_velocity_from(self.last_note, self.on_time, self.off_time, note)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use std::vec::Vec;

    type Log = RefCell<Vec<(u8, u8, u8)>>; // (status, note, velocity)

    /// 電極 4〜6 に 1 本の指を置くと、電極 5 の Note On が出て、離すと Note Off が出る
    #[test]
    fn one_finger_note_on_off() {
        let log: Log = RefCell::new(Vec::new());
        let mut qt = QubitTouch::new(
            |status, note, velocity, _location| log.borrow_mut().push((status, note, velocity)),
            [0u16; 12],
        );
        qt.set_value(4, 10);
        qt.set_value(5, 30);
        qt.set_value(6, 10);
        qt.seek_and_update_touch_point(WorkMode::Piano);

        // depth = 50、position = (4×10 + 5×30 + 6×10) / 50 = 5.0
        let tp = qt.touch_point(0).unwrap();
        assert!(tp.is_touched());
        assert_eq!(tp.get_intensity(), 50);
        assert_eq!(tp.get_location(), 5.0);
        assert!(qt.is_any_touched());
        // ノート = 5 + PIANO_OFFSET、ベロシティ = (100 + 50 / 4) / 2
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 5 + PIANO_OFFSET, 56)]
        );

        // 離すと、RELEASE_WAITING_TIME の後に Note Off が 1 回だけ出る
        for k in 4..=6 {
            qt.set_value(k, 0);
        }
        for _ in 0..10 {
            qt.seek_and_update_touch_point(WorkMode::Piano);
        }
        assert_eq!(
            log.borrow().as_slice(),
            &[
                (STATUS_NOTE_ON, 5 + PIANO_OFFSET, 56),
                (STATUS_NOTE_OFF, 5 + PIANO_OFFSET, 0x40)
            ]
        );
        assert!(!qt.touch_point(0).unwrap().is_touched());
        assert!(!qt.is_any_touched());
    }

    /// 指を動かして position がヒステリシスを越えると、新しいノートの Note On と古いノートの MOVED が出る
    #[test]
    fn moving_finger_changes_note() {
        let log: Log = RefCell::new(Vec::new());
        let mut qt = QubitTouch::new(
            |status, note, velocity, _location| log.borrow_mut().push((status, note, velocity)),
            [0u16; 12],
        );
        let put = |qt: &mut QubitTouch<_, [u16; 12]>, center: usize| {
            for k in 0..12 {
                qt.set_value(k, 0);
            }
            qt.set_value(center - 1, 10);
            qt.set_value(center, 30);
            qt.set_value(center + 1, 10);
            qt.seek_and_update_touch_point(WorkMode::Piano);
        };
        put(&mut qt, 5);
        put(&mut qt, 6);
        assert_eq!(
            log.borrow().as_slice(),
            &[
                (STATUS_NOTE_ON, 5 + PIANO_OFFSET, 56),
                (STATUS_NOTE_ON, 6 + PIANO_OFFSET, 56),
                (STATUS_NOTE_MOVED, 5 + PIANO_OFFSET, 0x40)
            ]
        );
    }

    /// Piano のベロシティは半分にして 127 で頭打ち（MIDI のデータバイトの範囲に収める）
    #[test]
    fn piano_velocity_is_within_midi_range() {
        type Qt = QubitTouch<fn(u8, u8, u8, f32), [u16; 1]>;
        assert_eq!(Qt::default_note_on_velocity(0), 50);
        assert_eq!(Qt::default_note_on_velocity(63), 57);
        assert_eq!(Qt::default_note_on_velocity(64), 127);
        assert_eq!(Qt::default_note_on_velocity(i16::MAX), 127);
        assert_eq!(Qt::default_note_on_velocity(-1), 0);
    }
}
