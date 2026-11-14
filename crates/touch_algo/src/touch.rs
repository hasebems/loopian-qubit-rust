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
//! - `update` に今の時刻（µs）を渡す。時間は周期の数ではなく実時間で数える（doc/touch_analysis.md §3.8）。
//!   解析は 10ms 毎を想定しているが、間隔が延びても時間の感覚はずれない（分解能が粗くなるだけ）
//! - ファームの `src/touch/qtouch.rs` から移したもの（2026-11-08、doc/touch_analysis.md §7）

// =========================================================
//      Constants
// =========================================================
pub const MAX_TOUCH_POINTS: usize = 4; // Maximum number of touch points to track
/// 区間（doc/touch_analysis.md §3.4）: output がこれより大きい電極が続く範囲を、1 つの候補とする。
/// 2 本の指の間や区間の端に残る小さな output で、区間がつながったり広がったりしないよう、0 にはしない。
/// 追跡中の指は近くに区間があれば続くので、これが OFF のしきい値にもあたる（§3.5）。
/// 2026-11-13、まずは 5 で始める
pub const SEGMENT_THRESHOLD: u16 = 5;
/// 新しい指を始める depth（§3.5）: 区間の depth がこれより大きく、近くに追跡中の指が無ければ、新しい指にする。
/// 追跡中の指は、depth によらず続く。2026-11-13、まずは 10 で始める
pub const TOUCH_ON_DEPTH: i16 = 10;
pub const CLOSE_RANGE: f32 = 3.0; // 同じタッチと見做される 10msec あたりの片側変化量

const INIT_VAL: f32 = 100.0; // Invalid location initially
/// 指を最後に見つけてからこの時間（µs）を超えたら、離れたとみなす。それまでは保持する
/// 2026-11-14 に 50ms → 20ms（Note Off を早くするため。区間で探すようになってから途切れが減った。
/// doc/touch_analysis.md §3.5）。10ms 毎の解析では、見失ってから 3 周期目（30ms 後）に離れたとする
const RELEASE_WAITING_US: u32 = 20_000;
/// 1 周期に取る区間の数の上限。超えたら depth の小さい区間を捨てる
const MAX_CANDIDATES: usize = 16;
/// 近い区間をまとめる距離（doc/touch_analysis.md §3.7.1）。隣り合う区間の position の差がこれ以内なら、
/// 1 本のタッチとしてまとめる。今まで 2 本だったとき・新しく触れたときに使う。
/// §3.7 の初めの値は 6 電極だったが、区間で探すと遠すぎたので 4 にした（2026-11-13）
pub const MERGE_DISTANCE: f32 = 4.0;
/// 1 本だったタッチを分ける距離（§3.7.1）。今まで 1 本だったときは、これより離れるまで 1 本のまま。
/// MERGE_DISTANCE との差がヒステリシスになる
pub const SPLIT_DISTANCE: f32 = 7.0;
/// 前の周期の指を数える範囲を、2 つの区間の position の外側へ広げる幅（電極）
const MERGE_COUNT_MARGIN: f32 = 1.0;

/// 電極の並び: 数と、リングかどうか（doc/touch_analysis.md §3.6）
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
    /// a から右へ b までの距離（b − a）。リングでは右回りに測る（0 〜 pads 未満）
    pub(crate) fn ahead(self, a: f32, b: f32) -> f32 {
        if self.ring {
            wrap(b - a, self.pads as f32)
        } else {
            b - a
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

/// 1 周期の候補: 区間（まとめたときは、まとめた範囲）と、そこから求めた position と depth。
/// first・last は電極番号（last を含む）。リングでは巡回させる前の番号で、last が電極数以上になることがある
#[derive(Clone, Copy, Debug)]
struct Candidate {
    first: i32,
    last: i32,
    position: f32,
    depth: i16,
}
impl Candidate {
    const EMPTY: Self = Candidate {
        first: 0,
        last: 0,
        position: INIT_VAL,
        depth: 0,
    };
}

/// 追跡している指 1 本。depth・position と継続時間を持つ
#[derive(Clone, Copy, Debug)]
pub struct TouchPoint {
    center_location: f32, // 離れた後も、最後の position を残す（Note Off で使う）
    intensity: i16,
    is_updated: bool,
    is_touched: bool,
    last_seen_us: u32, // 最後に見つけた（Started・Followed の）時刻
    track: Track,
}

impl TouchPoint {
    const INITIAL: Self = TouchPoint {
        center_location: INIT_VAL, // Invalid location initially
        intensity: 0,
        is_updated: false,
        is_touched: false,
        last_seen_us: 0,
        track: Track::None,
    };
    /// 新しいタッチを始める
    fn start(&mut self, location: f32, intensity: i16, now_us: u32) {
        self.center_location = location;
        self.intensity = intensity;
        self.is_updated = true;
        self.is_touched = true;
        self.last_seen_us = now_us;
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
    fn follow(&mut self, location: f32, intensity: i16, now_us: u32) {
        self.center_location = location;
        self.intensity = intensity;
        self.is_updated = true;
        self.is_touched = true;
        self.last_seen_us = now_us;
        self.track = Track::Followed;
    }
    /// この周期に更新が無かった: 最後に見つけてから RELEASE_WAITING_US を超えたら離れたとみなす
    fn maybe_released(&mut self, now_us: u32) {
        if now_us.wrapping_sub(self.last_seen_us) <= RELEASE_WAITING_US {
            return; // まだ離れたと見なさない（保持する）
        }
        self.is_touched = false;
        self.intensity = 0;
        self.track = Track::Released;
    }
    /// この周期の処理が終わったので、更新フラグを下ろす
    fn clear_updated_flag(&mut self) {
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
    now_us: u32,       // 最後の解析の時刻（update に渡された値）
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
            now_us: 0,
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
    /// リングでなければ範囲外の電極は 0 なので、重心に効かない（doc/touch_analysis.md §3.6）。
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
    /// 最後の解析の時刻（µs。`update` に渡された値）。`playinfo` が時間を数えるのに使う
    pub fn now_us(&self) -> u32 {
        self.now_us
    }
    /// 1 周期分の解析。now_us は今の時刻（µs。約 71 分で一周してよい）。10ms 毎を想定しているが、
    /// 時間は実時間で数えるので、間隔が延びてもよい。指を求めて追跡し、結果を各 TouchPoint の `track()` に残す
    pub fn update(&mut self, now_us: u32) {
        self.now_us = now_us;
        for tp in self.touch_points.iter_mut() {
            tp.track = Track::None;
        }
        if self.pads.as_ref().is_empty() {
            return;
        }
        let mut candidates = [Candidate::EMPTY; MAX_CANDIDATES];

        // 1: output が SEGMENT_THRESHOLD を超える電極が続く区間を探し、区間毎に position と depth を求める
        let count = self.scan_segments(&mut candidates);

        // 2: 近い区間を 1 本のタッチにまとめ、depth の大きい順に並べる（§3.7.1）
        let count = self.merge_segments(&mut candidates, count);
        candidates[..count].sort_unstable_by(|a, b| b.depth.cmp(&a.depth));
        self.touch_count = count;

        // 3: 前回の指と比較し、近いものを紐付けて更新、無ければ新しい指にする
        self.track_touch_points(&candidates[..count]);

        // 4: 更新のなかった指が離れたかを判断する
        self.release_touch_points();

        self.any_touched = self.touch_points.iter().any(|tp| tp.is_touched());
    }
    /// 区間（doc/touch_analysis.md §3.4）を探し、区間毎の position と depth を candidates に電極の順に入れて、
    /// その数を返す。区間は、output が SEGMENT_THRESHOLD より大きい電極が続く範囲。depth・position は区間の中の
    /// output をそのまま使う（`window_centroid`）。
    /// リングでは SEGMENT_THRESHOLD 以下の電極の次から 1 周調べるので、つなぎ目をまたぐ区間も 1 つになる。
    /// 全部の電極が SEGMENT_THRESHOLD を超えたときは、1 周を 1 つの区間にする。
    /// 区間が MAX_CANDIDATES を超えたら、depth の小さいものを捨てる（電極の順は保つ）
    fn scan_segments(&self, candidates: &mut [Candidate; MAX_CANDIDATES]) -> usize {
        let n = self.max_pads() as i32;
        let above = |i: i32| self.pad(i) > SEGMENT_THRESHOLD;
        let mut count = 0usize;
        let start = if self.ring {
            match (0..n).find(|&i| !above(i)) {
                Some(low) => low + 1,
                None => {
                    if let Some(c) = self.candidate(0, n - 1) {
                        candidates[0] = c;
                        count = 1;
                    }
                    return count;
                }
            }
        } else {
            0
        };
        let mut i = 0;
        while i < n {
            if !above(start + i) {
                i += 1;
                continue;
            }
            let first = start + i;
            while i < n && above(start + i) {
                i += 1;
            }
            let Some(c) = self.candidate(first, start + i - 1) else {
                continue;
            };
            if count < MAX_CANDIDATES {
                candidates[count] = c;
                count += 1;
            } else if let Some((min_idx, min)) =
                candidates.iter().enumerate().min_by_key(|(_, c)| c.depth)
                && min.depth < c.depth
            {
                // depth の最も小さい区間を抜いて詰め、最後に足す
                candidates.copy_within(min_idx + 1.., min_idx);
                candidates[MAX_CANDIDATES - 1] = c;
            }
        }
        count
    }
    /// 電極 first〜last（last を含む）の output から候補を作る。output がすべて 0 なら None
    fn candidate(&self, first: i32, last: i32) -> Option<Candidate> {
        self.window_centroid(first, 0, last - first)
            .map(|(position, depth)| Candidate {
                first,
                last,
                position,
                depth,
            })
    }
    /// 近い区間を 1 本のタッチにまとめ、まとめた後の数を返す（doc/touch_analysis.md §3.7.1）。
    /// 電極の順に、まとめた候補と次の区間の position の差が `merge_limit` 以内ならまとめる。
    /// まとめた候補の depth・position は、範囲（間の SEGMENT_THRESHOLD 以下の電極も含む）の output から求め直す。
    /// リングでは、最後の候補と最初の候補（つなぎ目をまたいで隣り合う）も比べる
    fn merge_segments(&self, candidates: &mut [Candidate; MAX_CANDIDATES], count: usize) -> usize {
        if count < 2 {
            return count;
        }
        let layout = self.layout();
        let mut merged = 0usize;
        for k in 1..count {
            let (cur, next) = (candidates[merged], candidates[k]);
            if layout.ahead(cur.position, next.position)
                <= self.merge_limit(cur.position, next.position)
                && let Some(c) = self.candidate(cur.first, next.last)
            {
                candidates[merged] = c;
            } else {
                merged += 1;
                candidates[merged] = next;
            }
        }
        let mut count = merged + 1;
        if self.ring && count >= 2 {
            let (last, first) = (candidates[count - 1], candidates[0]);
            if layout.ahead(last.position, first.position)
                <= self.merge_limit(last.position, first.position)
                && let Some(c) = self.candidate(last.first, first.last + layout.pads as i32)
            {
                candidates[0] = c;
                count -= 1;
            }
        }
        count
    }
    /// 区間 a・b（position。b は a から右へ測る）をまとめる距離。2 つの position の間（外側へ MERGE_COUNT_MARGIN 広げる）に
    /// 前の周期の指が 1 本だけあれば、今まで 1 本だったとして SPLIT_DISTANCE。
    /// 2 本以上、または 1 本も無ければ MERGE_DISTANCE（§3.7.1）
    fn merge_limit(&self, a: f32, b: f32) -> f32 {
        let layout = self.layout();
        let span = layout.ahead(a, b);
        let fingers = self
            .touch_points
            .iter()
            .filter(|tp| tp.is_touched())
            .filter(|tp| {
                // a の少し左（−MERGE_COUNT_MARGIN）から右へ測る
                let t =
                    layout.ahead(a - MERGE_COUNT_MARGIN, tp.get_location()) - MERGE_COUNT_MARGIN;
                t >= -MERGE_COUNT_MARGIN && t <= span + MERGE_COUNT_MARGIN
            })
            .count();
        if fingers == 1 {
            SPLIT_DISTANCE
        } else {
            MERGE_DISTANCE
        }
    }
    /// 候補を、前の周期の指（TouchPoint）に結び付ける。近いものが無ければ、depth が TOUCH_ON_DEPTH を
    /// 超えるときだけ新しい指にする（§3.5）。TouchPoint が空いていなければ、その候補は捨てる
    fn track_touch_points(&mut self, candidates: &[Candidate]) {
        let layout = self.layout();
        for cand in candidates {
            let (location, intensity) = (cand.position, cand.depth);

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
                tp.follow(location, intensity, self.now_us);
                continue;
            }
            // 近いタッチポイントがない場合は、depth が足りれば、空いているタッチポイントで新しいタッチを始める
            if intensity <= TOUCH_ON_DEPTH {
                continue;
            }
            if let Some(tp) = self.touch_points.iter_mut().find(|tp| !tp.is_touched()) {
                tp.start(location, intensity, self.now_us);
            }
        }
    }
    /// この周期に更新されなかった指が、離れたかを判断する
    fn release_touch_points(&mut self) {
        for tp in self.touch_points.iter_mut() {
            // タッチされていないポイントは処理不要
            if tp.is_touched() {
                if !tp.is_updated() {
                    tp.maybe_released(self.now_us);
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

    /// テストの解析の間隔（10ms）
    const PERIOD_US: u32 = 10_000;

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
        tracker.update(tracker.now_us() + PERIOD_US);
    }

    /// 候補（区間）の position を、depth の大きい順に返す
    fn candidates<const N: usize>(values: [u16; N], ring: bool) -> Vec<f32> {
        let tracker = TouchTracker::new(values, ring);
        let mut temp = [Candidate::EMPTY; MAX_CANDIDATES];
        let n = tracker.scan_segments(&mut temp);
        let n = tracker.merge_segments(&mut temp, n);
        temp[..n].sort_unstable_by(|a, b| b.depth.cmp(&a.depth));
        temp[..n].iter().map(|c| c.position).collect()
    }

    /// 指を保持する時間は実時間で数える（§3.8）: 最後に見つけてから RELEASE_WAITING_US を超えたら離れる。
    /// 解析の間隔が 10ms でも 30ms でも、離れたとするのは最後に見つけてから 20ms を超えた最初の解析
    #[test]
    fn release_waits_real_time() {
        fn released_after(period_us: u32) -> u32 {
            let mut tracker = TouchTracker::new([0u16; 12], false);
            let mut now = period_us;
            tracker.set_value(5, 30);
            tracker.update(now);
            let last_seen = now;
            tracker.set_value(5, 0);
            loop {
                now += period_us;
                tracker.update(now);
                if tracker.touch_point(0).unwrap().track() == Track::Released {
                    return now - last_seen;
                }
            }
        }
        assert_eq!(released_after(10_000), 30_000);
        assert_eq!(released_after(30_000), 30_000);
        assert_eq!(released_after(20_000), 40_000);
        assert_eq!(released_after(5_000), 25_000);
    }

    /// 電極 4〜6 に 1 本の指を置くと、depth・position が出て Started になり、
    /// 続けると Followed、離すと RELEASE_WAITING_US の後に Released になる
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

    /// 1 つの電極だけが突出していても（指が電極の真上を強く押した）、タッチになる（§3.4）
    #[test]
    fn dominant_single_electrode_is_a_touch() {
        let mut tracker = TouchTracker::new([0, 0, 0, 0, 30, 200, 30, 0, 0, 0, 0, 0u16], false);
        tracker.update(tracker.now_us() + PERIOD_US);
        let tp = tracker.touch_point(0).unwrap();
        assert!(tp.is_touched());
        assert_eq!(tp.get_intensity(), 260);
        assert_eq!(tp.get_location(), 5.0);
        // 記録にあった形（山 18、隣 2）
        let mut tracker = TouchTracker::new([0, 0, 0, 0, 0, 18, 2, 0, 0, 0, 0, 0u16], false);
        tracker.update(tracker.now_us() + PERIOD_US);
        assert!(tracker.touch_point(0).unwrap().is_touched());
    }

    /// 頂上が平らな山も、1 本の指で山が 2 つできても、区間が 1 つなら 1 つの候補になる（§3.4）
    #[test]
    fn segment_is_one_candidate() {
        assert_eq!(
            candidates([0, 3, 10, 10, 2, 0, 0, 0, 0, 0, 0, 0], false),
            [2.5]
        );
        // 山が 2 つ（電極 2 と 4）でも、間が SEGMENT_THRESHOLD を超えていれば 1 つの区間
        assert_eq!(
            candidates([0, 0, 20, 10, 20, 0, 0, 0, 0, 0, 0, 0], false),
            [3.0]
        );
        // SEGMENT_THRESHOLD 以下の電極は区間に入らない（端の 3・2 は重心に効かない）
        let mut tracker = TouchTracker::new([0, 3, 20, 20, 3, 0, 0, 0, 0, 0, 0, 0u16], false);
        tracker.update(tracker.now_us() + PERIOD_US);
        let tp = tracker.touch_point(0).unwrap();
        assert!(tp.is_touched());
        assert_eq!(tp.get_intensity(), 40);
        assert_eq!(tp.get_location(), 2.5);
    }

    /// SEGMENT_THRESHOLD 以下の電極で区切られても、近ければ（MERGE_DISTANCE 以内）1 つにまとめ、
    /// 間の電極も含めて depth・position を求め直す。離れていれば別の候補になる（§3.7.1）
    #[test]
    fn close_segments_are_merged() {
        // 電極 3 と 5（間の 4 は 5 で区間に入らない）: 合計 55、重心 (3×20 + 4×5 + 5×30) / 55
        let tracker = TouchTracker::new([0, 0, 0, 20, 5, 30, 0, 0, 0, 0, 0, 0u16], false);
        let mut temp = [Candidate::EMPTY; MAX_CANDIDATES];
        let n = tracker.scan_segments(&mut temp);
        assert_eq!(n, 2);
        assert_eq!(tracker.merge_segments(&mut temp, n), 1);
        assert_eq!(temp[0].depth, 55);
        assert!((temp[0].position - 230.0 / 55.0).abs() < 1e-4);
        // 電極 1 と 9（8 電極離れている）は別
        assert_eq!(
            candidates([0, 20, 0, 0, 0, 0, 0, 0, 0, 30, 0, 0], false),
            [9.0, 1.0]
        );
    }

    /// 1 本だった指は SPLIT_DISTANCE を超えるまで分けない（§3.7.1）
    #[test]
    fn one_finger_splits_only_beyond_split_distance() {
        let mut tracker = TouchTracker::new([0u16; 12], false);
        // 幅広く 1 本（電極 2〜9、position 5.5）
        let wide: Vec<(usize, u16)> = (2..=9).map(|k| (k, 20)).collect();
        put_values(&mut tracker, &wide);
        assert_eq!(tracker.touch_count(), 1);
        // 2 と 8.5（差 6.5）: MERGE_DISTANCE は超えるが、1 本だったので 1 本のまま
        put_values(&mut tracker, &[(2, 20), (8, 20), (9, 20)]);
        assert_eq!(tracker.touch_count(), 1);
        assert_eq!(tracker.touch_point(0).unwrap().track(), Track::Followed);
        assert!(!tracker.touch_point(1).unwrap().is_touched());
        // 2 と 9.5（差 7.5）: SPLIT_DISTANCE を超えたので分ける
        put_values(&mut tracker, &[(2, 20), (9, 20), (10, 20)]);
        assert_eq!(tracker.touch_count(), 2);
    }

    /// 2 本だった指は MERGE_DISTANCE 以内に近づくまでまとめない（§3.7.1）
    #[test]
    fn two_fingers_merge_only_within_merge_distance() {
        let mut tracker = TouchTracker::new([0u16; 12], false);
        // 1 と 9（差 8）: 2 本
        put_values(&mut tracker, &[(1, 20), (9, 20)]);
        assert_eq!(tracker.touch_count(), 2);
        // 2 と 8.5（差 6.5）: SPLIT_DISTANCE 以内だが、2 本だったので 2 本のまま
        put_values(&mut tracker, &[(2, 20), (8, 20), (9, 20)]);
        assert_eq!(tracker.touch_count(), 2);
        // 3 と 8.5（差 5.5）: まだ 2 本
        put_values(&mut tracker, &[(3, 20), (8, 20), (9, 20)]);
        assert_eq!(tracker.touch_count(), 2);
        // 4 と 8（差 4）: MERGE_DISTANCE 以内なのでまとめる
        put_values(&mut tracker, &[(4, 20), (8, 20)]);
        assert_eq!(tracker.touch_count(), 1);
    }

    /// 近い 2 本の指の間の電極が SEGMENT_THRESHOLD をまたいで揺れても、1 本のまま往復しない（§3.7.1）
    #[test]
    fn close_fingers_do_not_flicker() {
        let mut tracker = TouchTracker::new([0u16; 12], false);
        for t in 0..16 {
            let mid = if (t / 2) % 2 == 0 { 8 } else { 4 };
            put_values(
                &mut tracker,
                &[(3, 15), (4, 25), (5, mid), (6, 20), (7, 12)],
            );
            assert_eq!(tracker.touch_count(), 1, "t={t}");
            let tp = tracker.touch_point(0).unwrap();
            assert!(tp.is_touched());
            // 間の電極も含めて求めるので、position は 4.8〜5.0 の間にとどまる
            assert!(
                (tp.get_location() - 4.9).abs() < 0.15,
                "t={t} {}",
                tp.get_location()
            );
            assert!(!tracker.touch_point(1).unwrap().is_touched());
        }
    }

    /// 新しい指は depth が TOUCH_ON_DEPTH を超えたときに始まり、
    /// 追跡中の指は depth が小さくなっても、近くに区間があれば続く（§3.5）
    #[test]
    fn on_depth_starts_and_segment_follows() {
        let mut tracker = TouchTracker::new([0u16; 12], false);
        // depth 8（区間は電極 5 だけ）: 新しい指にならない
        put_values(&mut tracker, &[(5, 8)]);
        assert!(!tracker.is_any_touched());
        // depth 12（6, 6。電極の間）: 山はどちらも 6 だが、depth で始まる
        put_values(&mut tracker, &[(5, 6), (6, 6)]);
        let tp = tracker.touch_point(0).unwrap();
        assert_eq!(tp.track(), Track::Started);
        assert_eq!(tp.get_intensity(), 12);
        assert_eq!(tp.get_location(), 5.5);
        // depth 8 に下がっても続く
        put_values(&mut tracker, &[(5, 8)]);
        let tp = tracker.touch_point(0).unwrap();
        assert_eq!(tp.track(), Track::Followed);
        assert_eq!(tp.get_intensity(), 8);
    }

    /// 区間が MAX_CANDIDATES を超えても、depth の大きい区間は捨てない
    #[test]
    fn strongest_segment_is_kept_when_many() {
        // depth 6 の区間を 8 電極おきに 19 個（まとめない距離）と、depth 50 の区間を 1 つ
        let mut values = [0u16; 160];
        for k in 0..19 {
            values[k * 8] = 6;
        }
        values[155] = 50;
        let c = candidates(values, false);
        assert_eq!(c.len(), MAX_CANDIDATES);
        assert_eq!(c[0], 155.0);
        let mut tracker = TouchTracker::new(values, false);
        tracker.update(tracker.now_us() + PERIOD_US);
        let tp = tracker.touch_point(0).unwrap();
        assert!(tp.is_touched());
        assert_eq!(tp.get_location(), 155.0);
    }

    /// リングでない構成では、両端の電極は隣り合わない（§3.6）
    #[test]
    fn edges_are_not_neighbors_without_ring() {
        // 電極 0〜1 の区間。反対の端（電極 10・11）は SEGMENT_THRESHOLD 以下なので区間にならない
        let mut tracker = TouchTracker::new([30, 10, 0, 0, 0, 0, 0, 0, 0, 0, 5, 5u16], false);
        tracker.update(tracker.now_us() + PERIOD_US);
        let tp = tracker.touch_point(0).unwrap();
        assert!(tp.is_touched());
        assert_eq!(tp.get_intensity(), 40);
        assert_eq!(tp.get_location(), 0.25);
        // 両端に区間があっても、つながらない
        assert_eq!(
            candidates([20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 30], false),
            [10.75, 0.0]
        );
    }

    /// リングでは、つなぎ目をまたぐ区間も 1 つになる
    #[test]
    fn ring_segment_at_seam_is_one() {
        // 電極 11（20）と 0（10）: 11 + 10/30
        let c = candidates([10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 20], true);
        assert_eq!(c.len(), 1);
        assert!((c[0] - (11.0 + 10.0 / 30.0)).abs() < 1e-4, "{c:?}");
        assert_eq!(
            candidates([20, 3, 0, 0, 0, 0, 0, 0, 0, 0, 3, 20], true),
            [11.5]
        );
        // リングでない構成では、つながらない
        assert_eq!(
            candidates([20, 3, 0, 0, 0, 0, 0, 0, 0, 0, 3, 20], false),
            [0.0, 11.0]
        );
        // つなぎ目の両側の近い区間（電極 1 と 22。右回りに 3）は、つなぎ目をまたいでまとめる
        let mut v = [0u16; 24];
        v[1] = 20;
        v[22] = 20;
        let c = candidates(v, true);
        assert_eq!(c.len(), 1);
        assert!((c[0] - 23.5).abs() < 1e-4, "{c:?}");
        // つなぎ目から離れた区間（電極 1 と 12）は別
        let mut v = [0u16; 24];
        v[1] = 20;
        v[12] = 30;
        assert_eq!(candidates(v, true), [12.0, 1.0]);
        // 全部の電極が SEGMENT_THRESHOLD を超えたら、1 周を 1 つの区間にする
        assert_eq!(candidates([10u16; 12], true).len(), 1);
    }

    /// リングでは、つなぎ目の近くの position も 0 以上電極数未満になる（§3.6）
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
}
