//  Created by Hasebe Masahiko on 2026/02/11.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! タスク間の共有状態
//!
//! 各項目のコメントに「書き手 → 読み手」を記す。基本は Atomic (Ordering::Relaxed) で、
//! 各タスクは共有状態をポーリングして独立周期で動く。
use core::sync::atomic::AtomicBool;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use portable_atomic::{AtomicI32, AtomicU8, AtomicU16, AtomicU32, Ordering};

use crate::constants::*;

/// USB MIDI のパケット（CIN, status, data1, data2）
pub type MidiPacket = [u8; 4];
pub const MIDI_TX_QUEUE_SIZE: usize = 16;
// 送信する MIDI パケット: touch_task (Core1, ノート), pressure_task (Core0, CC) → midi_tx_task (Core0)
// 送る側は try_send で待たない。USB が詰まっても送る側のタスクは止まらない
pub static MIDI_TX: Channel<CriticalSectionRawMutex, MidiPacket, MIDI_TX_QUEUE_SIZE> =
    Channel::new();

// 表示用変数: read_touch → OLED (page 2)
pub static POINT0: AtomicU16 = AtomicU16::new(0);
pub static POINT1: AtomicU16 = AtomicU16::new(0);
pub static POINT2: AtomicU16 = AtomicU16::new(0);
pub static POINT3: AtomicU16 = AtomicU16::new(0);
pub static POINT4: AtomicU16 = AtomicU16::new(0);
pub static POINT5: AtomicU16 = AtomicU16::new(0);
// タッチ位置 ×100（0–9999）、10000 は未タッチ: touch_task (qtouch) → ringled_task, OLED (page 3)
pub static TOUCH0: AtomicI32 = AtomicI32::new(10000);
pub static TOUCH1: AtomicI32 = AtomicI32::new(10000);
pub static TOUCH2: AtomicI32 = AtomicI32::new(10000);
pub static TOUCH3: AtomicI32 = AtomicI32::new(10000);
// 受信Note On/Off状態(1bit/LED)。LED n は [n / 32] の (n % 32) ビット目
// midi_rx_task, ui_task(全消去) → ringled_task
pub static RINGLED_RX_BITS: [AtomicU32; RINGLED_RX_WORDS] =
    [const { AtomicU32::new(0) }; RINGLED_RX_WORDS];
// ADCの値: pressure_task → OLED (page 1)
pub static AD_VALUE0: AtomicU32 = AtomicU32::new(0); // ADCの値(A0)
pub static AD_VALUE1: AtomicU32 = AtomicU32::new(0); // ADCの値(A1)
pub static AD_VALUE2: AtomicU32 = AtomicU32::new(0); // ADCの値(B0)
pub static AD_VALUE3: AtomicU32 = AtomicU32::new(0); // ADCの値(B1)
pub static PRESSURE: AtomicU32 = AtomicU32::new(0); // 圧力計算結果: pressure_task → CC11送信, OLED (page 1)
// 動作モード（Piano/Violin）: ui_task → touch_task, pressure_task, midi_rx_task, OLED
pub static WORK_MODE: AtomicU8 = AtomicU8::new(0);
// 設定画面にいる（タッチ・圧力の基準値を補正中。センサーに触れない前提）
// ui_task → touch_task (read_touch), pressure_task, ringled
pub static SETTING_MODE: AtomicBool = AtomicBool::new(false);
pub static DEBUG_VALUE: AtomicU32 = AtomicU32::new(0); // デバッグ用: touch_task (qtouch のビブラート) → OLED (page 1)
pub static ANY_TOUCH: AtomicBool = AtomicBool::new(false); // touch_task (qtouch) → pressure_task

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      診断用: 各タスク → OLED の診断ページ (page 5)
//      最小・最大と回数は、設定画面に入ったときに reset_diagnostics() でリセットする
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
/// 処理時間（us）の最小・平均・最大。書き手は 1 つのタスクに限る
pub struct TimeStat {
    min: AtomicU32,
    avg: AtomicU32, // 指数移動平均（1/16）
    max: AtomicU32,
}

impl TimeStat {
    pub const fn new() -> Self {
        Self {
            min: AtomicU32::new(u32::MAX),
            avg: AtomicU32::new(0),
            max: AtomicU32::new(0),
        }
    }

    pub fn record(&self, us: u32) {
        self.min.fetch_min(us, Ordering::Relaxed);
        self.max.fetch_max(us, Ordering::Relaxed);
        let avg = self.avg.load(Ordering::Relaxed);
        let avg = if avg == 0 {
            us
        } else {
            avg - avg / 16 + us / 16
        };
        self.avg.store(avg, Ordering::Relaxed);
    }

    /// (最小, 平均, 最大)。まだ記録が無いときの最小は 0
    pub fn get(&self) -> (u32, u32, u32) {
        let min = self.min.load(Ordering::Relaxed);
        (
            if min == u32::MAX { 0 } else { min },
            self.avg.load(Ordering::Relaxed),
            self.max.load(Ordering::Relaxed),
        )
    }

    fn reset(&self) {
        self.min.store(u32::MAX, Ordering::Relaxed);
        self.max.store(0, Ordering::Relaxed);
    }
}

pub static SCAN_TIME: TimeStat = TimeStat::new(); // タッチのスキャン時間: touch_task
pub static ANALYSIS_TIME: TimeStat = TimeStat::new(); // QubitTouch の解析時間: touch_task
pub static UI_DRAW_TIME: TimeStat = TimeStat::new(); // OLED の描画で 1 回に Core0 を止めた最長の時間（転送を除く）: ui_task
// スキャン＋解析が周期 (scan_period_us()) を超えた回数: touch_task
pub static PERIOD_OVERRUN: AtomicU32 = AtomicU32::new(0);
// MIDI_TX の最大使用数とあふれた回数: queue_midi (touch_task, pressure_task)
pub static MIDI_TX_MAX_USED: AtomicU32 = AtomicU32::new(0);
pub static MIDI_TX_OVERFLOW: AtomicU32 = AtomicU32::new(0);

/// 診断用の最小・最大と回数をリセットする
pub fn reset_diagnostics() {
    SCAN_TIME.reset();
    ANALYSIS_TIME.reset();
    UI_DRAW_TIME.reset();
    PERIOD_OVERRUN.store(0, Ordering::Relaxed);
    MIDI_TX_MAX_USED.store(0, Ordering::Relaxed);
    MIDI_TX_OVERFLOW.store(0, Ordering::Relaxed);
}

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      デバッグ用: touch_task (Core1) → debug_stream_task (Core0) → USB CDC → PC
//      doc/debug_env.md
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
/// 1 回のスキャンで読んだ生値（hi/lo ずれ補正の前）
#[cfg(feature = "debug_stream")]
pub struct DebugFrame {
    pub seq: u32, // スキャン番号（送らなかったフレームも数える。PC は欠けで取りこぼしを知る）
    pub time_us: u32, // スキャン開始時刻（起動からの µs。約 71 分で一周）
    pub valid: u128, // 読み取り成功フラグ（1 bit / キー。96 キーまで）
    pub raw: [u16; TOTAL_QT_KEYS],
}
#[cfg(feature = "debug_stream")]
pub const DEBUG_FRAMES_QUEUE_SIZE: usize = 16;
// スキャン毎の生値: touch_task (Core1) → debug_stream_task (Core0)
#[cfg(feature = "debug_stream")]
pub static DEBUG_FRAMES: Channel<CriticalSectionRawMutex, DebugFrame, DEBUG_FRAMES_QUEUE_SIZE> =
    Channel::new();
// PC がポートを開いて start を送った後か: debug_stream_task → touch_task
// false の間、touch_task は DEBUG_FRAMES に入れない（キューが無駄にあふれないようにするため）
#[cfg(feature = "debug_stream")]
pub static DEBUG_STREAMING: AtomicBool = AtomicBool::new(false);
// DEBUG_FRAMES / DEBUG_EVENTS があふれて捨てた数の累計: touch_task → debug_stream_task (INFO)
#[cfg(feature = "debug_stream")]
pub static DEBUG_DROPPED: AtomicU32 = AtomicU32::new(0);

/// タッチ信号以外のできごと（Note On/Off など）。kind と data の意味は doc/debug_env.md §5.1
#[cfg(feature = "debug_stream")]
pub struct DebugEvent {
    pub time_us: u32, // 起動からの µs（DebugFrame と同じ基準）
    pub kind: u8,
    pub data: [u8; 4],
}
#[cfg(feature = "debug_stream")]
pub const DEBUG_EVENTS_QUEUE_SIZE: usize = 16;
// イベント: touch_task (Core1, Note) → debug_stream_task (Core0)
#[cfg(feature = "debug_stream")]
pub static DEBUG_EVENTS: Channel<CriticalSectionRawMutex, DebugEvent, DEBUG_EVENTS_QUEUE_SIZE> =
    Channel::new();
// PC がポートを開いているか: debug_stream_task → touch_task
// false の間はイベントをキューに入れない
#[cfg(feature = "debug_stream")]
pub static DEBUG_CONNECTED: AtomicBool = AtomicBool::new(false);

// スキャン周期（µs）。PC の period コマンドで変える: debug_stream_task → touch_task
#[cfg(feature = "debug_stream")]
static SCAN_PERIOD_SETTING_US: AtomicU32 = AtomicU32::new(SCAN_PERIOD_US);

/// 今のスキャン周期（µs）。debug_stream が無いときは SCAN_PERIOD_US（const）のまま
pub fn scan_period_us() -> u32 {
    #[cfg(feature = "debug_stream")]
    {
        SCAN_PERIOD_SETTING_US.load(Ordering::Relaxed)
    }
    #[cfg(not(feature = "debug_stream"))]
    {
        SCAN_PERIOD_US
    }
}

/// スキャン周期を変える。SCAN_PERIODS_US に無い値なら変えずに false を返す
#[cfg(feature = "debug_stream")]
pub fn set_scan_period_us(period_us: u32) -> bool {
    if SCAN_PERIODS_US.contains(&period_us) {
        SCAN_PERIOD_SETTING_US.store(period_us, Ordering::Relaxed);
        true
    } else {
        false
    }
}
