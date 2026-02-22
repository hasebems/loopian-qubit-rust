//  Created by Hasebe Masahiko on 2026/02/15.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
use crate::constants;
use crate::{TOUCH0, TOUCH1, TOUCH2, TOUCH3};

// =========================================================
//      Touch Constants
// =========================================================
pub const MAX_PADS: u16 = constants::TOTAL_QT_KEYS as u16; // MAX_SENS;
pub const TOUCH_THRESHOLD: u16 = 40; // Threshold for touch point detection
pub const CLOSE_RANGE: f32 = 3.0; // 同じタッチと見做される 10msec あたりの動作範囲
pub const FINGER_RANGE: usize = 3; // Maximum serial numbers of one touch point
pub const HISTERESIS: f32 = 0.7; // Hysteresis value for touch point detection

// =========================================================
//      Pad Class
// =========================================================
#[derive(Copy, Clone, Debug)]
pub struct Pad {
    mv_avg_value: u16, // 移動平均(MAX_MOVING_AVERAGEで割らない)
    diff_from_before: i16,
    top_flag: bool,
    past_value: [u16; Pad::MAX_MOVING_AVERAGE],
    past_index: usize,
}
impl Pad {
    const MAX_MOVING_AVERAGE: usize = 4; // Number of samples for moving average}

    fn new() -> Self {
        Pad {
            mv_avg_value: 0,
            diff_from_before: 0,
            top_flag: false,
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
    fn set_diff_from_before(&mut self, value_before: u16) -> i16 {
        self.diff_from_before = value_before as i16 - self.mv_avg_value as i16;
        self.diff_from_before
    }
    fn _diff_from_before(&self) -> i16 {
        self.diff_from_before
    }
    fn note_top_flag(&mut self) {
        self.top_flag = true;
    }
    fn _is_top_flag(&self) -> bool {
        self.top_flag
    }
    /// Reset top flag
    fn _clear_top_flag(&mut self) {
        self.top_flag = false;
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
    midi_callback: Option<F>, // MIDI callback function
}
impl<F> TouchPoint<F>
where
    F: Fn(u8, u8, u8, f32) + Clone,
{
    const NEW_NOTE: u8 = 0xff;
    const TOUCH_POINT_ERROR: u8 = 0xfe;
    const INIT_VAL: f32 = 100.0;
    const OFFSET_NOTE: u8 = constants::KEYBD_LO - 4;

    /// Constructor は起動時に最大数分呼ばれる
    fn new(id: usize) -> Self {
        TouchPoint {
            id,
            center_location: Self::INIT_VAL, // Invalid location initially
            intensity: 0,
            real_crnt_note: 0, // Initialize to 0, will be set when a touch is detected
            is_updated: false,
            is_touched: false,
            touching_time: 0,
            no_update_time: 0,
            midi_callback: None,
        }
    }

    /// 新しいタッチポイントを作成する
    fn new_touch(&mut self, location: f32, intensity: i16, callback: F) {
        if let Ok(crnt_note) = self.new_location(Self::NEW_NOTE, location) {
            self.center_location = location;
            self.real_crnt_note = crnt_note; // Set the current note
            self.intensity = intensity;
            self.is_updated = true;
            self.is_touched = true;
            self.touching_time = 0; // Reset the touching time
            self.midi_callback = Some(callback);
            // MIDI Note On
            if let Some(ref midi_callback) = self.midi_callback {
                midi_callback(
                    constants::RINGLED_CMD_TX_ON | self.id as u8,
                    self.real_crnt_note + Self::OFFSET_NOTE,
                    self.intensity_to_velocity(self.intensity),
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
    fn update_touch(&mut self, location: f32, intensity: u16) {
        self.center_location = location;
        self.intensity = intensity as i16;
        self.is_updated = true;
        self.is_touched = true;
        if let Ok(updated_note) = self.new_location(self.real_crnt_note, location) {
            // MIDI Note On & Off
            if let Some(ref midi_callback) = self.midi_callback
                && updated_note != self.real_crnt_note
            {
                midi_callback(
                    constants::RINGLED_CMD_TX_ON | self.id as u8,
                    updated_note + Self::OFFSET_NOTE,
                    self.intensity_to_velocity(self.intensity),
                    self.center_location,
                );
                midi_callback(
                    constants::RINGLED_CMD_TX_MOVED | self.id as u8, // Note Off と同じ
                    self.real_crnt_note + Self::OFFSET_NOTE,
                    0x40,
                    self.center_location,
                );
                self.real_crnt_note = updated_note; // Update the current note
            }
        }
    }
    /// タッチポイントが離れたときの処理
    fn maybe_released(&mut self) {
        if self.no_update_time + 5 > self.touching_time {
            // 5回以上更新がなかったら、タッチポイントを離れたとみなす
            self.touching_time = self.touching_time.wrapping_add(1);
            return;
        }
        // MIDI Note Off
        if let Some(ref midi_callback) = self.midi_callback {
            midi_callback(
                constants::RINGLED_CMD_TX_OFF | self.id as u8,
                self.real_crnt_note + Self::OFFSET_NOTE,
                0x40,
                self.center_location,
            );
        }
        self.is_touched = false;
        self.center_location = Self::INIT_VAL;
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
    fn clear_updated_flag(&mut self) {
        self.touching_time = self.touching_time.wrapping_add(1);
        self.no_update_time = self.touching_time;
        self.is_updated = false;
    }

    //private:
    /// crnt_note : 0-(MAX_SENS-1) 現在の位置、NEW_NOTE は新規ノート
    fn new_location(&self, crnt_note: u8, location: f32) -> Result<u8, u8> {
        // Manual round implementation for no_std
        fn round(x: f32) -> f32 {
            if x >= 0.0 {
                (x + 0.5) as i32 as f32
            } else {
                (x - 0.5) as i32 as f32
            }
        }

        let location = if location < 0.0 {
            0.0 // Ensure location is non-negative
        } else if location >= (MAX_PADS - 1) as f32 {
            (MAX_PADS - 1) as f32 // Ensure location is within bounds
        } else {
            location
        };
        if crnt_note == Self::NEW_NOTE {
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
            Err(Self::TOUCH_POINT_ERROR)
        }
    }
    fn intensity_to_velocity(&self, intensity: i16) -> u8 {
        // Convert intensity to MIDI velocity (0-127)
        if intensity < 0 {
            return 0; // No touch
        } else if intensity > 255 {
            return 255; // Max MIDI velocity
        }
        (100 + (intensity >> 4)) as u8
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
    touch_points: [TouchPoint<F>; constants::MAX_TOUCH_POINTS], // Store detected touch points
    midi_callback: F,               // MIDI callback function
    touch_count: usize,             // Current number of touch points
    _debug: i16,
}
impl<F> QubitTouch<F>
where
    F: Fn(u8, u8, u8, f32) + Clone,
{
    pub fn new(cb: F) -> Self {
        QubitTouch {
            pads: [Pad::new(); MAX_PADS as usize],
            touch_points: core::array::from_fn(|i| TouchPoint::<F>::new(i)),
            midi_callback: cb,
            touch_count: 0,
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
    /*void seek_and_update_touch_point() {
        std::array<std::tuple<size_t, float, int16_t>, MAX_TOUCH_POINTS> temp_touch_point;
        temp_touch_point.fill(std::make_tuple(TouchPoint::INIT_VAL, TouchPoint::INIT_VAL, 0));
        size_t temp_index = 0;

        // 1: 全パッドを走査し、差分の符号が変化した箇所をタッチポイントとみなし、temp_touch_point に保存
        int16_t diff_before = 0;
        for (size_t i = 0; i <= MAX_PADS; ++i) {
            Pad& prev_pad = proper_pad(i-1);
            int16_t diff_after = proper_pad(i).set_diff_from_before(prev_pad.get_crnt());
            if ((diff_after > 0 ) && (diff_before < 0)) { // - -> + 変化時
                int16_t value = prev_pad.get_crnt(); // Note the top flag
                if (value > TOUCH_THRESHOLD) { // Example threshold for touch point
                    prev_pad.note_top_flag();
                    std::get<0>(temp_touch_point[temp_index++]) = (i >= 1) ? i - 1 : i - 1 + MAX_PADS;
                    if (temp_index >= MAX_TOUCH_POINTS) {
                        break; // Prevent overflow of touch points
                    }
                }
            }
            diff_before = diff_after;
        }
        touch_count_ = temp_index; // Update the touch count

        // 2: タッチポイントの前後のパッドの値を足し、平均をとってパッドの位置と強度を確定する
        for (int i = 0; i < temp_index; ++i) {
            int16_t sum = 0;
            float locate = 0.0f;
            auto &tpi = temp_touch_point[i];
            int tp = static_cast<int>(std::get<0>(tpi));
            int window_idx = 0; // Initialize window index for averaging
            for (int j = 0; j < FINGER_RANGE*2 + 1; ++j) {
                window_idx = static_cast<int>(j - FINGER_RANGE);
                Pad& neighbor_pad = proper_pad(tp + window_idx);
                int16_t tp_value = neighbor_pad.get_crnt();
                sum += tp_value;
                locate += (tp + window_idx) * tp_value; // Wrap around to ensure valid index
            }
            locate /= sum; // Calculate the average location based on intensity
            std::get<1>(tpi) = locate;
            std::get<2>(tpi) = sum;
        }

        // 3: 各パッドの値を確認し、タッチポイントを更新または追加する
        for (int k = 0; k < temp_index; ++k) {
            auto &tpi = temp_touch_point[k];
            float location = std::get<1>(tpi);
            int16_t intensity = std::get<2>(tpi);
            // 現在のタッチポイントで近いものがあれば、タッチポイントがそこから移動したとみなす
            float nearest = TouchPoint::INIT_VAL;
            TouchPoint* nearest_tp = nullptr;
            for (auto& tp: touch_points_) {
                if (!tp.is_touched() || tp.is_updated()) {
                    continue; // Skip if the touch point is not touched
                }
                float diff = std::abs(tp.get_location() - location);
                if (diff < nearest) {
                    // 近いものがあれば、タッチポイントを更新する
                    nearest = diff;
                    nearest_tp = &tp;
                }
            }
            if (nearest_tp && nearest_tp->is_near_here(location)) {
                // 一番近いタッチポイントが、現在のタッチポイントに近い場合
                nearest_tp->update_touch(location, intensity);
            } else {
                new_touch_point(location, intensity, midi_callback_);
            }
        }

        // 更新のなかったタッチポイントを削除する
        erase_touch_point();
    }*/
    pub fn seek_and_update_touch_point(&mut self) {
        const INIT_VAL: f32 = 100.0; // Invalid location initially
        let mut temp_touch_point: [(f32, f32, i16); constants::MAX_TOUCH_POINTS] =
            [(INIT_VAL, INIT_VAL, 0); constants::MAX_TOUCH_POINTS];
        let mut temp_index = 0;

        // 1: 全パッドを走査し、差分の符号が変化した箇所をタッチポイントとみなし、temp_touch_point に保存
        let mut diff_before: i16 = 0;
        for i in 0..=MAX_PADS {
            // Get previous pad value first
            let prev_value = self.proper_pad(i as i32 - 1).get_crnt();
            // Now get current pad and set diff
            let diff_after = self.proper_pad(i as i32).set_diff_from_before(prev_value);
            if (diff_after > 0) && (diff_before < 0) {
                // - -> + 変化時
                let value = prev_value; // Note the top flag
                if value > TOUCH_THRESHOLD {
                    // Example threshold for touch point
                    self.proper_pad(i as i32 - 1).note_top_flag();
                    temp_touch_point[temp_index] = (
                        (if i >= 1 { i - 1 } else { i - 1 + MAX_PADS }) as f32,
                        INIT_VAL,
                        0,
                    );
                    temp_index += 1;
                    if temp_index >= constants::MAX_TOUCH_POINTS {
                        break; // Prevent overflow of touch points
                    }
                }
            }
            diff_before = diff_after;
        }
        self.touch_count = temp_index; // Update the touch count

        // 2: タッチポイントの前後のパッドの値を足し、平均をとってパッドの位置と強度を確定する
        for ttp in temp_touch_point.iter_mut().take(temp_index) {
            let mut sum: i32 = 0;
            let mut locate: f32 = 0.0;
            let tp = ttp.0 as usize;
            for j in 0..(FINGER_RANGE * 2 + 1) {
                let window_idx = j as i32 - FINGER_RANGE as i32;
                let neighbor_pad = self.proper_pad(tp as i32 + window_idx);
                let tp_value = neighbor_pad.get_crnt();
                sum += tp_value as i32;
                locate += ((tp as i32 + window_idx) as f32) * tp_value as f32; // Wrap around to ensure valid index
            }
            // ゼロ除算を防ぐ
            if sum > 0 {
                locate /= sum as f32; // Calculate the average location based on intensity
                ttp.1 = locate;
                ttp.2 = sum as i16;
            } else {
                // sumが0の場合はこのタッチポイントを無効化（INIT_VALのまま）
                continue; // このイテレーションをスキップ
            }
        }
        // 3: 各パッドの値を確認し、タッチポイントを更新または追加する
        for ttp in temp_touch_point.iter().take(temp_index) {
            let location = ttp.1;
            let intensity = ttp.2;

            // 無効なタッチポイント（sum=0だった場合）はスキップ
            if location == INIT_VAL {
                continue;
            }

            // 現在のタッチポイントで近いものがあれば、タッチポイントがそこから移動したとみなす
            let mut nearest: f32 = TouchPoint::<F>::INIT_VAL;
            let mut nearest_idx: Option<usize> = None;
            for (idx, tp) in self.touch_points.iter().enumerate() {
                if !tp.is_touched() || tp.is_updated() {
                    continue; // Skip if the touch point is not touched
                }
                let diff = (tp.get_location() - location).abs();
                if diff < nearest {
                    // 近いものがあれば、タッチポイントを更新する
                    nearest = diff;
                    nearest_idx = Some(idx);
                }
            }

            // 借用の問題を回避するため、インデックスを使って処理を分離
            if let Some(idx) = nearest_idx {
                let should_update = self.touch_points[idx].is_near_here(location);
                if should_update {
                    // 一番近いタッチポイントが、現在のタッチポイントに近い場合
                    self.touch_points[idx].update_touch(location, intensity as u16);
                    self.display_location(idx);
                } else {
                    self.new_touch_point(location, intensity as u16);
                }
            } else {
                self.new_touch_point(location, intensity as u16);
            }
        }

        // 更新のなかったタッチポイントを削除する
        self.erase_touch_point();
    }
    /// LEDを点灯させるためのコールバック関数をコールする
    /*void lighten_leds(std::function<void(float, int16_t)> led_callback) {
        bool empty = true;
        for (auto& tp : touch_points_) {
            if (tp.is_touched()) {
                float location = tp.get_location();
                int16_t intensity = tp.get_intensity();
                led_callback(location, intensity);
                empty = false;
            }
        }
        if (empty) {
            // Call the callback with default values if no touch points are active
            led_callback(-1.0f, 0);
        }
    }*/
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
    fn new_touch_point(&mut self, location: f32, intensity: u16) {
        let cb = self.midi_callback.clone();
        let id = self
            .touch_points
            .iter_mut()
            .find(|tp| !tp.is_touched())
            .map(|tp| {
                tp.new_touch(location, intensity as i16, cb);
                tp.id
            });
        if let Some(id) = id {
            self.display_location(id); // Update the display for this touch point
        }
    }
    fn erase_touch_point(&mut self) {
        let mut display_ids: [Option<usize>; constants::MAX_TOUCH_POINTS] =
            [None; constants::MAX_TOUCH_POINTS];
        let mut display_count = 0;

        for tp in self.touch_points.iter_mut() {
            // タッチされていないポイントは処理不要
            if tp.is_touched() {
                if !tp.is_updated() {
                    tp.maybe_released();
                    display_ids[display_count] = Some(tp.id);
                    display_count += 1;
                } else {
                    tp.clear_updated_flag(); // Clear the updated flag for the next cycle
                }
            }
        }

        for i in 0..display_count {
            if let Some(id) = display_ids[i] {
                self.display_location(id); // Update the display for this touch point
            }
        }
    }
}
