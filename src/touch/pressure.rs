use crate::PRESSURE;
use crate::constants::*;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_time::{Duration, with_timeout};
use embassy_usb::class::midi::Sender;
use portable_atomic::Ordering;

const PRESSURE_THRESHOLD: u32 = 100;
const ADJUSTMENT_TABLE: [u32; 4] = [256, 160, 256, 0]; // x/256
const PRESSURE_SENSITIVITY: u32 = 20; // 大きいほど反応が悪くなる（MIDI値が低いまま）
const CC11_MIN_VALUE: u8 = 20;
const CC11_INDEX_MAX: usize = 100;
const CC11_SEND_DEADBAND: u8 = 2;
const CC11_MAX_STEP: u8 = 5;
const MIDI_CC_CIN: u8 = 0x0b;
const MIDI_CC_STATUS: u8 = 0xb0 | MIDI_CH_VIOLIN;
const MIDI_CC_ALL_SOUND_OFF: u8 = 120;
const MIDI_CC_EXPRESSION: u8 = 11;
const CC11_TABLE: [u8; CC11_INDEX_MAX + 1] = [
    0, 3, 6, 9, 12, 15, 18, 20, 23, 25, 28, 30, 32, 34, 36, 38, 40, 42, 44, 45, 47, 49, 50, 52, 54,
    55, 56, 58, 59, 61, 62, 63, 64, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81,
    82, 83, 83, 84, 85, 86, 86, 87, 88, 89, 89, 90, 91, 91, 92, 93, 93, 94, 94, 95, 96, 96, 97, 97,
    98, 98, 99, 100, 100, 101, 101, 102, 102, 103, 103, 103, 104, 104, 105, 105, 106, 106, 106,
    107, 107, 108, 108, 108, 109, 109, 110, 110,
];

pub struct PressureMidiState {
    last_sent_cc11: u8,
    previous_work_mode: u8,
}

impl PressureMidiState {
    pub const fn new() -> Self {
        Self {
            last_sent_cc11: CC11_MIN_VALUE,
            previous_work_mode: 0,
        }
    }

    pub fn reset_for_violin_mode(&mut self) -> u8 {
        self.last_sent_cc11 = CC11_MIN_VALUE;
        CC11_MIN_VALUE
    }

    fn entered_violin_mode(&self, work_mode: u8) -> bool {
        self.previous_work_mode != 1 && work_mode == 1
    }

    fn update_work_mode(&mut self, work_mode: u8) {
        self.previous_work_mode = work_mode;
    }

    pub fn next_cc11_to_send(&mut self) -> Option<u8> {
        let target = pressure_to_cc11(PRESSURE.load(Ordering::Relaxed));
        let delta = target as i16 - self.last_sent_cc11 as i16;
        // 目標値との差が小さい間は送信せず、ジッタ由来の細かい更新を抑える。
        if delta.unsigned_abs() < CC11_SEND_DEADBAND as u16 {
            return None;
        }

        // 1回あたりの変化量を制限し、急激な増減を段階的に追従させる。
        let step = delta.clamp(-(CC11_MAX_STEP as i16), CC11_MAX_STEP as i16);
        let candidate = (self.last_sent_cc11 as i16 + step) as u8;
        // 送信予定値を次回比較の基準として保持する。
        self.last_sent_cc11 = candidate;
        Some(candidate)
    }
}

pub fn pressure_to_cc11(pressure: u32) -> u8 {
    let index = (pressure / PRESSURE_SENSITIVITY).min(CC11_INDEX_MAX as u32) as usize;
    CC11_TABLE[index]
}

async fn send_control_change(
    sender: &mut Sender<'static, Driver<'static, USB>>,
    controller: u8,
    value: u8,
) -> Result<(), ()> {
    let result = with_timeout(
        Duration::from_millis(5),
        sender.write_packet(&[MIDI_CC_CIN, MIDI_CC_STATUS, controller, value]),
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
    work_mode: u8,
) -> Result<(), ()> {
    if work_mode != 1 {
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
    sums: &mut [u64; MAX_ADC_CHANNELS],
    adc_counter: u32,
) {
    if adc_counter > 100 {
        // センサーおのおの、前回までの積算値から平均値を計算する
        let mut averages = [0u32; MAX_ADC_CHANNELS];
        for i in 0..MAX_ADC_CHANNELS {
            averages[i] = (sums[i] / adc_counter as u64) as u32;
        }

        // 今回のサンプルと平均値の差を計算し、サンプル側が大きい場合は0、小さい場合は差分値として保持
        let mut diffs = [0u32; MAX_ADC_CHANNELS];
        for i in 0..MAX_ADC_CHANNELS {
            diffs[i] = averages[i].saturating_sub(samples[i]);
        }

        // 差分値がある一定値以上なら印加圧力とみなし、４つのセンサーの圧力を加算して保存
        let mut total_pressure = 0u32;
        for (i, diff) in diffs.iter().enumerate() {
            let adj_num = ADJUSTMENT_TABLE[i] * *diff / 256; // 調整値を計算
            if adj_num >= PRESSURE_THRESHOLD {
                let pressure = (adj_num * adj_num) / 100; // 差分値の二乗を圧力とする
                total_pressure = total_pressure.saturating_add(pressure);
            }
        }
        PRESSURE.store(total_pressure, Ordering::Relaxed);
    }

    // 次回の平均値計算に備えて積算値を更新する
    for i in 0..MAX_ADC_CHANNELS {
        sums[i] = sums[i].wrapping_add(samples[i] as u64);
    }
}
