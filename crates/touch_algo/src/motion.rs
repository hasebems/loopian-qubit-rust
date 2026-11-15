//  Created by Hasebe Masahiko on 2026/11/15.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! 指 1 本の動き（doc/touch_analysis.md §4.4）
//!
//! 発音中の position の差分（前の解析との差）から、次の 2 つを求める。`playinfo` が指毎に 1 つ持つ。
//! - ビブラート: 差分の符号が変わる点（position の山と谷）の間隔から速さ、山と谷の差から深さ
//! - 動く速さ: 差分が最後に 0 になった点（向きが反転したか、止まった点）と 500ms 前の新しい方からの、
//!   動いた距離 ÷ 時間（Move のベロシティ。ポルタメント）
//!
//! position はリングのつなぎ目で 0 と電極数の間を跳ぶので、差分を足していった続きの値（`pos`）で山と谷を見る。
//! 時間は実時間（µs）で数える（§3.8）

use crate::touch::Layout;

// =========================================================
//      Constants
// =========================================================
// ビブラート（2026-11-15、ユーザー）
/// ビブラートとみなす周波数の下限・上限（Hz）。以前の検出と同じ
pub const VIBRATO_MIN_HZ: f32 = 1.5;
pub const VIBRATO_MAX_HZ: f32 = 8.0;
/// 山と谷の差がこれ（電極。隣の電極との差を 1.0 とする）以上のときだけ、山・谷として数える。
/// 10ms 毎の差分はノイズで符号が細かく変わるため。まずは以前の検出と同じ 0.3（仮）
pub const VIBRATO_MIN_SWING: f32 = 0.3;
/// ビブラートの終わり: 速さが直前の 1 / この数 以下（50% 以下）になったら終わり（仮）
pub const VIBRATO_END_RATIO: u32 = 2;

// 動く速さ（ポルタメント。2026-11-15）
/// 差分の速さ（電極/s）がこれより小さい周期は、動いていない（距離 0）とみなす。ノイズの揺れを距離に入れず、
/// 向きの反転にも数えないため。時間は捨てない（時間を捨てると速さにならない。ユーザー）。
/// 記録（2026-10-27・11-14）では、指を止めているときの速さは p90 で 1〜2 電極/s だった（仮）
pub const MOVE_MIN_SPEED: f32 = 2.0;
/// 動いていない周期がこの時間（µs）続いたら、止まった（差分が 0 になった）として、区間を取り直す（仮）
pub const MOVE_STOP_US: u32 = 50_000;
/// 速さを求める区間の長さの上限（µs）。反転（または止まった点）がこれより前なら、ここで打ち切る（ユーザー）
pub const MOVE_WINDOW_US: u32 = 500_000;
/// 区間は、この時間（µs）毎の箱に距離と時間をためて、古い箱から捨てる。解析の間隔に左右されないため。
/// 区間の端はこの単位になる
const MOVE_BUCKET_US: u32 = 50_000;
const MOVE_BUCKETS: usize = (MOVE_WINDOW_US / MOVE_BUCKET_US) as usize;

/// 覚えておく山・谷の数（ビブラートの判定に 1 往復 = 山・谷 3 つ要る）
const PEAKS: usize = 3;

// =========================================================
//      Motion
// =========================================================
/// 山か谷 1 つ: 続きの position と、その時刻
#[derive(Clone, Copy, Debug, Default)]
struct Peak {
    pos: f32,
    time_us: u32,
}

/// 動く速さの区間の箱 1 つ: 動いた距離（電極）と時間（µs）
#[derive(Clone, Copy, Debug)]
struct Bucket {
    dist: f32,
    us: u32,
}
const EMPTY_BUCKET: Bucket = Bucket { dist: 0.0, us: 0 };

/// 指 1 本の動き
#[derive(Clone, Copy, Debug)]
pub(crate) struct Motion {
    raw: f32,     // 前の解析の position（差分を求める）
    pos: f32,     // 差分を足していった続きの position（リングのつなぎ目で跳ばない）
    time_us: u32, // 前の解析の時刻

    // 動く速さ（ポルタメント）。区間（反転・止まった点からの、MOVE_WINDOW_US まで）を箱に分けてためる
    buckets: [Bucket; MOVE_BUCKETS], // [cur] が今の箱。使っているのは cur から古い方へ filled 個
    cur: usize,
    filled: usize,         // 使っている箱の数。0 は区間が無い（まだ動いていないか、取り直した）
    bucket_start_us: u32,  // 今の箱が始まった時刻
    run_dir: i8,           // 動いている向き（+1 / −1）。0 はまだ動いていない
    still_us: u32,         // 動いていない周期が続いている時間（µs）

    // 山と谷
    dir: i8,         // 今向かっている向き（+1: 増える方、−1: 減る方）。0 はまだ決まっていない
    candidate: Peak, // 今向かっている向きで、いちばん先まで行った点（次の山・谷の候補）
    peaks: [Peak; PEAKS], // 確かめた山・谷（[0] が最も新しい）
    peak_count: usize, // peaks の中の有効な数

    // ビブラート
    vibrato: bool,
    half_us: u32, // 直前の山と谷の間隔（µs）
    rate_hz: f32, // ビブラートの速さ（直前の 1 往復）
    swing: f32,   // ビブラートの深さ（直前の 1 往復の、山と谷の差の平均。電極）
}

impl Motion {
    pub(crate) const INITIAL: Self = Motion {
        raw: 0.0,
        pos: 0.0,
        time_us: 0,
        buckets: [EMPTY_BUCKET; MOVE_BUCKETS],
        cur: 0,
        filled: 0,
        bucket_start_us: 0,
        run_dir: 0,
        still_us: 0,
        dir: 0,
        candidate: Peak {
            pos: 0.0,
            time_us: 0,
        },
        peaks: [Peak {
            pos: 0.0,
            time_us: 0,
        }; PEAKS],
        peak_count: 0,
        vibrato: false,
        half_us: 0,
        rate_hz: 0.0,
        swing: 0.0,
    };

    /// 新しい指: 今の position から始める
    pub(crate) fn start(&mut self, location: f32, now_us: u32) {
        *self = Self::INITIAL;
        self.raw = location;
        self.pos = location;
        self.time_us = now_us;
        self.candidate = Peak {
            pos: location,
            time_us: now_us,
        };
    }

    /// ビブラートと判定しているか
    pub(crate) fn is_vibrato(&self) -> bool {
        self.vibrato
    }
    /// ビブラートの速さ（Hz）と深さ（電極）。ビブラートでなければ None
    pub(crate) fn vibrato(&self) -> Option<(f32, f32)> {
        self.vibrato.then_some((self.rate_hz, self.swing))
    }
    /// 動く速さ（電極/s）: 区間（反転・止まった点と MOVE_WINDOW_US 前の新しい方から今まで）で動いた距離 ÷ 時間。
    /// 区間が無ければ None
    pub(crate) fn move_speed(&self) -> Option<f32> {
        let (dist, us) = (0..self.filled)
            .map(|k| self.buckets[(self.cur + MOVE_BUCKETS - k) % MOVE_BUCKETS])
            .fold((0.0, 0u32), |(d, u), b| (d + b.dist, u.saturating_add(b.us)));
        (us > 0).then(|| dist * 1_000_000.0 / us as f32)
    }

    /// 1 周期分（指を見つけた周期）。location はこの周期の position
    pub(crate) fn update(&mut self, location: f32, layout: Layout, now_us: u32) {
        let dt = now_us.wrapping_sub(self.time_us);
        let diff = layout.diff(location, self.raw);
        self.raw = location;
        self.time_us = now_us;
        if dt == 0 {
            return;
        }
        self.pos += diff;
        self.update_run(diff, dt, now_us);
        self.update_peaks(now_us);
        self.check_end(now_us);
    }

    /// 指を見失って保持している周期（position は前のまま）。ビブラートの終わりだけ確かめる
    pub(crate) fn hold(&mut self, now_us: u32) {
        self.check_end(now_us);
    }

    /// 動く速さ: 速さが MOVE_MIN_SPEED より小さい周期は、動いていない（距離 0）として時間だけ足す。
    /// 動いていない周期が MOVE_STOP_US 続くか、向きが変わったら、差分が 0 になったとして区間を取り直す。
    /// 区間が始まるのは、動いた周期から（止まっていた時間は入れない）
    fn update_run(&mut self, diff: f32, dt: u32, now_us: u32) {
        let dist = diff.abs();
        if dist * 1_000_000.0 < MOVE_MIN_SPEED * dt as f32 {
            self.still_us = self.still_us.saturating_add(dt);
            if self.still_us >= MOVE_STOP_US {
                self.reset_run();
            } else if self.run_dir != 0 {
                self.add_run(0.0, dt, now_us);
            }
            return;
        }
        self.still_us = 0;
        let dir = if diff > 0.0 { 1 } else { -1 };
        if self.run_dir != dir {
            self.reset_run();
            self.run_dir = dir;
        }
        self.add_run(dist, dt, now_us);
    }
    /// 区間に 1 周期分を足す。今の箱が MOVE_BUCKET_US を過ぎていたら次の箱へ進み、いちばん古い箱を捨てる
    fn add_run(&mut self, dist: f32, dt: u32, now_us: u32) {
        if self.filled == 0 {
            self.cur = 0;
            self.filled = 1;
            self.buckets[0] = EMPTY_BUCKET;
            self.bucket_start_us = now_us;
        }
        for _ in 0..MOVE_BUCKETS {
            if now_us.wrapping_sub(self.bucket_start_us) < MOVE_BUCKET_US {
                break;
            }
            self.cur = (self.cur + 1) % MOVE_BUCKETS;
            self.buckets[self.cur] = EMPTY_BUCKET;
            self.filled = (self.filled + 1).min(MOVE_BUCKETS);
            self.bucket_start_us = self.bucket_start_us.wrapping_add(MOVE_BUCKET_US);
        }
        // 長く空いた（箱をすべて進めても追いつかない）ときは、今の時刻から箱を始める
        if now_us.wrapping_sub(self.bucket_start_us) >= MOVE_BUCKET_US {
            self.bucket_start_us = now_us;
        }
        let b = &mut self.buckets[self.cur];
        b.dist += dist;
        b.us = b.us.saturating_add(dt);
    }
    fn reset_run(&mut self) {
        self.filled = 0;
        self.run_dir = 0;
    }

    /// 山と谷: 今の向きでいちばん先まで行った点（候補）から、逆の向きへ VIBRATO_MIN_SWING 戻ったら、
    /// 候補を山・谷と確かめる。向きがまだ決まっていなければ、最初の点から VIBRATO_MIN_SWING 動いたら決める
    fn update_peaks(&mut self, now_us: u32) {
        let here = Peak {
            pos: self.pos,
            time_us: now_us,
        };
        let moved = self.pos - self.candidate.pos;
        if self.dir == 0 {
            if moved.abs() >= VIBRATO_MIN_SWING {
                self.dir = if moved > 0.0 { 1 } else { -1 };
                self.candidate = here;
            }
        } else if moved * self.dir as f32 > 0.0 {
            self.candidate = here; // 同じ向きに、さらに先へ
        } else if -moved * self.dir as f32 >= VIBRATO_MIN_SWING {
            let peak = self.candidate;
            self.dir = -self.dir;
            self.candidate = here;
            self.add_peak(peak);
        }
    }

    /// 山・谷を 1 つ確かめた
    /// - ビブラートでないとき: 山・谷が 3 つ（1 往復）そろい、2 つの間隔がどちらも周波数の範囲に入れば、ビブラート
    /// - ビブラートのとき: 速さと深さを更新する。間隔が直前の VIBRATO_END_RATIO 倍を超えたら（速さが半分以下）、
    ///   終わり（check_end と同じ。候補の時刻で先に終わっていることが多い）
    fn add_peak(&mut self, peak: Peak) {
        self.peaks.copy_within(0..PEAKS - 1, 1);
        self.peaks[0] = peak;
        self.peak_count = (self.peak_count + 1).min(PEAKS);
        if self.peak_count < 2 {
            return;
        }
        let half = self.peaks[0].time_us.wrapping_sub(self.peaks[1].time_us);
        if self.vibrato && half > self.half_us.saturating_mul(VIBRATO_END_RATIO) {
            self.end_vibrato();
            return;
        }
        if self.peak_count >= PEAKS {
            let prev_half = self.peaks[1].time_us.wrapping_sub(self.peaks[2].time_us);
            if self.vibrato || (in_vibrato_range(half) && in_vibrato_range(prev_half)) {
                let period_us = half.saturating_add(prev_half).max(1);
                self.vibrato = true;
                self.rate_hz = 1_000_000.0 / period_us as f32;
                self.swing = ((self.peaks[0].pos - self.peaks[1].pos).abs()
                    + (self.peaks[1].pos - self.peaks[2].pos).abs())
                    / 2.0;
            }
        }
        self.half_us = half;
    }

    /// ビブラートの終わり（doc/touch_analysis.md §4.4）: 最後の山・谷から、次の候補までの時間か、
    /// 候補が先へ進まなくなってからの時間が、直前の山と谷の間隔の VIBRATO_END_RATIO 倍を超えたら終わり。
    /// 前者はゆっくりになった、後者は止まった（揺れが VIBRATO_MIN_SWING より小さくなった）とき
    fn check_end(&mut self, now_us: u32) {
        if !self.vibrato {
            return;
        }
        let limit = self.half_us.saturating_mul(VIBRATO_END_RATIO);
        let to_candidate = self.candidate.time_us.wrapping_sub(self.peaks[0].time_us);
        let since_candidate = now_us.wrapping_sub(self.candidate.time_us);
        if to_candidate > limit || since_candidate > limit {
            self.end_vibrato();
        }
    }
    /// ビブラートを終える。次のビブラートは、また 1 往復そろってから
    fn end_vibrato(&mut self) {
        self.vibrato = false;
        self.peak_count = 0;
    }
}

/// 山と谷の間隔（µs）が、ビブラートの周波数の範囲（VIBRATO_MIN_HZ〜VIBRATO_MAX_HZ）に入るか
fn in_vibrato_range(half_us: u32) -> bool {
    let hz = 1_000_000.0 / (2.0 * half_us.max(1) as f32);
    (VIBRATO_MIN_HZ..=VIBRATO_MAX_HZ).contains(&hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD_US: u32 = 10_000;
    const LINE: Layout = Layout {
        pads: 12,
        ring: false,
    };

    /// 三角波（no_std の f32 に sin が無いので代わりにする）: 中心 center、山と谷の差 swing、周波数 hz
    fn tri(center: f32, swing: f32, hz: f32, t_us: u32) -> f32 {
        let phase = (t_us as f32 / 1_000_000.0 * hz) % 1.0; // 0〜1
        let x = if phase < 0.5 {
            phase * 4.0 - 1.0
        } else {
            3.0 - phase * 4.0
        };
        center + swing / 2.0 * x
    }

    /// 0 から duration_us まで 10ms 毎に f(t) を入れる
    fn run(m: &mut Motion, layout: Layout, from_us: u32, duration_us: u32, f: impl Fn(u32) -> f32) {
        let mut t = from_us;
        while t <= from_us + duration_us {
            m.update(f(t), layout, t);
            t += PERIOD_US;
        }
    }

    /// 5Hz・山と谷の差 0.6 の揺れはビブラート。速さと深さが求まる
    #[test]
    fn vibrato_is_detected() {
        let mut m = Motion::INITIAL;
        m.start(5.0, 0);
        run(&mut m, LINE, PERIOD_US, 1_000_000, |t| tri(5.0, 0.6, 5.0, t));
        let (hz, swing) = m.vibrato().expect("ビブラート");
        assert!((hz - 5.0).abs() < 0.6, "hz = {hz}");
        assert!((swing - 0.6).abs() < 0.1, "swing = {swing}");
    }

    /// 1 往復（山・谷 3 つ）そろうまではビブラートにしない
    #[test]
    fn vibrato_needs_one_round_trip() {
        let mut m = Motion::INITIAL;
        m.start(5.0, 0);
        // 5Hz の 1 周期（200ms）では、確かめた山・谷はまだ 2 つ以下
        run(&mut m, LINE, PERIOD_US, 200_000, |t| tri(5.0, 0.6, 5.0, t));
        assert!(!m.is_vibrato());
    }

    /// 周波数の範囲の外（10Hz・1Hz）と、揺れが VIBRATO_MIN_SWING より小さいときはビブラートにしない
    #[test]
    fn vibrato_range_and_swing() {
        for (hz, swing) in [(10.0, 0.6), (1.0, 0.6), (5.0, 0.2)] {
            let mut m = Motion::INITIAL;
            m.start(5.0, 0);
            run(&mut m, LINE, PERIOD_US, 3_000_000, |t| tri(5.0, swing, hz, t));
            assert!(!m.is_vibrato(), "{hz}Hz swing {swing}");
        }
    }

    /// 指を止めたら、直前の山と谷の間隔の 2 倍で終わる。また揺らせば、1 往復で戻る
    #[test]
    fn vibrato_ends_when_finger_stops() {
        let mut m = Motion::INITIAL;
        m.start(5.0, 0);
        run(&mut m, LINE, PERIOD_US, 1_000_000, |t| tri(5.0, 0.6, 5.0, t));
        assert!(m.is_vibrato());
        let stop = m.raw;
        // 5Hz の山と谷の間隔は 100ms。止めてから 300ms で終わっている
        run(&mut m, LINE, 1_010_000, 300_000, |_| stop);
        assert!(!m.is_vibrato());
        run(&mut m, LINE, 1_320_000, 1_000_000, |t| tri(5.0, 0.6, 5.0, t));
        assert!(m.is_vibrato());
    }

    /// 速さが半分以下になったら終わる（5Hz → 2Hz）
    #[test]
    fn vibrato_ends_when_slow() {
        let mut m = Motion::INITIAL;
        m.start(5.0, 0);
        run(&mut m, LINE, PERIOD_US, 1_000_000, |t| tri(5.0, 0.6, 5.0, t));
        assert!(m.is_vibrato());
        // 位相をそろえるため、2Hz の波も 0 から数える（1s は 5Hz でも 2Hz でも周期の整数倍）
        run(&mut m, LINE, 1_010_000, 400_000, |t| tri(5.0, 0.6, 2.0, t));
        assert!(!m.is_vibrato());
    }

    /// リングのつなぎ目（11.9 ↔ 0.1）をまたいで揺らしても、跳ばずにビブラートになる
    #[test]
    fn vibrato_across_ring_seam() {
        let ring = Layout {
            pads: 12,
            ring: true,
        };
        let mut m = Motion::INITIAL;
        m.start(0.0, 0);
        run(&mut m, ring, PERIOD_US, 1_000_000, |t| {
            let x = tri(0.0, 0.6, 5.0, t);
            if x < 0.0 { x + 12.0 } else { x }
        });
        assert!(m.is_vibrato());
    }

    /// 動く速さ: 一定の速さ（20 電極/s）で動けば 20。止まる前の速さは、止まって MOVE_STOP_US で消える
    #[test]
    fn move_speed_from_last_zero() {
        let mut m = Motion::INITIAL;
        m.start(2.0, 0);
        assert_eq!(m.move_speed(), None);
        run(&mut m, LINE, PERIOD_US, 100_000, |t| 2.0 + t as f32 * 20.0 / 1e6);
        let v = m.move_speed().unwrap();
        assert!((v - 20.0).abs() < 0.01, "v = {v}");
        let stop = m.raw;
        run(&mut m, LINE, 110_000, 60_000, |_| stop);
        assert_eq!(m.move_speed(), None);
    }

    /// 動く速さ: 向きが変わったら取り直す（戻る前の速い動きは入らない）
    #[test]
    fn move_speed_resets_on_reverse() {
        let mut m = Motion::INITIAL;
        m.start(2.0, 0);
        // 50 電極/s で 100ms 進んでから、10 電極/s で戻る
        run(&mut m, LINE, PERIOD_US, 100_000, |t| 2.0 + t as f32 * 50.0 / 1e6);
        let top = m.raw;
        run(&mut m, LINE, 110_000, 100_000, |t| {
            top - (t - 100_000) as f32 * 10.0 / 1e6
        });
        let v = m.move_speed().unwrap();
        assert!((v - 10.0).abs() < 0.01, "v = {v}");
    }

    /// 動く速さ: MOVE_MIN_SPEED より遅い周期は、距離は足さないが時間は足す（時間を捨てると速さにならない）
    #[test]
    fn move_speed_keeps_time_of_slow_samples() {
        let mut m = Motion::INITIAL;
        m.start(2.0, 0);
        // 20 電極/s で 50ms、1 電極/s で 30ms（距離は足さない。MOVE_STOP_US より短いので取り直さない）、
        // 20 電極/s で 50ms。距離 2.0 電極 ÷ 130ms
        let mut t = 0;
        let mut x = 2.0;
        for speed in [20.0, 20.0, 20.0, 20.0, 20.0, 1.0, 1.0, 1.0, 20.0, 20.0, 20.0, 20.0, 20.0] {
            t += PERIOD_US;
            x += speed * PERIOD_US as f32 / 1e6;
            m.update(x, LINE, t);
        }
        let v = m.move_speed().unwrap();
        assert!((v - 2.0 / 0.13).abs() < 0.01, "v = {v}");
    }

    /// 動く速さ: 区間は MOVE_WINDOW_US（500ms）で打ち切る。5 電極/s で 1 秒動いてから 50 電極/s で 100ms 動くと、
    /// 全体の平均（約 9 電極/s）ではなく、最後の約 500ms の平均（約 14 電極/s）
    #[test]
    fn move_speed_window_is_500ms() {
        let mut m = Motion::INITIAL;
        m.start(2.0, 0);
        run(&mut m, LINE, PERIOD_US, 1_000_000, |t| 2.0 + t as f32 * 5.0 / 1e6);
        let x = m.raw;
        run(&mut m, LINE, 1_010_000, 100_000, |t| {
            x + (t - 1_000_000) as f32 * 50.0 / 1e6
        });
        let v = m.move_speed().unwrap();
        assert!((12.0..17.0).contains(&v), "v = {v}");
    }
}
