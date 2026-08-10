use crate::constants::*;
use crate::{ANY_TOUCH, PRESSURE};
use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_time::{Duration, with_timeout};
use embassy_usb::class::midi::Sender;
use portable_atomic::Ordering;

const PRESSURE_THRESHOLD: u32 = 100;
pub const PRESSURE_BASELINE_WINDOW: usize = 512;
const ADJUSTMENT_TABLE: [u32; 4] = [256, 256, 256, 0]; // x/256
const BASELINE_RISE_TRACK_PERCENT: u32 = 10; // 100に近いほど基準値がサンプルの上昇に追従しやすくなり、ドリフト耐性が下がる
const BASELINE_FALL_TRACK_PERCENT: u32 = 50; // 100に近いほど基準値がサンプルの下降に追従しやすくなり、復帰が速くなる
const PRESSURE_SENSITIVITY: u32 = 6; // Pressure生値を割って、EXP_PRESSURE_TABLEのインデックスに変換するための係数
const EXP_MIN_VALUE: u8 = 40; // この値を最小値、127を最大値として、Expの値は変化する
const EXP_SEND_DEADBAND: u8 = 4; // この値未満の変化は送信せず、ジッタ由来の細かい更新を抑える
const EXP_MAX_STEP: u8 = 2; // 1回の送信での変化量を制限し、急激な増減を段階的に追従させる
const MIDI_CC_EXPRESSION: u8 = 11;
const MIDI_CC_INPUT_CH: u8 = 0x0b;
const MIDI_CC_STATUS: u8 = 0xb0 | MIDI_CH_VIOLIN;
const MIDI_CC_ALL_SOUND_OFF: u8 = 120;
const EXP_INDEX_MAX: usize = 100;
const EXP_PRESSURE_TABLE: [u8; EXP_INDEX_MAX + 1] = [
    // 100段階の圧力を 100段階のCC11に変換するためのテーブル
    0, 4, 8, 11, 14, 17, 19, 22, 24, 27, 29, 31, 33, 34, 36, 38, 40, 41, 43, 44, 45, 47, 48, 49, 51,
    52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 67, 68, 69, 70, 71, 71, 72, 73,
    73, 74, 75, 76, 76, 77, 77, 78, 79, 79, 80, 81, 81, 82, 82, 83, 83, 84, 85, 85, 86, 86, 87, 87,
    88, 88, 89, 89, 90, 90, 91, 91, 91, 92, 92, 93, 93, 94, 94, 94, 95, 95, 96, 96, 97, 97, 97, 98,
    98, 99, 99, 100,
];

pub struct PressureMidiState {
    last_sent_cc11: u8,
    previous_work_mode: WorkMode,
}

impl PressureMidiState {
    pub const fn new() -> Self {
        Self {
            last_sent_cc11: EXP_MIN_VALUE,
            previous_work_mode: WorkMode::Piano,
        }
    }

    pub fn reset_for_violin_mode(&mut self) -> u8 {
        self.last_sent_cc11 = EXP_MIN_VALUE;
        EXP_MIN_VALUE
    }

    fn entered_violin_mode(&self, work_mode: WorkMode) -> bool {
        self.previous_work_mode != WorkMode::Violin && work_mode == WorkMode::Violin
    }

    fn update_work_mode(&mut self, work_mode: WorkMode) {
        self.previous_work_mode = work_mode;
    }

    pub fn next_cc11_to_send(&mut self) -> Option<u8> {
        let target = pressure_to_cc11(PRESSURE.load(Ordering::Relaxed));
        let delta = target as i16 - self.last_sent_cc11 as i16;
        // 目標値との差が小さい間は送信せず、ジッタ由来の細かい更新を抑える。
        if delta.unsigned_abs() < EXP_SEND_DEADBAND as u16 {
            return None;
        }

        // 1回あたりの変化量を制限し、急激な増減を段階的に追従させる。
        let step = delta.clamp(-(EXP_MAX_STEP as i16), EXP_MAX_STEP as i16);
        let candidate = (self.last_sent_cc11 as i16 + step) as u8;
        // 送信予定値を次回比較の基準として保持する。
        self.last_sent_cc11 = candidate;
        Some(candidate)
    }
}

pub fn pressure_to_cc11(pressure: u32) -> u8 {
    let index = (pressure / PRESSURE_SENSITIVITY).min(EXP_INDEX_MAX as u32) as usize;
    let variable_range = (127 - EXP_MIN_VALUE) as u32;
    let scaled = (EXP_PRESSURE_TABLE[index] as u32 * variable_range) / 100;
    (scaled + EXP_MIN_VALUE as u32).min(127) as u8
}

async fn send_control_change(
    sender: &mut Sender<'static, Driver<'static, USB>>,
    controller: u8,
    value: u8,
) -> Result<(), ()> {
    let result = with_timeout(
        Duration::from_millis(MIDI_TX_TIMEOUT_MS),
        sender.write_packet(&[MIDI_CC_INPUT_CH, MIDI_CC_STATUS, controller, value]),
    )
    .await;
    if result.is_err() {
        return Err(());
    }
    Ok(())
}

pub async fn send_pressure_cc11_if_needed(
    sender: &mut Sender<'static, Driver<'static, USB>>,
    pressure_midi: &mut PressureMidiState,
    work_mode: WorkMode,
) -> Result<(), ()> {
    if work_mode != WorkMode::Violin {
        pressure_midi.update_work_mode(work_mode);
        return Ok(());
    }

    if pressure_midi.entered_violin_mode(work_mode) {
        let init_cc11 = pressure_midi.reset_for_violin_mode();
        send_control_change(sender, MIDI_CC_ALL_SOUND_OFF, 0).await?;
        send_control_change(sender, MIDI_CC_EXPRESSION, init_cc11).await?;
    } else if let Some(cc11_value) = pressure_midi.next_cc11_to_send() {
        send_control_change(sender, MIDI_CC_EXPRESSION, cc11_value).await?;
    }

    pressure_midi.update_work_mode(work_mode);
    Ok(())
}

pub fn update_pressure(
    samples: &[u32; MAX_ADC_CHANNELS],
    baseline_history: &mut [[u16; PRESSURE_BASELINE_WINDOW]; MAX_ADC_CHANNELS],
    baseline_sums: &mut [u64; MAX_ADC_CHANNELS],
    baseline_wptr: &mut usize,
    adc_counter: u32,
    work_mode_display: bool,
) {
    if adc_counter <= 100 {
        // 起動直後は安定した基準値が得られないため、圧力を0にしておく
        PRESSURE.store(0, Ordering::Relaxed);
        return;
    }

    if work_mode_display || *baseline_wptr < PRESSURE_BASELINE_WINDOW {
        // ワークモードでは基準値の更新のみ行う（センサーを触っていないことが前提）
        update_baseline_history(samples, baseline_history, baseline_sums, baseline_wptr);
        return;
    }

    // センサーおのおの、前回までの積算値から基準値を計算する
    let mut averages = [0u32; MAX_ADC_CHANNELS];
    for i in 0..MAX_ADC_CHANNELS {
        averages[i] = (baseline_sums[i] / PRESSURE_BASELINE_WINDOW as u64) as u32;
    }

    // 今回のサンプルと基準値の差を計算し、サンプル側が大きい場合は0、小さい場合は差分値として保持
    let mut diffs = [0u32; MAX_ADC_CHANNELS];
    for i in 0..MAX_ADC_CHANNELS {
        diffs[i] = averages[i].saturating_sub(samples[i]);
    }

    // 差分値がある一定値以上なら印加圧力とみなし、４つのセンサーの圧力を加算して保存
    let mut total_pressure = 0u32;
    let mut baseline_samples = *samples;
    for (i, diff) in diffs.iter().enumerate() {
        let adj_num = ADJUSTMENT_TABLE[i] * *diff / 256; // 調整値を計算
        if adj_num >= PRESSURE_THRESHOLD {
            let pressure = (adj_num * adj_num) / 100; // 差分値の二乗を圧力とする
            total_pressure = total_pressure.saturating_add(pressure);
        }

        // 基準値更新は「上昇時」と「下降時」で追従率を分ける。
        // 上昇追従を小さくするとドリフト耐性が上がり、下降追従を大きくすると復帰が速くなる。
        let avg = averages[i];
        let sample = samples[i];
        baseline_samples[i] = if sample >= avg {
            let rise = sample - avg;
            avg.saturating_add(rise * BASELINE_RISE_TRACK_PERCENT / 100)
        } else {
            let fall = avg - sample;
            avg.saturating_sub(fall * BASELINE_FALL_TRACK_PERCENT / 100)
        };
    }
    PRESSURE.store(total_pressure, Ordering::Relaxed);

    // 基準値の更新
    if !ANY_TOUCH.load(Ordering::Relaxed) {
        update_baseline_history(
            &baseline_samples,
            baseline_history,
            baseline_sums,
            baseline_wptr,
        );
    }
}

fn update_baseline_history(
    samples: &[u32; MAX_ADC_CHANNELS],
    baseline_history: &mut [[u16; PRESSURE_BASELINE_WINDOW]; MAX_ADC_CHANNELS],
    baseline_sums: &mut [u64; MAX_ADC_CHANNELS],
    baseline_wptr: &mut usize,
) {
    // 差分計算後に履歴と積算値を更新し、基準値を移動平均で保つ
    let baseline_index = *baseline_wptr % PRESSURE_BASELINE_WINDOW;
    for i in 0..MAX_ADC_CHANNELS {
        let new_sample = samples[i] as u16;
        let old_sample = baseline_history[i][baseline_index] as u64;
        baseline_history[i][baseline_index] = new_sample;
        baseline_sums[i] = baseline_sums[i]
            .saturating_sub(old_sample)
            .saturating_add(new_sample as u64);
    }
    *baseline_wptr = baseline_wptr.wrapping_add(1);
}
