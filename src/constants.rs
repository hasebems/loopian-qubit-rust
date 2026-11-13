//  Created by Hasebe Masahiko on 2026/02/15.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//

// コア1のスタックサイズ。Core1 の touch_task は TouchTracker・PlayInfo（合わせて約 3.7KB）・ElectrodeSignal (96 キーで数 KB)・ReadTouch を持ち、
// 初期化時に一時的にスタックに置かれることがあるので、余裕を持たせる
pub const CORE1_STACK_SIZE: usize = 16384;

// タッチのスキャン周期と解析周期
// TouchTracker・PlayInfo の時間に関する定数は 10ms 毎に呼ばれる前提なので、解析は 10ms 毎に行う。
// 解析するフレームはフレームの数ではなく時刻で決める（前の解析から ANALYSIS_PERIOD_US 経ったフレーム。tasks/touch.rs）
// debug_stream では、チップの更新（8ms 周期）より速く読んで観察するため既定を 2ms にし、
// PC からのコマンドで SCAN_PERIODS_US のどれかに変えられる（doc/debug_env.md §3.3）
#[cfg(not(feature = "debug_stream"))]
pub const SCAN_PERIOD_US: u32 = 10_000; // スキャン周期の既定値
#[cfg(feature = "debug_stream")]
pub const SCAN_PERIOD_US: u32 = 2_000;
#[cfg(feature = "debug_stream")]
pub const SCAN_PERIODS_US: [u32; 4] = [2_000, 5_000, 10_000, 20_000]; // PC から選べる周期
pub const ANALYSIS_PERIOD_US: u32 = 10_000;
/// 解析の時刻がこのくらい早くても解析する（2ms 周期のフレームの揺れを吸収する。qubit_etude の DECIMATION_TOLERANCE_US と同じ）。
/// スキャン周期が 10ms より長い（20ms）ときは毎フレーム解析する。このとき TouchTracker・PlayInfo の時間の計算はずれる
pub const ANALYSIS_TOLERANCE_US: u32 = 1_000;
// 起動後、この時間はスキャンだけを行い、信号処理（touch_algo）にも解析（ノートの生成）にも渡さない。
// AT42QT1070 は電源投入から 230ms 未満で自身を校正する（データシート §6.5 TD）ので、
// その前の値で touch_algo が基準値を初期取得しないようにする（doc/touch_baseline.md §3.11）
pub const TOUCH_STARTUP_SETTLE_MS: u64 = 500;

// Message for Ringled / touch callback status

// キー数の構成: 本番 4ch×4台 (96キー)、etude 2ch×1台 (12キー)、no_pca9544 直結1個 (6キー)。
// CI の clippy --all-features で両方が有効になるので、no_pca9544 を優先する
#[cfg(not(any(feature = "etude", feature = "no_pca9544")))]
pub const PCA9544_NUM_CHANNELS: u8 = 4; // PCA9544のチャネル数
#[cfg(all(feature = "etude", not(feature = "no_pca9544")))]
pub const PCA9544_NUM_CHANNELS: u8 = 2; // PCA9544のチャネル数 (etude)
#[cfg(feature = "no_pca9544")]
pub const PCA9544_NUM_CHANNELS: u8 = 1; // PCA9544 無しで直結 (no_pca9544)

#[cfg(not(any(feature = "etude", feature = "no_pca9544")))]
pub const PCA9544_NUM_DEVICES: u8 = 4; // PCA9544の台数
#[cfg(any(feature = "etude", feature = "no_pca9544"))]
pub const PCA9544_NUM_DEVICES: u8 = 1; // PCA9544の台数 (etude / no_pca9544)
pub const AT42QT_KEYS_PER_DEVICE: usize = 6; // AT42QT1070

pub const TOTAL_CH: usize = (PCA9544_NUM_CHANNELS * PCA9544_NUM_DEVICES) as usize;
pub const TOTAL_QT_KEYS: usize = TOTAL_CH * AT42QT_KEYS_PER_DEVICE;
pub const NUM_LEDS: usize = TOTAL_QT_KEYS;
// 電極がリングに並んでいるか（最後の電極と電極 0 が隣り合う）。etude・no_pca9544 の試験用の基板は一列
pub const TOUCH_RING: bool = !cfg!(any(feature = "etude", feature = "no_pca9544"));
pub const RINGLED_RX_WORDS: usize = NUM_LEDS.div_ceil(32); // 受信ノート表示のビット列(1bit/LED)を格納する u32 の個数

pub use touch_algo::touch::MAX_TOUCH_POINTS; // 追跡するタッチの最大数
pub const MAX_ADC_CHANNELS: usize = 3; // ADCのチャンネル数

// MIDI
pub const _MIDI_CH_PIANO: u8 = 0; // ピアノ用MIDIチャンネル
pub const MIDI_CH_VIOLIN: u8 = 1; // バイオリン用MIDIチャンネル。2026-11-13 に動作モードを無くし、ノート・CC11 はすべてこれで送る

pub const MIDI_TX_TIMEOUT_MS: u64 = 20;
