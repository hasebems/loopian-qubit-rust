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
//! - `TouchTracker::update` の後で `update` を呼ぶ。時間は周期の数ではなく実時間で数える（`TouchTracker::now_us`。
//!   doc/touch_analysis.md §3.8）。10ms 毎を想定しているが、間隔が延びても時間の感覚はずれない
//! - 共有状態（ファームの `TOUCH0-3`・`ANY_TOUCH` など）には書かない。呼び出し側が読んで書く
//! - 動作モード（Piano / Violin）は 2026-11-13 に無くした。今は Violin の形（電極 2 つで 1 ノート）だけ。
//!   Piano の処理は後で足す

use crate::motion::Motion;
use crate::touch::{Layout, MAX_TOUCH_POINTS, TOUCH_ON_DEPTH, TouchTracker, Track};

// =========================================================
//      Constants
// =========================================================
/// ノートを変えるヒステリシス（ノートの単位。1 ノート = 電極 ELECTRODES_PER_NOTE 個）。今のノートの中心から
/// これを越えて離れたらノートを変える。境目（0.5）から 0.1 ノート = 0.2 電極の余裕で、指を止めたときの
/// 揺れ（key5 の上で p-p 0.19 電極）では変わらない。2026-11-21 に 0.7 から下げた（ユーザー）。
/// 0.7（境目から 0.4 電極）では、隣のノートへ移る軽やかさが失われていたため
pub const HISTERESIS: f32 = 0.6;

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

// Note On の出し方（doc/touch_analysis.md §4.2「Note On の出し方」）。
// ベロシティで変えたいのは音量ではなく、音の立ち上がり。depth が 0 から NOTE_ON_DEPTH まで上がるのに
// かかった本当の時間（立ち上がりの時間）で決める。速い側は頭打ちにし、遅い側の違いを取る
/// depth がこれを超えた周期に Note On を出す。2026-11-14 に 70 → 50（70 では Note On が遅れた）
pub const NOTE_ON_DEPTH: i16 = 50;
/// NOTE_ON_DEPTH に届かずに depth が下がったとき、それまでの最大の depth がこれを超えていれば「はじいた」として
/// VELOCITY_FLICK で Note On を出す。超えていなければ（かすっただけ）、消えても何も出さない。
/// NOTE_ON_DEPTH より小さくする。2026-11-14 に 50 → 30（50 では弱いタップが鳴らなかった）
pub const FLICK_DEPTH: i16 = 30;
/// NOTE_ON_DEPTH に届かず、下がりもしないまま（軽く触れたまま）この時間（µs）が過ぎたら、VELOCITY_MIN で Note On
pub const NOTE_ON_TIMEOUT_US: u32 = 300_000;
/// ベロシティの表（2026-11-14、ユーザー）: 立ち上がりの時間が VELOCITY_FAST_US 以下で VELOCITY_MAX、
/// VELOCITY_KNEE_US で VELOCITY_KNEE、VELOCITY_MID_US で VELOCITY_MID、NOTE_ON_TIMEOUT_US 以上で VELOCITY_MIN。
/// その間は直線でつなぐ
pub const VELOCITY_MAX: u8 = 110;
/// VELOCITY_MAX になる立ち上がりの時間（µs）。補間で求まる最も短い時間の見当（仮）
pub const VELOCITY_FAST_US: u32 = 10_000;
/// 普通の押し方（立ち上がりの時間 13〜24ms 程度）が 70〜110 に散らばるよう、速い側を急にする点。
/// 前は 10ms の 110 から 100ms の 30 まで 1 本の直線で、普通の押し方が 98〜107 に固まっていた（2026-11-14）
pub const VELOCITY_KNEE: u8 = 70;
pub const VELOCITY_KNEE_US: u32 = 25_000;
pub const VELOCITY_MID: u8 = 30;
pub const VELOCITY_MID_US: u32 = 100_000;
/// 立ち上がりの時間が NOTE_ON_TIMEOUT_US 以上、または上限の時間が過ぎた（軽く触れたまま）ときのベロシティ
pub const VELOCITY_MIN: u8 = 10;
/// はじいたときのベロシティの中心（2026-11-14）。最大の depth まで上がった速さで表から求めた値を、
/// この値の方へ 1 / FLICK_SPREAD_DIV に縮める（それほど差はつけない。ユーザー）
pub const VELOCITY_FLICK: u8 = 90;
/// はじいたときに、表の値と VELOCITY_FLICK の差をこの数で割る（表の 10〜110 が 70〜95 になる）
pub const FLICK_SPREAD_DIV: i32 = 4;
/// Expression（CC11。SWAM では弓速で、主に音量を決める。doc/touch_analysis.md §4.3）は depth から作る（2026-11-14）。
/// 発音中の指のうち、ならした depth が最大のものを 1〜127 に制限して送る。変換テーブルは後で決める
pub const MIDI_CC_EXPRESSION: u8 = 11;
/// 発音中の指が無いときの Expression
pub const EXPRESSION_IDLE: u8 = 10;
/// depth をならす 1 次の LPF の係数（y += α (depth − y)）。10ms 毎の解析で時定数は約 15ms。
/// 細かいちらつきだけを取る。境目の電極（key5 など）のゆっくりした揺れ（数十〜数百 ms）は通る
const EXPRESSION_LPF_ALPHA: f32 = 0.5;
/// 前に送った値との差がこれ未満なら送らない（MIDI を送りすぎない）。端（1・127・EXPRESSION_IDLE）は必ず送る
const EXPRESSION_DEADBAND: u8 = 2;

/// 離す速さ（doc/touch_analysis.md §4.2「離す速さ」）: 発音中の depth の平均に対して、depth がこの割合（‰）を
/// 割った時刻から、RELEASE_END_PERMILLE を割った時刻までを、離す時間とする（2026-11-14、ユーザー）。
/// まだ MIDI には出さない（何に割り当てるかは後で決める）
pub const RELEASE_START_PERMILLE: i32 = 800;
pub const RELEASE_END_PERMILLE: i32 = 500;
/// 前の解析の時刻が分からない（最初の解析）・離れすぎているときに、前の解析の時刻とみなす間隔（µs）
const NOMINAL_PERIOD_US: u32 = 10_000;
/// 前の解析からこれより離れていたら、前の解析の時刻を使わない（NOMINAL_PERIOD_US とみなす）
const MAX_PERIOD_US: u32 = 50_000;
// ポルタメント（doc/touch_analysis.md §4.4。2026-11-15）
// Move（指が動いてノートが変わった）の Note On のベロシティを、動く速さ（`Motion::move_speed`。反転・止まった点と
// 500ms 前の新しい方からの、動いた距離 ÷ 時間、電極/s）で決める。SWAM は、重ねた次のノートのベロシティが低いとポルタメント、高いとレガート。
// 速く動いたら高く、ゆっくり動いたら低くする。表は SWAM で鳴らして決める（今の値は仮）
/// 動く速さがこれ以下で VELOCITY_MOVE_SLOW（電極/s。1 半音 = 2 電極を 1 秒で動く速さ。motion::MOVE_MIN_SPEED と同じ）。
/// SWAM のポルタメントがよく効くのは 1〜10 くらいの低いところ（2026-11-15、ユーザー）。
/// 2026-11-15 に 4 → 2（VELOCITY_MOVE_SLOW を 1 にしたら 1 が出やすくなったので、全体を速めにした。ユーザー）
pub const MOVE_SLOW_SPEED: f32 = 2.0;
/// 2026-11-15 に 20 → 1（ユーザー）
pub const VELOCITY_MOVE_SLOW: u8 = 1;
/// 動く速さがこれ以上で VELOCITY_MOVE_FAST（電極/s。1 半音を 100ms で動く速さ）。その間は直線。
/// 2026-11-15 に 40 → 25（速い方があまり出なかった。ユーザー。動いていない周期の時間も入れるようにして、
/// 速さが全体に低く出るようになった分もある）→ 20（全体を速めに。ユーザー）
pub const MOVE_FAST_SPEED: f32 = 20.0;
pub const VELOCITY_MOVE_FAST: u8 = 110;
/// 動く速さが分からない（MOVE_MIN_SPEED より速い周期が無かった）ときの Move のベロシティ。
/// 2026-11-13〜15 はこの値で固定していた
pub const VELOCITY_MOVE: u8 = 50;

// ビブラート（doc/touch_analysis.md §4.4。2026-11-15）。検出は `motion.rs`
pub const MIDI_CC_VIBRATO_DEPTH: u8 = 1;
pub const MIDI_CC_VIBRATO_RATE: u8 = 19;
/// Vibrato Rate（CC19）= 周波数（Hz）× この数（1.5〜8Hz が 15〜80。以前の検出と同じ。仮）
pub const VIBRATO_RATE_PER_HZ: f32 = 10.0;
/// Vibrato Depth（CC1）= 山と谷の差（電極）× この数（0.3 電極で 19、1 電極（半音の半分）で 64、2 電極で 127。仮）
pub const VIBRATO_DEPTH_PER_ELECTRODE: f32 = 64.0;


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

/// 立ち上がりの時間（µs）から、Note On のベロシティを決める（§4.2 のベロシティの表）。
/// VELOCITY_FAST_US 以下で VELOCITY_MAX、VELOCITY_KNEE_US で VELOCITY_KNEE、VELOCITY_MID_US で VELOCITY_MID、
/// NOTE_ON_TIMEOUT_US 以上で VELOCITY_MIN。その間は直線（四捨五入）
fn onset_velocity(rise_us: u32) -> u8 {
    // (t0, v0) から (t1, v1) へ下がる直線の、t での値
    let line = |t: u32, t0: u32, v0: u8, t1: u32, v1: u8| {
        let span = (v0 - v1) as u64;
        let len = (t1 - t0) as u64;
        let down = (span * (t - t0) as u64 + len / 2) / len;
        v0 - down as u8
    };
    if rise_us <= VELOCITY_FAST_US {
        VELOCITY_MAX
    } else if rise_us <= VELOCITY_KNEE_US {
        line(rise_us, VELOCITY_FAST_US, VELOCITY_MAX, VELOCITY_KNEE_US, VELOCITY_KNEE)
    } else if rise_us <= VELOCITY_MID_US {
        line(rise_us, VELOCITY_KNEE_US, VELOCITY_KNEE, VELOCITY_MID_US, VELOCITY_MID)
    } else if rise_us < NOTE_ON_TIMEOUT_US {
        line(rise_us, VELOCITY_MID_US, VELOCITY_MID, NOTE_ON_TIMEOUT_US, VELOCITY_MIN)
    } else {
        VELOCITY_MIN
    }
}

/// 2 つの周期の間で、depth が target を超えた時刻を直線で補間する（§4.2「立ち上がりの時間の求め方」）。
/// (t0, d0) が前の周期、(t1, d1) が今の周期。d0 < target <= d1 のときに使う
fn cross_time(t0: u32, d0: i16, t1: u32, d1: i16, target: i16) -> u32 {
    let dt = t1.wrapping_sub(t0) as u64;
    let rise = (d1 as i32 - d0 as i32).max(1) as u64;
    let up = (target as i32 - d0 as i32).clamp(0, rise as i32) as u64;
    t0.wrapping_add((dt * up / rise) as u32)
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

/// 指 1 本の演奏情報（状態、鳴らしているノート、ベロシティ・離す速さ・Expression を求める値）。添字は TouchPoint の id と同じ
#[derive(Clone, Copy, Debug)]
struct Voice {
    note: Option<u8>, // 鳴らしているノート（0〜ノートの数−1、NOTE_OFFSET を足す前）。None は鳴らしていない
    state: VoiceState,
    onset_start_us: u32, // 指を検出した時刻（上限の時間を数える）
    rise_start_us: u32, // depth が上がり始めた時刻（TOUCH_ON_DEPTH を超えた時刻を補間したもの）
    max_depth: i16,     // 測定中の、それまでの最大の depth
    peak_time_us: u32,  // その最大の depth を測った周期の時刻（はじいたときの速さを求める）
    prev_depth: i16,    // 測定中の、前に depth を測った周期の depth（下がったかを見る・補間に使う）
    prev_time_us: u32,  // その周期の時刻
    hold_sum: u32,                   // 発音中、離し始める前の depth の合計（平均を求める）
    hold_count: u32,                 // その周期の数
    release_start_us: Option<u32>,   // depth が平均の RELEASE_START_PERMILLE を割った時刻（補間）
    release_us: Option<u32>,         // 測った離す時間（µs）。新しい指で None に戻す
    expr_depth: f32, // 発音中の、LPF でならした depth（Expression を作る）。見失った周期は 0 に向かう
    velocity: u8,        // Note On のベロシティ（つなぎ目を越えたときの Note On も同じ値を使う）
    motion: Motion,      // 指の動き: ビブラートと、動く速さ（ポルタメント。doc/touch_analysis.md §4.4）
}

impl Voice {
    const INITIAL: Self = Voice {
        note: None,
        state: VoiceState::Idle,
        onset_start_us: 0,
        rise_start_us: 0,
        max_depth: 0,
        peak_time_us: 0,
        prev_depth: 0,
        prev_time_us: 0,
        hold_sum: 0,
        hold_count: 0,
        release_start_us: None,
        release_us: None,
        expr_depth: 0.0,
        velocity: VELOCITY_MAX,
        motion: Motion::INITIAL,
    };
    /// 新しい指: 測定中から始める。prev_us は前の解析の時刻で、そのときの depth は 0 とみなす
    /// （区間が無いか、TOUCH_ON_DEPTH 以下だった）。depth が上がり始めた時刻を、前の解析とこの周期の間で
    /// TOUCH_ON_DEPTH を超えた時刻として補間する。検出した周期を起点にすると、検出したときに depth が
    /// 大きかったものほど速く見えてしまうため（doc/touch_analysis.md §4.2「立ち上がりの時間の求め方」）
    fn start_onset(&mut self, depth: i16, prev_us: u32, now_us: u32) {
        self.state = VoiceState::Onset;
        self.onset_start_us = now_us;
        self.rise_start_us = cross_time(prev_us, 0, now_us, depth, TOUCH_ON_DEPTH);
        self.max_depth = 0;
        self.peak_time_us = now_us;
        self.hold_sum = 0;
        self.hold_count = 0;
        self.release_start_us = None;
        self.release_us = None;
        self.prev_depth = 0;
        self.prev_time_us = prev_us;
    }
    /// 測定中に、この周期に測った depth で Note On を出すか決める（doc/touch_analysis.md §4.2）。
    /// 出すならベロシティを返す
    /// - depth が NOTE_ON_DEPTH を超えた: 立ち上がりの時間（超えた時刻を補間し、上がり始めた時刻との差）で決める
    /// - NOTE_ON_TIMEOUT_US が過ぎた（軽く触れたまま）: VELOCITY_MIN
    /// - 前の周期より下がり、それまでの最大が FLICK_DEPTH を超えている（はじいた）: `flick_velocity`
    fn onset(&mut self, depth: i16, now_us: u32) -> Option<u8> {
        let (d0, t0) = (self.prev_depth, self.prev_time_us);
        let fell = depth < d0;
        self.prev_depth = depth;
        self.prev_time_us = now_us;
        if depth > self.max_depth {
            self.max_depth = depth;
            self.peak_time_us = now_us;
        }
        if depth > NOTE_ON_DEPTH {
            let crossed_us = cross_time(t0, d0, now_us, depth, NOTE_ON_DEPTH);
            Some(onset_velocity(crossed_us.wrapping_sub(self.rise_start_us)))
        } else if now_us.wrapping_sub(self.onset_start_us) >= NOTE_ON_TIMEOUT_US {
            Some(VELOCITY_MIN)
        } else if fell && self.max_depth > FLICK_DEPTH {
            Some(self.flick_velocity())
        } else {
            None
        }
    }
    /// 測定中に指を見失った周期（depth は分からないが、区間が消えたので下がったとみなす）。
    /// はじいた（それまでの最大が FLICK_DEPTH を超えている）なら、そのベロシティを返す
    fn onset_lost(&mut self) -> Option<u8> {
        (self.max_depth > FLICK_DEPTH).then(|| self.flick_velocity())
    }
    /// 発音中に、この周期の depth で離す速さを測る（doc/touch_analysis.md §4.2「離す速さ」）。
    /// 指を見失った周期（`Track::None`）は depth 0 として呼ぶ（区間が消えたので下がったとみなす）
    /// - 離し始める前: depth を平均に足す。平均の RELEASE_START_PERMILLE を割ったら、割った時刻（補間）を覚える
    /// - 離し始めた後: RELEASE_END_PERMILLE を割ったら、割った時刻（補間）との差を離す時間にする。
    ///   RELEASE_START_PERMILLE 以上に戻ったら（離しかけて戻した）、測り直す
    fn measure_release(&mut self, depth: i16, now_us: u32) {
        self.expr_depth += EXPRESSION_LPF_ALPHA * (depth.max(0) as f32 - self.expr_depth);
        let (d0, t0) = (self.prev_depth, self.prev_time_us);
        self.prev_depth = depth;
        self.prev_time_us = now_us;
        if self.hold_count == 0 {
            if depth > 0 {
                self.hold_sum = depth as u32;
                self.hold_count = 1;
            }
            return;
        }
        let avg = (self.hold_sum / self.hold_count) as i32;
        let start = (avg * RELEASE_START_PERMILLE / 1000) as i16;
        let end = (avg * RELEASE_END_PERMILLE / 1000) as i16;
        // 下がって th を割った時刻: depth の符号を反転して、上がって超えた時刻として補間する
        let fall = |th: i16| cross_time(t0, -d0, now_us, -depth, -th);
        if depth >= start {
            self.release_start_us = None;
            self.release_us = None;
            self.hold_sum = self.hold_sum.saturating_add(depth as u32);
            self.hold_count += 1;
            return;
        }
        let started = *self.release_start_us.get_or_insert_with(|| fall(start));
        if self.release_us.is_none() && depth < end {
            self.release_us = Some(fall(end).wrapping_sub(started));
        }
    }
    /// はじいたときのベロシティ（doc/touch_analysis.md §4.2「はじいたときのベロシティ」）。
    /// 最大の depth まで上がった速さのまま NOTE_ON_DEPTH まで上がったとみなした時間を立ち上がりの時間として
    /// 表から求め、VELOCITY_FLICK の方へ 1 / FLICK_SPREAD_DIV に縮める
    fn flick_velocity(&self) -> u8 {
        let rise = (self.max_depth - TOUCH_ON_DEPTH).max(1) as u64;
        let elapsed = self.peak_time_us.wrapping_sub(self.rise_start_us) as u64;
        let rise_us = (elapsed * (NOTE_ON_DEPTH - TOUCH_ON_DEPTH) as u64 / rise).min(u32::MAX as u64);
        let diff = onset_velocity(rise_us as u32) as i32 - VELOCITY_FLICK as i32;
        (VELOCITY_FLICK as i32 + diff / FLICK_SPREAD_DIV) as u8
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
    last_update_us: Option<u32>, // 前の解析の時刻（立ち上がりの時間の補間に使う）
    last_expression: Option<u8>, // 最後に送った Expression。None はまだ送っていない
    vibrato_hz: Option<f32>,     // 今のビブラートの速さ（Hz）。ビブラートでなければ None
    last_vibrato: Option<(u8, u8)>, // 最後に送った (Vibrato Depth, Vibrato Rate)。None はまだ送っていない
}

impl<F> PlayInfo<F>
where
    F: Fn(u8, u8, u8, f32),
{
    pub fn new(cb: F) -> Self {
        PlayInfo {
            midi_callback: cb,
            voices: [Voice::INITIAL; MAX_TOUCH_POINTS],
            last_update_us: None,
            last_expression: None,
            vibrato_hz: None,
            last_vibrato: None,
        }
    }
    /// 今のビブラートの速さ（Hz。OLED の表示用）。ビブラートと判定している発音中の指が無ければ None
    pub fn vibrato_hz(&self) -> Option<f32> {
        self.vibrato_hz
    }
    /// 指 i（TouchPoint の id）について、最後に測った離す時間（µs。doc/touch_analysis.md §4.2「離す速さ」）。
    /// 新しい指で None に戻る。Note Off の後も、次の指まで残る
    pub fn release_us(&self, i: usize) -> Option<u32> {
        self.voices.get(i).and_then(|v| v.release_us)
    }

    /// 1 周期分。`TouchTracker::update` の後に呼ぶ。指毎の `Track` で状態を進めて Note On/Off を出し、
    /// 続けて Expression とビブラートを送る
    pub fn update<P>(&mut self, touch: &TouchTracker<P>)
    where
        P: AsRef<[u16]> + AsMut<[u16]>,
    {
        let layout = touch.layout();
        let now_us = touch.now_us();
        // 前の解析の時刻。最初の解析と、前の解析から離れすぎているときは、NOMINAL_PERIOD_US 前とみなす
        let prev_us = match self.last_update_us {
            Some(t) if (1..=MAX_PERIOD_US).contains(&now_us.wrapping_sub(t)) => t,
            _ => now_us.wrapping_sub(NOMINAL_PERIOD_US),
        };
        self.last_update_us = Some(now_us);
        if note_slots(layout) == 0 {
            return;
        }
        self.perform_notes(touch, layout, prev_us);
        self.send_expression(false);
        self.send_vibrato(touch, layout);
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
    /// | 発音中 | Followed | ノートが変わったら Move（つなぎ目では cross_seam）。ビブラートの間は変えない |
    ///
    /// 指の動き（`Motion`）は、指を見つけた周期（Started・Followed）に更新する。保持している周期は終わりだけ見る
    /// | 発音中 | Released | Note Off → 待機 |
    fn perform_notes<P>(&mut self, touch: &TouchTracker<P>, layout: Layout, prev_us: u32)
    where
        P: AsRef<[u16]> + AsMut<[u16]>,
    {
        let tps = touch.touch_points();
        let now_us = touch.now_us();
        for (i, tp) in tps.iter().enumerate() {
            let location = tp.get_location();
            let voice = &mut self.voices[i];
            let velocity = match (tp.track(), voice.state) {
                (Track::Started, _) => {
                    voice.motion.start(location, now_us);
                    voice.start_onset(tp.get_intensity(), prev_us, now_us);
                    voice.onset(tp.get_intensity(), now_us)
                }
                (Track::Followed, VoiceState::Onset) => {
                    voice.motion.update(location, layout, now_us);
                    voice.onset(tp.get_intensity(), now_us)
                }
                (Track::Followed, VoiceState::Sounding) => {
                    voice.motion.update(location, layout, now_us);
                    voice.measure_release(tp.get_intensity(), now_us);
                    self.note_follow(i, location, layout);
                    None
                }
                (Track::None, VoiceState::Onset) if tp.is_touched() => voice.onset_lost(),
                (Track::None, VoiceState::Sounding) if tp.is_touched() => {
                    voice.motion.hold(now_us);
                    voice.measure_release(0, now_us);
                    None
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
        voice.expr_depth = voice.prev_depth.max(0) as f32;
        // SWAM は Note On の時点の Expression でアタックを決めることがあるので、Note On の前に送る
        self.send_expression(true);
        if let Some(note) = note {
            self.send_note_on(i, note.saturating_add(NOTE_OFFSET), velocity, location);
        }
    }
    /// 同じ指が続いている: ノートが変わったら、Move かつなぎ目を越えたかで分ける。
    /// ビブラートと判定している間は、ノートを変えない（doc/touch_analysis.md §4.4）。終わったら、
    /// 次の周期から今の position でまた判定する（ずれていれば Move）
    fn note_follow(&mut self, i: usize, location: f32, layout: Layout) {
        if self.voices[i].motion.is_vibrato() {
            return;
        }
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
    /// Move: 次のノートの Note On を出して、前のノートの Note Off（MOVED）を出す。音が重なるので、SWAM では
    /// レガートかポルタメントになる。ベロシティは動く速さで決める（`move_velocity`。doc/touch_analysis.md §4.4）。
    /// 前のノートの Note Off のベロシティも、Note On と同じ値にする（2026-11-15、ユーザー）。SWAM が、Move の
    /// ベロシティを 64（前は Note Off を 0x40 で出していた）と表示し、ポルタメントもかからなかったため。
    /// SWAM が後に来た Note Off のベロシティを見ているらしい（Note Off Velocity の設定を OFF にしても同じ）
    fn move_note(&self, i: usize, from: u8, to: u8, location: f32) {
        let velocity = move_velocity(self.voices[i].motion.move_speed());
        self.send_note_on(i, to, velocity, location);
        (self.midi_callback)(STATUS_NOTE_MOVED | i as u8, from, velocity, location);
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
    /// 今の Expression（doc/touch_analysis.md §4.3）: 発音中の指のうち、ならした depth が最大のものを
    /// 1〜127 に制限する。発音中の指が無ければ EXPRESSION_IDLE
    fn expression(&self) -> u8 {
        self.voices
            .iter()
            .filter(|v| v.state == VoiceState::Sounding)
            .map(|v| v.expr_depth)
            .fold(None, |m: Option<f32>, d| Some(m.map_or(d, |m| m.max(d))))
            .map_or(EXPRESSION_IDLE, |d| (d + 0.5).clamp(1.0, 127.0) as u8)
    }
    /// Expression が変わったら CC11 を送る。前に送った値との差が EXPRESSION_DEADBAND 未満なら送らない。
    /// ただし端の値（1・127・EXPRESSION_IDLE）に変わったときと、force（Note On の直前）のときは、変わっていれば送る
    fn send_expression(&mut self, force: bool) {
        let value = self.expression();
        let send = match self.last_expression {
            None => true,
            Some(last) if last == value => false,
            Some(last) => {
                force
                    || last.abs_diff(value) >= EXPRESSION_DEADBAND
                    || matches!(value, 1 | 127 | EXPRESSION_IDLE)
            }
        };
        if send {
            (self.midi_callback)(STATUS_CC, MIDI_CC_EXPRESSION, value, 0.0);
            self.last_expression = Some(value);
        }
    }
    fn send_note_on(&self, i: usize, midi_note: u8, velocity: u8, location: f32) {
        (self.midi_callback)(STATUS_NOTE_ON | i as u8, midi_note, velocity, location);
    }
    /// ビブラート（doc/touch_analysis.md §4.4）: 発音中でビブラートと判定している指のうち、ならした depth が
    /// 最大のものの速さと深さを、Vibrato Rate（CC19）・Vibrato Depth（CC1）にして、変わったら送る（Rate が先）。
    /// そういう指がいなくなったら、Depth を 0 にする。
    /// その指がリングのつなぎ目の上（position が電極数 − 1 以上）にいる間は送らない（§4.1）
    fn send_vibrato<P>(&mut self, touch: &TouchTracker<P>, layout: Layout)
    where
        P: AsRef<[u16]> + AsMut<[u16]>,
    {
        let seam = (layout.pads - 1) as f32;
        let strongest = self
            .voices
            .iter()
            .zip(touch.touch_points().iter())
            .filter(|(v, _)| v.state == VoiceState::Sounding)
            .filter_map(|(v, tp)| v.motion.vibrato().map(|vib| (v.expr_depth, vib, tp.get_location())))
            .fold(None, |m: Option<(f32, (f32, f32), f32)>, x| match m {
                Some(m) if m.0 >= x.0 => Some(m),
                _ => Some(x),
            });
        let Some((_, (hz, swing), location)) = strongest else {
            self.vibrato_hz = None;
            if let Some((depth, rate)) = self.last_vibrato
                && depth != 0
            {
                (self.midi_callback)(STATUS_CC, MIDI_CC_VIBRATO_DEPTH, 0, 0.0);
                self.last_vibrato = Some((0, rate));
            }
            return;
        };
        self.vibrato_hz = Some(hz);
        if layout.ring && location >= seam {
            return;
        }
        let rate = (hz * VIBRATO_RATE_PER_HZ + 0.5).clamp(1.0, 127.0) as u8;
        let depth = (swing * VIBRATO_DEPTH_PER_ELECTRODE + 0.5).clamp(1.0, 127.0) as u8;
        let last = self.last_vibrato;
        if last.map(|l| l.1) != Some(rate) {
            (self.midi_callback)(STATUS_CC, MIDI_CC_VIBRATO_RATE, rate, 0.0);
        }
        if last.map(|l| l.0) != Some(depth) {
            (self.midi_callback)(STATUS_CC, MIDI_CC_VIBRATO_DEPTH, depth, 0.0);
        }
        self.last_vibrato = Some((depth, rate));
    }
}

/// Move のベロシティ（doc/touch_analysis.md §4.4）: 動く速さ（電極/s）が MOVE_SLOW_SPEED 以下で
/// VELOCITY_MOVE_SLOW、MOVE_FAST_SPEED 以上で VELOCITY_MOVE_FAST、その間は直線（四捨五入）。
/// 速さが分からなければ VELOCITY_MOVE
fn move_velocity(speed: Option<f32>) -> u8 {
    let Some(speed) = speed else {
        return VELOCITY_MOVE;
    };
    let r = ((speed - MOVE_SLOW_SPEED) / (MOVE_FAST_SPEED - MOVE_SLOW_SPEED)).clamp(0.0, 1.0);
    let span = (VELOCITY_MOVE_FAST - VELOCITY_MOVE_SLOW) as f32;
    (VELOCITY_MOVE_SLOW as f32 + span * r + 0.5) as u8
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use std::vec::Vec;

    type Log = RefCell<Vec<(u8, u8, u8)>>; // (status, note, velocity)

    /// テストの解析の間隔（10ms）
    const PERIOD_US: u32 = 10_000;

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
        tracker.update(tracker.now_us() + PERIOD_US);
        play.update(tracker);
    }

    /// Note のイベントと、CC11（Expression）以外の CC を記録する。Expression は `expression_logger` で見る
    fn logger(log: &Log) -> impl Fn(u8, u8, u8, f32) + '_ {
        |status, note, velocity, _location| {
            if !(status == STATUS_CC && note == MIDI_CC_EXPRESSION) {
                log.borrow_mut().push((status, note, velocity))
            }
        }
    }

    /// すべてのイベント（CC11 を含む）を記録する
    fn expression_logger(log: &Log) -> impl Fn(u8, u8, u8, f32) + '_ {
        |status, note, velocity, _location| log.borrow_mut().push((status, note, velocity))
    }

    /// 電極 c を中心にした指（depth 100。NOTE_ON_DEPTH を超える）
    fn finger(c: usize, pads: usize) -> [(usize, u16); 3] {
        [((c + pads - 1) % pads, 20), (c, 60), ((c + 1) % pads, 20)]
    }

    /// 速く押す: depth が初めから NOTE_ON_DEPTH を超えていれば、検出した周期に VELOCITY_MAX で Note On。
    /// 電極 4 は 4 / 2 = ノート 2。離すと Note Off が 1 回だけ出る
    #[test]
    fn fast_press_note_on_off() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        step(&mut tracker, &mut play, &finger(4, 12));
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_MAX)]
        );
        for _ in 0..10 {
            step(&mut tracker, &mut play, &[]);
        }
        assert_eq!(
            log.borrow().as_slice(),
            &[
                (STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_MAX),
                (STATUS_NOTE_OFF, 2 + NOTE_OFFSET, 0x40)
            ]
        );
    }

    /// ゆっくり押す: NOTE_ON_DEPTH を超えた周期に、立ち上がりの時間のベロシティで Note On。
    /// 前の解析（depth 0）が 0ms、検出が 10ms（depth 20）なので、上がり始めは 5ms（TOUCH_ON_DEPTH = 10 を超えた時刻）。
    /// 40ms で 50（超えていない）、50ms で 60 なので、50 を超えたのは 40ms。立ち上がりの時間は 35ms
    #[test]
    fn slow_press_velocity_by_rise_time() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for v in [20, 30, 40, 50] {
            step(&mut tracker, &mut play, &[(4, v)]);
        }
        assert!(log.borrow().is_empty());
        step(&mut tracker, &mut play, &[(4, 60)]);
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, onset_velocity(35_000))]
        );
        assert_eq!(onset_velocity(35_000), 65);
    }

    /// 軽く触れたまま: NOTE_ON_DEPTH に届かず下がりもしなければ、NOTE_ON_TIMEOUT_US で VELOCITY_MIN
    #[test]
    fn light_touch_times_out_slow() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for _ in 0..NOTE_ON_TIMEOUT_US / PERIOD_US {
            step(&mut tracker, &mut play, &[(4, 30)]);
        }
        assert!(log.borrow().is_empty());
        step(&mut tracker, &mut play, &[(4, 30)]);
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_MIN)]
        );
    }

    /// はじく: NOTE_ON_DEPTH に届かずに下がっても、最大が FLICK_DEPTH を超えていれば、
    /// 下がった周期に Note On。ベロシティは最大の depth まで上がった速さで決め、VELOCITY_FLICK の方へ縮める
    #[test]
    fn flick_sounds() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for v in [20, 35, 45] {
            step(&mut tracker, &mut play, &[(4, v)]);
        }
        assert!(log.borrow().is_empty());
        step(&mut tracker, &mut play, &[(4, 35)]);
        // 上がり始め 5ms、最大 45 は 30ms: 25ms で 35 上がったので、50 までの 40 は約 28.6ms。
        // 表では 68 で、90 との差 −22 の 1/4（−5）だけ下げて 85
        assert_eq!(onset_velocity(28_571), 68);
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, 85)]
        );
        // 区間が消えたら、そのまま見失った周期にも Note On を出す（下がったとみなす）
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for v in [20, 45] {
            step(&mut tracker, &mut play, &[(4, v)]);
        }
        step(&mut tracker, &mut play, &[]);
        // 上がり始め 5ms、最大 45 は 20ms: 15ms で 35 なので、50 までは約 17.1ms。表では 91 で、91 に縮めても 90
        assert_eq!(onset_velocity(17_142), 91);
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, 90)]
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
        for v in [15, 25, 20] {
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
            &[(STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_MAX)]
        );
    }

    /// 指を動かしてノートが変わると、新しいノートの Note On と古いノートの MOVED が出る。
    /// 1 周期で 2 電極（200 電極/s）動いたので、ベロシティは VELOCITY_MOVE_FAST。MOVED も同じベロシティ
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
                (STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_MAX),
                (STATUS_NOTE_ON, 3 + NOTE_OFFSET, VELOCITY_MOVE_FAST),
                (STATUS_NOTE_MOVED, 2 + NOTE_OFFSET, VELOCITY_MOVE_FAST)
            ]
        );
    }

    /// ベロシティの表: 10ms 以下で 110、25ms で 70、100ms で 30、300ms 以上で 10。その間は直線
    #[test]
    fn onset_velocity_table() {
        assert_eq!(onset_velocity(0), VELOCITY_MAX);
        assert_eq!(onset_velocity(VELOCITY_FAST_US), VELOCITY_MAX);
        assert_eq!(onset_velocity(17_500), 90);
        assert_eq!(onset_velocity(VELOCITY_KNEE_US), VELOCITY_KNEE);
        assert_eq!(onset_velocity(55_000), 54);
        assert_eq!(onset_velocity(VELOCITY_MID_US), VELOCITY_MID);
        assert_eq!(onset_velocity(200_000), 20);
        assert_eq!(onset_velocity(NOTE_ON_TIMEOUT_US), VELOCITY_MIN);
        assert_eq!(onset_velocity(u32::MAX), VELOCITY_MIN);
    }

    /// 補間: 2 つの周期の間で、depth が target を超えた時刻
    #[test]
    fn cross_time_interpolates() {
        assert_eq!(cross_time(0, 0, 10_000, 20, 10), 5_000);
        assert_eq!(cross_time(10_000, 40, 20_000, 60, 50), 15_000);
        // 時刻が一周しても求まる
        assert_eq!(cross_time(u32::MAX - 4_999, 0, 5_000, 20, 10), 0);
    }

    /// 解析の間隔が 10ms でなくても、また検出した周期の depth が違っても、同じ速さで押せば同じベロシティ
    /// （§3.8・§4.2 の補間）。depth が 0 から 2/ms で上がると、10 を超えるのは 5ms、50 を超えるのは 25ms 後で、
    /// 立ち上がりの時間は 20ms。10・12・15ms 毎に呼ぶと、検出した周期の depth は 20・24・30 になる
    #[test]
    fn velocity_follows_real_time() {
        fn run(period_us: u32) -> u8 {
            let log: Log = RefCell::new(Vec::new());
            let mut tracker = TouchTracker::new([0u16; 12], false);
            let mut play = PlayInfo::new(logger(&log));
            let mut now = 0;
            let mut put = |v: u16| {
                tracker.set_value(4, v);
                now += period_us;
                tracker.update(now);
                play.update(&tracker);
            };
            put(0); // 押し始め（depth 0）
            for k in 1..=10u32 {
                put((2 * k * period_us / 1000) as u16);
                if !log.borrow().is_empty() {
                    break;
                }
            }
            let v = log.borrow()[0].2;
            v
        }
        assert_eq!(onset_velocity(20_000), 83);
        assert_eq!(run(10_000), 83);
        assert_eq!(run(12_000), 83);
        assert_eq!(run(15_000), 83);
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
                (STATUS_NOTE_ON, 5 + NOTE_OFFSET, VELOCITY_MAX),
                (STATUS_NOTE_OFF, 5 + NOTE_OFFSET, 0x40),
                (STATUS_NOTE_ON, NOTE_OFFSET, VELOCITY_MAX),
            ]
        );
        // つなぎ目を越えないノートの変化は Move のまま
        step(&mut tracker, &mut play, &finger(2, 12));
        assert_eq!(
            log.borrow()[3..],
            [
                (STATUS_NOTE_ON, 1 + NOTE_OFFSET, VELOCITY_MOVE_FAST),
                (STATUS_NOTE_MOVED, NOTE_OFFSET, VELOCITY_MOVE_FAST)
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

    /// 離す速さ: 発音中の平均（100）の 80% を割った時刻から 50% を割った時刻までを、補間して測る。
    /// 100 → 75 の間で 80 を割るのは 8ms 後、次の 75 → 25 の間で 50 を割るのは 5ms 後なので、離す時間は 7ms。
    /// 80% 以上の depth は平均に入る（離し始める前の値として）
    #[test]
    fn release_time_is_measured() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for v in [100, 100, 100, 100, 75] {
            step(&mut tracker, &mut play, &[(4, v)]);
        }
        assert_eq!(play.release_us(0), None);
        step(&mut tracker, &mut play, &[(4, 25)]);
        assert_eq!(play.release_us(0), Some(7_000));
        // 離れた後も、次の指まで残る
        for _ in 0..5 {
            step(&mut tracker, &mut play, &[]);
        }
        assert_eq!(play.release_us(0), Some(7_000));
        step(&mut tracker, &mut play, &[(4, 100)]);
        assert_eq!(play.release_us(0), None);
    }

    /// 50% を割る前に指を見失ったら、その周期の depth を 0 として補間する。
    /// 100 → 0 の間で、80 を割るのは 2ms 後、50 を割るのは 5ms 後なので、離す時間は 3ms
    #[test]
    fn release_time_when_lost() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for v in [100, 100, 100, 100] {
            step(&mut tracker, &mut play, &[(4, v)]);
        }
        step(&mut tracker, &mut play, &[]);
        assert_eq!(play.release_us(0), Some(3_000));
    }

    /// 離しかけて戻したら（80% を割った後、80% 以上に戻った）、測り直す
    #[test]
    fn release_restarts_when_pressed_again() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        for v in [100, 100, 100, 70, 100, 100, 40] {
            step(&mut tracker, &mut play, &[(4, v)]);
            if v == 100 {
                assert_eq!(play.voices[0].release_start_us, None);
            }
        }
        // 70 の後に 100 に戻ったので、最後の 100 → 40 の 1 周期で 80% と 50% を割った
        // （平均は 100: 80 を割るのは 3.3ms 後、50 を割るのは 8.3ms 後で、離す時間は 5ms）
        assert_eq!(play.release_us(0), Some(8_333 - 3_333));
        assert_eq!(log.borrow().len(), 1); // Note On だけ（音は切れていない）
    }

    /// Expression: 最初の解析で EXPRESSION_IDLE を送る。Note On の直前に、その周期の depth（1〜127）を送る。
    /// 発音中は LPF でならした depth を送り、離れたら EXPRESSION_IDLE に戻す
    #[test]
    fn expression_follows_depth() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(expression_logger(&log));
        step(&mut tracker, &mut play, &[]);
        assert_eq!(
            log.borrow().as_slice(),
            &[(STATUS_CC, MIDI_CC_EXPRESSION, EXPRESSION_IDLE)]
        );
        log.borrow_mut().clear();
        step(&mut tracker, &mut play, &[(4, 80)]);
        // CC11 が Note On より先
        assert_eq!(
            log.borrow().as_slice(),
            &[
                (STATUS_CC, MIDI_CC_EXPRESSION, 80),
                (STATUS_NOTE_ON, 2 + NOTE_OFFSET, VELOCITY_MAX)
            ]
        );
        log.borrow_mut().clear();
        // 80 → 100: LPF（α 0.5）で 90、95 … と近づく
        step(&mut tracker, &mut play, &[(4, 100)]);
        step(&mut tracker, &mut play, &[(4, 100)]);
        assert_eq!(
            log.borrow().as_slice(),
            &[
                (STATUS_CC, MIDI_CC_EXPRESSION, 90),
                (STATUS_CC, MIDI_CC_EXPRESSION, 95)
            ]
        );
        // 1 未満にはならず、127 を超えない
        log.borrow_mut().clear();
        for _ in 0..10 {
            step(&mut tracker, &mut play, &[(4, 300)]);
        }
        assert_eq!(log.borrow().last(), Some(&(STATUS_CC, MIDI_CC_EXPRESSION, 127)));
        // 離すと 0 に向かい、Note Off の後に EXPRESSION_IDLE
        log.borrow_mut().clear();
        for _ in 0..10 {
            step(&mut tracker, &mut play, &[]);
        }
        let events = log.borrow();
        let off = events.iter().position(|e| e.0 == STATUS_NOTE_OFF).unwrap();
        assert!(events[..off].iter().all(|e| e.0 == STATUS_CC && e.2 < 127));
        assert_eq!(events.last(), Some(&(STATUS_CC, MIDI_CC_EXPRESSION, EXPRESSION_IDLE)));
    }

    /// Expression は、発音中の指のうち depth が最大のもの
    #[test]
    fn expression_uses_strongest_finger() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 24], false);
        let mut play = PlayInfo::new(expression_logger(&log));
        for _ in 0..5 {
            step(&mut tracker, &mut play, &[(2, 60), (16, 110)]);
        }
        assert_eq!(play.expression(), 110);
        for _ in 0..10 {
            step(&mut tracker, &mut play, &[(2, 60)]);
        }
        assert_eq!(play.expression(), 60);
    }

    /// position x（小数）の指（電極 12 個）: x の前後の電極に、x からの距離で減る output を置く
    /// （重心がほぼ x になる）。電極の番号は 12 で巡回させる（リングのつなぎ目の上）
    fn blob(x: f32) -> Vec<(usize, u16)> {
        let c = x as usize + 12;
        (c - 2..=c + 3)
            .filter_map(|k| {
                let w = 1.0 - (k as f32 - 12.0 - x).abs() / 2.0;
                (w > 0.0).then(|| (k % 12, (80.0 * w) as u16))
            })
            .collect()
    }

    /// 三角波（中心 center、山と谷の差 swing、周波数 hz）の、t（µs）での値
    fn tri(center: f32, swing: f32, hz: f32, t_us: u32) -> f32 {
        let phase = (t_us as f32 / 1_000_000.0 * hz) % 1.0;
        let x = if phase < 0.5 { phase * 4.0 - 1.0 } else { 3.0 - phase * 4.0 };
        center + swing / 2.0 * x
    }

    /// ゆっくり（5 電極/s）動かしてノートが変わると、Move のベロシティは低い（ポルタメント寄り）
    #[test]
    fn slow_move_has_low_velocity() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        let mut x = 4.0;
        for _ in 0..60 {
            step(&mut tracker, &mut play, &blob(x));
            x += 5.0 * PERIOD_US as f32 / 1e6;
        }
        let moves: Vec<u8> = log
            .borrow()
            .iter()
            .filter(|e| e.0 == STATUS_NOTE_ON)
            .skip(1)
            .map(|e| e.2)
            .collect();
        assert!(!moves.is_empty());
        assert!(moves.iter().all(|&v| v < 40), "{moves:?}");
        let log = log.borrow();
        for w in log.windows(2).filter(|w| w[1].0 == STATUS_NOTE_MOVED) {
            assert_eq!(w[0].0, STATUS_NOTE_ON);
            assert_eq!(w[1].2, w[0].2, "MOVED のベロシティは直前の Note On と同じ");
        }
    }

    /// Move のベロシティの表
    #[test]
    fn move_velocity_table() {
        assert_eq!(move_velocity(None), VELOCITY_MOVE);
        assert_eq!(move_velocity(Some(0.0)), VELOCITY_MOVE_SLOW);
        assert_eq!(move_velocity(Some(MOVE_SLOW_SPEED)), VELOCITY_MOVE_SLOW);
        assert_eq!(move_velocity(Some(11.0)), 56);
        assert_eq!(move_velocity(Some(MOVE_FAST_SPEED)), VELOCITY_MOVE_FAST);
        assert_eq!(move_velocity(Some(1000.0)), VELOCITY_MOVE_FAST);
    }

    /// ビブラート: 大きく（ヒステリシス（1.2 電極）を越える ±1.6 電極）揺らしても、ビブラートと判定した後はノートを変えず、
    /// CC19（Rate）→ CC1（Depth）を送る。指を止めると終わって Depth 0 を送る
    #[test]
    fn vibrato_holds_note_and_sends_cc() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], false);
        let mut play = PlayInfo::new(logger(&log));
        let mut t = 0;
        for _ in 0..100 {
            step(&mut tracker, &mut play, &blob(tri(4.0, 3.2, 5.0, t)));
            t += PERIOD_US;
        }
        let hz = play.vibrato_hz().expect("ビブラート");
        assert!((hz - 5.0).abs() < 0.6, "hz = {hz}");
        let events = log.borrow().clone();
        let first_rate = events
            .iter()
            .position(|e| *e == (STATUS_CC, MIDI_CC_VIBRATO_RATE, e.2))
            .expect("CC19");
        assert_eq!(events[first_rate + 1].0, STATUS_CC);
        assert_eq!(events[first_rate + 1].1, MIDI_CC_VIBRATO_DEPTH);
        assert!(
            events[first_rate..].iter().all(|e| e.0 == STATUS_CC),
            "ビブラートの間にノートが変わった: {events:?}"
        );
        // 止める: 山と谷の間隔（100ms）の 2 倍を過ぎたら終わり、Depth 0
        for _ in 0..30 {
            step(&mut tracker, &mut play, &blob(4.0));
        }
        assert_eq!(play.vibrato_hz(), None);
        assert!(log.borrow().contains(&(STATUS_CC, MIDI_CC_VIBRATO_DEPTH, 0)));
    }

    /// リングのつなぎ目の上（position が電極数 − 1 以上）でのビブラートは、CC を送らない（§4.1）
    #[test]
    fn vibrato_is_held_on_seam() {
        let log: Log = RefCell::new(Vec::new());
        let mut tracker = TouchTracker::new([0u16; 12], true);
        let mut play = PlayInfo::new(logger(&log));
        let mut t = 0;
        for _ in 0..100 {
            // 11.2〜11.8 で揺らす（いつもつなぎ目の上）
            step(&mut tracker, &mut play, &blob(tri(11.5, 0.6, 5.0, t)));
            t += PERIOD_US;
        }
        assert!(play.vibrato_hz().is_some());
        assert!(!log.borrow().iter().any(|e| e.0 == STATUS_CC));
    }
}
