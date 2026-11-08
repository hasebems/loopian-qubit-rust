//  Created by Hasebe Masahiko on 2026/02/15.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! 指 1 本のタッチ: depth・position の層（doc/touch_analysis.md §3）
//!
//! 電極毎の output（`electrode`）から、1 本の指とみなした電極の depth（output の合計）と
//! position（重心）を求め、周期をまたいで指を追跡する（`TouchTracker`）。
//! リングのつなぎ目をまたいでも、なるべく正しく出す（§3.6）。
//! ノートやビブラートなどの演奏情報は知らない。それは `playinfo` が、ここの結果から作る
//!
//! - 電極の output は、呼び出し側が用意した `[u16]`（ファームは配列、PC は Vec）に持つ
//! - 10ms 毎に `update` を呼ぶ前提（`RELEASE_WAITING_TIME` など）
//! - ファームの `src/touch/qtouch.rs` から移したもの（2026-11-08、doc/touch_analysis.md §7）

// =========================================================
//      Constants
// =========================================================
pub const MAX_TOUCH_POINTS: usize = 4; // Maximum number of touch points to track
// Threshold for touch point detection。Pad の移動平均（4 サンプルの合計）を外したので、
// 以前の 32 を 1/4 にして同じ感度にしている（doc/touch_baseline.md §4.6）
pub const TOUCH_THRESHOLD: u16 = 8;
pub const CLOSE_RANGE: f32 = 3.0; // 同じタッチと見做される 10msec あたりの片側変化量
pub const FINGER_RANGE: usize = 3; // Maximum serial numbers of one touch point

const INIT_VAL: f32 = 100.0; // Invalid location initially
const RELEASE_WAITING_TIME: u32 = 5; // Number of cycles to wait before considering a touch point released
const CANDIDATE_MERGE_RANGE: f32 = 1.6; // Merge close candidates in the same cycle

/// 電極の並び: 数と、リングかどうか（doc/touch_analysis.md §6.1 の 3・4）
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub(crate) pads: u16,
    pub(crate) ring: bool,
}
impl Layout {
    /// a − b。リングでは近い向きに測る（−pads/2 〜 pads/2）
    pub(crate) fn diff(self, a: f32, b: f32) -> f32 {
        let d = a - b;
        if self.ring {
            let n = self.pads as f32;
            let d = wrap(d, n);
            if d > n / 2.0 { d - n } else { d }
        } else {
            d
        }
    }
    /// 2 つの position の距離。リングでは近い向きに測る（例: 96 電極で 95.8 と 0.1 は 0.3）
    pub(crate) fn distance(self, a: f32, b: f32) -> f32 {
        let d = self.diff(a, b);
        if d < 0.0 { -d } else { d }
    }
    /// position を有効な範囲に収める。リングでは 0 以上 pads 未満に巡回させ、
    /// リングでなければ 0〜pads−1 に切り詰める
    pub(crate) fn normalize(self, x: f32) -> f32 {
        let n = self.pads as f32;
        if self.ring {
            wrap(x, n)
        } else {
            x.clamp(0.0, n - 1.0)
        }
    }
}

/// x を 0 以上 n 未満に巡回させる（no_std なので f32::rem_euclid の代わり）
pub(crate) fn wrap(x: f32, n: f32) -> f32 {
    let mut r = x % n;
    if r < 0.0 {
        r += n;
    }
    // −0.000001 % 96 + 96 が丸めで 96.0 になることがある
    if r >= n { r - n } else { r }
}

// =========================================================
//      TouchPoint: 追跡している指 1 本
// =========================================================
/// この周期の追跡の結果。`playinfo` が読んで、Note On/Off などを決める
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Track {
    None,     // 変化なし（触れていない、または離れるのを待っている）
    Started,  // 新しいタッチが始まった
    Followed, // 同じ指が続いている（動いたかもしれない）
    Released, // 離れた
}

/// 追跡している指 1 本。depth・position と継続時間を持つ
#[derive(Clone, Copy, Debug)]
pub struct TouchPoint {
    center_location: f32, // 離れた後も、最後の position を残す（Note Off で使う）
    intensity: i16,
    is_updated: bool,
    is_touched: bool,
    touching_time: u32,
    no_update_time: u32,
    track: Track,
}

impl TouchPoint {
    const INITIAL: Self = TouchPoint {
        center_location: INIT_VAL, // Invalid location initially
        intensity: 0,
        is_updated: false,
        is_touched: false,
        touching_time: 0,
        no_update_time: 0,
        track: Track::None,
    };
    /// 新しいタッチを始める
    fn start(&mut self, location: f32, intensity: i16) {
        self.center_location = location;
        self.intensity = intensity;
        self.is_updated = true;
        self.is_touched = true;
        self.touching_time = 0; // Reset the touching time
        self.track = Track::Started;
    }
    /// タッチポイントが近いかどうかを判断する。リングでは距離を近い向きに測る
    fn is_near_here(&self, location: f32, layout: Layout) -> bool {
        if !self.is_touched {
            return false;
        }
        layout.distance(self.center_location, location) <= CLOSE_RANGE
    }
    /// 同じ指が続いている: depth・position を更新する
    fn follow(&mut self, location: f32, intensity: i16) {
        self.center_location = location;
        self.intensity = intensity;
        self.is_updated = true;
        self.is_touched = true;
        self.track = Track::Followed;
    }
    /// この周期に更新が無かった: RELEASE_WAITING_TIME 回続いたら離れたとみなす
    fn maybe_released(&mut self) {
        if self.no_update_time + RELEASE_WAITING_TIME > self.touching_time {
            // RELEASE_WAITING_TIME 回まで、更新のないタッチポイントは、まだ離れたと見なさない
            self.touching_time = self.touching_time.wrapping_add(1);
            return;
        }
        self.is_touched = false;
        self.intensity = 0;
        self.track = Track::Released;
    }
    /// タッチされたタッチポイントの処理が終了したので、時間更新して更新フラグを下ろす
    fn clear_updated_flag(&mut self) {
        self.touching_time = self.touching_time.wrapping_add(1);
        self.no_update_time = self.touching_time;
        self.is_updated = false;
    }
    fn is_updated(&self) -> bool {
        self.is_updated
    }
    /// 触れているか（離れるのを待っている間も true）
    pub fn is_touched(&self) -> bool {
        self.is_touched
    }
    /// position（電極番号を単位とする実数、§3.3）。触れていないときは 100.0
    pub fn get_location(&self) -> f32 {
        if self.is_touched {
            self.center_location
        } else {
            INIT_VAL
        }
    }
    /// 最後の position。離れた後も残る（離れた周期の Note Off で使う）
    pub fn last_location(&self) -> f32 {
        self.center_location
    }
    /// depth（§3.2）
    pub fn get_intensity(&self) -> i16 {
        self.intensity
    }
    /// この周期の追跡の結果
    pub fn track(&self) -> Track {
        self.track
    }
    /// 触れ始めてからの周期数（離れるのを待っている間も数える）
    pub fn touching_time(&self) -> u32 {
        self.touching_time
    }
}

// =========================================================
//      TouchTracker: 電極の output から指を求めて追跡する
// =========================================================
/// P は電極毎の output の置き場所（`[u16; N]` や `Vec<u16>`）
#[derive(Clone, Copy, Debug)]
pub struct TouchTracker<P> {
    pads: P,                                      // 電極毎の output
    touch_points: [TouchPoint; MAX_TOUCH_POINTS], // 追跡している指
    touch_count: usize,                           // Current number of touch points
    any_touched: bool, // 最後の解析で、どれかのタッチポイントが触れていたか
    ring: bool,        // 電極がリングに並んでいるか（最後の電極と電極 0 が隣り合う）
}

impl<P> TouchTracker<P>
where
    P: AsRef<[u16]> + AsMut<[u16]>,
{
    /// pads は全電極分を 0 で埋めて渡す。ring は電極がリングに並んでいるか（96 電極の本番の基板）。
    /// false（etude・no_pca9544 の試験用の構成）では、両端の外側を output 0 の電極として扱う
    pub fn new(pads: P, ring: bool) -> Self {
        TouchTracker {
            pads,
            touch_points: [TouchPoint::INITIAL; MAX_TOUCH_POINTS],
            touch_count: 0,
            any_touched: false,
            ring,
        }
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
    pub fn touch_point(&self, index: usize) -> Option<&TouchPoint> {
        self.touch_points.get(index)
    }
    /// すべてのタッチポイント（添字が id）
    pub fn touch_points(&self) -> &[TouchPoint; MAX_TOUCH_POINTS] {
        &self.touch_points
    }
    /// 最後の解析で、どれかのタッチポイントが触れていたか（離れるのを待っている間も含む）
    pub fn is_any_touched(&self) -> bool {
        self.any_touched
    }
    /// 電極数
    fn max_pads(&self) -> u16 {
        self.pads.as_ref().len() as u16
    }
    /// 電極の並び
    pub(crate) fn layout(&self) -> Layout {
        Layout {
            pads: self.max_pads(),
            ring: self.ring,
        }
    }
    /// 電極 center からの相対位置 lo..=hi の電極の output から、(position, depth) を求める。
    /// 重心は center からの相対位置で計算し、リングでは 0 以上電極数未満に巡回させる。
    /// リングでなければ範囲外の電極は 0 なので、重心に効かない（doc/touch_analysis.md §6.1 の 3）。
    /// output がすべて 0 なら None
    fn window_centroid(&self, center: i32, lo: i32, hi: i32) -> Option<(f32, i16)> {
        let mut sum: i32 = 0;
        let mut moment: i32 = 0;
        for j in lo..=hi {
            let v = self.pad(center + j) as i32;
            sum += v;
            moment += j * v;
        }
        if sum <= 0 {
            return None;
        }
        let position = self
            .layout()
            .normalize(center as f32 + moment as f32 / sum as f32);
        Some((position, sum.min(i16::MAX as i32) as i16))
    }
    /// 指定された電極の output を取得する（範囲外の番号も受け付ける）。
    /// リングでは番号を巡回させ、リングでなければ範囲外は 0
    fn pad(&self, pad_num: i32) -> u16 {
        let pads = self.pads.as_ref();
        if self.ring {
            pads[pad_num.rem_euclid(pads.len() as i32) as usize]
        } else {
            usize::try_from(pad_num)
                .ok()
                .and_then(|i| pads.get(i))
                .copied()
                .unwrap_or(0)
        }
    }
    /// 1 周期分の解析。10ms 毎に呼ぶ。指を求めて追跡し、結果を各 TouchPoint の `track()` に残す
    pub fn update(&mut self) {
        for tp in self.touch_points.iter_mut() {
            tp.track = Track::None;
        }
        if self.pads.as_ref().is_empty() {
            return;
        }
        let mut temp_touch_point: [(f32, f32, i16); MAX_TOUCH_POINTS] =
            [(INIT_VAL, INIT_VAL, 0); MAX_TOUCH_POINTS];
        let mut temp_index = 0;

        // 1: 全パッドを走査し、山をタッチの候補として temp_touch_point に保存
        self.scan_pads(&mut temp_touch_point, &mut temp_index);

        // 2: 候補の前後のパッドから、position と depth を求める
        self.decide_touch_point(&mut temp_touch_point, &mut temp_index);

        // 2.5: 同一周期内の近接候補を統合し、1つのタッチ候補として扱う
        self.merge_close_candidates(&mut temp_touch_point, &mut temp_index);

        // 3: 前回の指と比較し、近いものを紐付けて更新、無ければ新しい指にする
        self.track_touch_points(&temp_touch_point, temp_index);

        // 4: 更新のなかった指が離れたかを判断する
        self.release_touch_points();

        self.any_touched = self.touch_points.iter().any(|tp| tp.is_touched());
    }
    /// 山（両隣より output が大きい電極、またはその平らな頂上）を探し、TOUCH_THRESHOLD を超えたものを候補にする。
    /// 傾きが 0 の所（同じ値が続く所）では、直前の 0 でない傾きを引き継ぐので、
    /// 頂上が平らな山（例: 3, 10, 10, 2）も 1 つの山になる（doc/touch_analysis.md §6.1 の 1）。
    /// 候補の電極は平らな頂上の中央（偶数個なら番号の小さい側）
    fn scan_pads(
        &mut self,
        temp_touch_point: &mut [(f32, f32, i16); MAX_TOUCH_POINTS],
        temp_index: &mut usize,
    ) {
        let max_pads = self.max_pads() as i32;
        // 傾き: 電極 i-1 → i で下がれば正、上がれば負（slope(i) = pad(i-1) - pad(i)）
        let slope = |i: i32| self.pad(i - 1) as i16 - self.pad(i) as i16;
        // 調べる傾きの範囲。リングでは 0..max_pads で 1 周（電極 max_pads-1 → 0 の傾きも含む）。
        // リングでなければ、外側の 0 との傾きも含めて 0..=max_pads
        let (end, mut diff_before) = if self.ring {
            // 1 周目の始めの傾きは、つなぎ目の手前の最後の 0 でない傾きから引き継ぐ
            let init = (1..max_pads)
                .rev()
                .map(slope)
                .find(|&d| d != 0)
                .unwrap_or(0);
            (max_pads, init)
        } else {
            (max_pads + 1, 0)
        };
        for i in 0..end {
            let diff_after = slope(i);
            if diff_after == 0 {
                continue; // 平らな所は、直前の傾きを引き継ぐ
            }
            if diff_after > 0 && diff_before < 0 {
                // 上り → 下り。電極 i-1 が頂上の右端
                let top = i - 1;
                let value = self.pad(top);
                if value > TOUCH_THRESHOLD {
                    // 平らな頂上の左端まで戻り、中央を候補にする
                    let mut width = 1;
                    while width < max_pads && self.pad(top - width) == value {
                        width += 1;
                    }
                    let left = top - width + 1;
                    let center = (left + (width - 1) / 2).rem_euclid(max_pads);
                    temp_touch_point[*temp_index] = (center as f32, INIT_VAL, 0);
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
        // 1 つの電極だけが突出した候補をノイズとして捨てる判定（SINGLE_PAD_SPIKE_*）は外した（2026-11-08、
        // doc/touch_analysis.md §6.1 の 10）。指が電極の真上を強く押したときの本当のタッチまで捨てていたため。
        // 1 周期だけ跳ねるノイズは、electrode の中央値フィルタで時間方向に取り除いている
        let fr = FINGER_RANGE as i32;
        for tp in temp_touch_point.iter_mut().take(*temp_index) {
            *tp = match self.window_centroid(tp.0 as i32, -fr, fr) {
                Some((position, depth)) => (tp.0, position, depth),
                // 無効なタッチポイント（sum=0だった場合）は初期値のままにする
                None => (tp.0, INIT_VAL, 0),
            };
        }
    }
    /// 同一周期内で近接した候補を1つに統合する（doc/touch_analysis.md §6.1 の 7）。
    /// まとまりのどれかの候補から CANDIDATE_MERGE_RANGE 以内の候補を、加わらなくなるまで加える。
    /// 2 つ以上まとめたときは、まとめた候補の窓を合わせた範囲（一番左の山 − FINGER_RANGE 〜
    /// 一番右の山 + FINGER_RANGE）で position と depth を計算し直す
    fn merge_close_candidates(
        &mut self,
        temp_touch_point: &mut [(f32, f32, i16); MAX_TOUCH_POINTS],
        temp_index: &mut usize,
    ) {
        let layout = self.layout();
        let n = *temp_index;
        let mut merged: [(f32, f32, i16); MAX_TOUCH_POINTS] =
            [(INIT_VAL, INIT_VAL, 0); MAX_TOUCH_POINTS];
        let mut merged_count = 0usize;
        let mut used = [false; MAX_TOUCH_POINTS];

        for i in 0..n {
            if used[i] {
                continue;
            }
            used[i] = true;
            let seed = temp_touch_point[i];
            if seed.1 == INIT_VAL {
                continue;
            }

            // まとまりを広げる
            let mut member = [false; MAX_TOUCH_POINTS];
            member[i] = true;
            loop {
                let mut added = false;
                for j in 0..n {
                    if used[j] {
                        continue;
                    }
                    let cand = temp_touch_point[j];
                    if cand.1 == INIT_VAL {
                        used[j] = true;
                        continue;
                    }
                    let near = (0..n).any(|k| {
                        member[k]
                            && layout.distance(temp_touch_point[k].1, cand.1)
                                <= CANDIDATE_MERGE_RANGE
                    });
                    if near {
                        used[j] = true;
                        member[j] = true;
                        added = true;
                    }
                }
                if !added {
                    break;
                }
            }

            let result = if member.iter().filter(|&&m| m).count() == 1 {
                seed
            } else {
                // 山の電極の、seed の山からの相対位置（リングでは近い向き）で範囲を決める
                let (mut lo, mut hi) = (0i32, 0i32);
                for k in (0..n).filter(|&k| member[k]) {
                    let d = layout.diff(temp_touch_point[k].0, seed.0) as i32;
                    lo = lo.min(d);
                    hi = hi.max(d);
                }
                let fr = FINGER_RANGE as i32;
                lo -= fr;
                // 同じ電極を 2 回数えないよう、範囲は電極数までにする
                hi = (hi + fr).min(lo + layout.pads as i32 - 1);
                match self.window_centroid(seed.0 as i32, lo, hi) {
                    Some((position, depth)) => (seed.0, position, depth),
                    None => seed,
                }
            };
            if merged_count < MAX_TOUCH_POINTS {
                merged[merged_count] = result;
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
    /// 候補を、前の周期の指（TouchPoint）に結び付ける。近いものが無ければ新しい指にする。
    /// TouchPoint が空いていなければ、その候補は捨てる
    fn track_touch_points(
        &mut self,
        temp_touch_point: &[(f32, f32, i16); MAX_TOUCH_POINTS],
        temp_index: usize,
    ) {
        let layout = self.layout();
        for cand in temp_touch_point.iter().take(temp_index) {
            let (location, intensity) = (cand.1, cand.2);
            // 無効なタッチポイント（sum=0だった場合）はスキップ
            if location == INIT_VAL {
                continue;
            }

            // 現在のタッチポイントで近いものがあれば、タッチポイントがそこから移動したとみなす
            let mut nearest = layout.pads as f32;
            let mut nearest_tp: Option<&mut TouchPoint> = None;
            for tp in self.touch_points.iter_mut() {
                if !tp.is_touched() || tp.is_updated() {
                    continue; // Skip if the touch point is not touched
                }
                let diff = layout.distance(tp.get_location(), location);
                if diff < nearest {
                    nearest = diff;
                    nearest_tp = Some(tp);
                }
            }
            if let Some(tp) = nearest_tp
                && tp.is_near_here(location, layout)
            {
                tp.follow(location, intensity);
                continue;
            }
            // 近いタッチポイントがない場合は、空いているタッチポイントで新しいタッチを始める
            if let Some(tp) = self.touch_points.iter_mut().find(|tp| !tp.is_touched()) {
                tp.start(location, intensity);
            }
        }
    }
    /// この周期に更新されなかった指が、離れたかを判断する
    fn release_touch_points(&mut self) {
        for tp in self.touch_points.iter_mut() {
            // タッチされていないポイントは処理不要
            if tp.is_touched() {
                if !tp.is_updated() {
                    tp.maybe_released();
                } else {
                    tp.clear_updated_flag(); // Clear the updated flag for the next cycle
                }
            }
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
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    /// 1 周期分の output を置いて解析する
    pub(crate) fn put_values<P: AsRef<[u16]> + AsMut<[u16]>>(
        tracker: &mut TouchTracker<P>,
        values: &[(usize, u16)],
    ) {
        for k in 0..tracker.max_pads() as usize {
            tracker.set_value(k, 0);
        }
        for &(k, v) in values {
            tracker.set_value(k, v);
        }
        tracker.update();
    }

    /// 候補（山の電極）を返す
    fn candidates<const N: usize>(values: [u16; N], ring: bool) -> Vec<f32> {
        let mut tracker = TouchTracker::new(values, ring);
        let mut temp = [(INIT_VAL, INIT_VAL, 0); MAX_TOUCH_POINTS];
        let mut n = 0;
        tracker.scan_pads(&mut temp, &mut n);
        temp[..n].iter().map(|t| t.0).collect()
    }

    /// 電極 4〜6 に 1 本の指を置くと、depth・position が出て Started になり、
    /// 続けると Followed、離すと RELEASE_WAITING_TIME の後に Released になる
    #[test]
    fn one_finger_is_tracked() {
        let mut tracker = TouchTracker::new([0u16; 12], false);
        put_values(&mut tracker, &[(4, 10), (5, 30), (6, 10)]);
        // depth = 50、position = (4×10 + 5×30 + 6×10) / 50 = 5.0
        let tp = tracker.touch_point(0).unwrap();
        assert!(tp.is_touched());
        assert_eq!(tp.get_intensity(), 50);
        assert_eq!(tp.get_location(), 5.0);
        assert_eq!(tp.track(), Track::Started);
        assert!(tracker.is_any_touched());

        put_values(&mut tracker, &[(4, 10), (5, 30), (6, 10)]);
        assert_eq!(tracker.touch_point(0).unwrap().track(), Track::Followed);

        let mut tracks = Vec::new();
        for _ in 0..10 {
            put_values(&mut tracker, &[]);
            tracks.push(tracker.touch_point(0).unwrap().track());
        }
        assert_eq!(tracks.iter().filter(|&&t| t == Track::Released).count(), 1);
        let tp = tracker.touch_point(0).unwrap();
        assert!(!tp.is_touched());
        assert_eq!(tp.get_location(), INIT_VAL);
        assert_eq!(tp.last_location(), 5.0);
        assert!(!tracker.is_any_touched());
    }

    /// 1 つの電極だけが突出していても（指が電極の真上を強く押した）、タッチになる（§6.1 の 10）
    #[test]
    fn dominant_single_electrode_is_a_touch() {
        let mut tracker = TouchTracker::new([0, 0, 0, 0, 30, 200, 30, 0, 0, 0, 0, 0u16], false);
        tracker.update();
        let tp = tracker.touch_point(0).unwrap();
        assert!(tp.is_touched());
        assert_eq!(tp.get_intensity(), 260);
        assert_eq!(tp.get_location(), 5.0);
        // 記録にあった形（山 18、隣 2）
        let mut tracker = TouchTracker::new([0, 0, 0, 0, 0, 18, 2, 0, 0, 0, 0, 0u16], false);
        tracker.update();
        assert!(tracker.touch_point(0).unwrap().is_touched());
    }

    /// 頂上が平らな山も 1 つの山になる（§6.1 の 1）。候補は頂上の中央（偶数個なら小さい側）
    #[test]
    fn plateau_peak_is_found() {
        assert_eq!(
            candidates([0, 3, 10, 10, 2, 0, 0, 0, 0, 0, 0, 0], false),
            [2.0]
        );
        assert_eq!(
            candidates([0, 3, 10, 10, 10, 2, 0, 0, 0, 0, 0, 0], false),
            [3.0]
        );
        // 山の途中の平らな所（上りの途中）は山ではない
        assert_eq!(
            candidates([0, 3, 9, 9, 20, 2, 0, 0, 0, 0, 0, 0], false),
            [4.0]
        );
        // 平らな頂上の山でも、タッチになる
        let mut tracker = TouchTracker::new([0, 3, 20, 20, 3, 0, 0, 0, 0, 0, 0, 0u16], false);
        tracker.update();
        let tp = tracker.touch_point(0).unwrap();
        assert!(tp.is_touched());
        assert_eq!(tp.get_location(), 2.5);
    }

    /// リングでない構成では、両端の電極は隣り合わない（§6.1 の 4）
    #[test]
    fn edges_are_not_neighbors_without_ring() {
        // 電極 0 の山。反対の端（電極 11）の output は窓に入らない
        let mut tracker = TouchTracker::new([30, 10, 0, 0, 0, 0, 0, 0, 0, 0, 5, 5u16], false);
        tracker.update();
        let tp = tracker.touch_point(0).unwrap();
        assert!(tp.is_touched());
        assert_eq!(tp.get_intensity(), 40);
        assert_eq!(tp.get_location(), 0.25);
        // 最後の電極の山も候補になる
        assert_eq!(
            candidates([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 30], false),
            [11.0]
        );
    }

    /// リングでは、つなぎ目をまたぐ山も 1 回だけ見つかる
    #[test]
    fn ring_finds_peak_at_seam_once() {
        assert_eq!(
            candidates([10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 20], true),
            [11.0]
        );
        assert_eq!(
            candidates([20, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10], true),
            [0.0]
        );
        // 平らな頂上がつなぎ目をまたぐ（電極 11 と 0）: 中央の小さい側は電極 11
        assert_eq!(
            candidates([20, 3, 0, 0, 0, 0, 0, 0, 0, 0, 3, 20], true),
            [11.0]
        );
    }

    /// リングでは、つなぎ目の近くの position も 0 以上電極数未満になる（§6.1 の 3）
    #[test]
    fn ring_position_wraps_at_seam() {
        let mut tracker = TouchTracker::new([0u16; 12], true);
        let mut position = |values: &[(usize, u16)]| {
            put_values(&mut tracker, values);
            let p = tracker.touch_point(0).unwrap().get_location();
            // 次のために離す
            for _ in 0..10 {
                put_values(&mut tracker, &[]);
            }
            p
        };
        assert_eq!(position(&[(11, 30), (0, 30)]), 11.5);
        assert_eq!(position(&[(10, 10), (11, 30), (0, 10)]), 11.0);
        assert_eq!(position(&[(11, 10), (0, 30), (1, 10)]), 0.0);
        // 0 − 10/60 → 11.833…
        let p = position(&[(11, 20), (0, 30), (1, 10)]);
        assert!((p - (12.0 - 10.0 / 60.0)).abs() < 1e-4, "{p}");
    }

    /// リングでは、指がつなぎ目を越えても同じ指として追跡する
    #[test]
    fn ring_finger_crosses_seam_as_same_touch() {
        let mut tracker = TouchTracker::new([0u16; 12], true);
        put_values(&mut tracker, &[(10, 10), (11, 30), (0, 10)]);
        put_values(&mut tracker, &[(11, 10), (0, 30), (1, 10)]);
        let tp = tracker.touch_point(0).unwrap();
        assert_eq!(tp.track(), Track::Followed);
        assert_eq!(tp.get_location(), 0.0);
        assert!(!tracker.touch_point(1).unwrap().is_touched());
    }

    /// まとめた候補は、窓を合わせた範囲で計算し直す（§6.1 の 7）
    #[test]
    fn merged_candidates_are_recomputed() {
        let mut tracker = TouchTracker::new([0u16; 12], false);
        // 山は電極 3 と 5（電極 1 と 8 の 8 はしきい値以下なので山にならない）。
        // 窓の重心は 3.55 と 4.60 で、1.6 以内なのでまとめる
        put_values(&mut tracker, &[(1, 8), (3, 20), (4, 5), (5, 20), (8, 8)]);
        let tp = tracker.touch_point(0).unwrap();
        // 電極 0〜8 の output: 合計 61、重心 = 3 + (−2×8 + 1×5 + 2×20 + 5×8) / 61
        assert_eq!(tp.get_intensity(), 61);
        let expected = 3.0 + 69.0 / 61.0;
        assert!((tp.get_location() - expected).abs() < 1e-4);
        assert!(!tracker.touch_point(1).unwrap().is_touched());
    }
}
