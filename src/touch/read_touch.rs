use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_time::{Duration, with_timeout};
use portable_atomic::Ordering;

use crate::TOUCH_RAW_DATA;
use crate::WORK_MODE_DISPLAY;
use crate::constants;
use crate::devices::{at42qt, pca9544};
use crate::{POINT0, POINT1, POINT2, POINT3, POINT4, POINT5};

// read_touch 内で完結する ON/OFF ヒステリシス制限
const TOUCH_LATCH_ON_LIMIT: u16 = 38; // タッチセンサーの値がこの値以上のときにタッチONとみなす。値が大きいほどタッチONの判定が厳しくなり、誤検出が減るが、反応も悪くなる。
const TOUCH_LATCH_OFF_LIMIT: u16 = 20; // タッチセンサーの値がこの値以下のときにタッチOFFとみなす。値が小さいほどタッチOFFの判定が厳しくなり、誤検出が減るが、反応も悪くなる。
const TOUCH_NOISE_FLOOR: u16 = 8; // タッチセンサーの値がこの値以下の場合、ノイズとして無視する。
const TOUCH_DIFF_GAIN_DEFAULT_X256: u16 = 256; // 1.0

// コンパイル時のみに走る関数
const fn set_touch_gain_if_in_bounds(
    table: &mut [u16; constants::TOTAL_QT_KEYS],
    idx: usize,
    gain_x256: u16,
) {
    if idx < constants::TOTAL_QT_KEYS {
        table[idx] = gain_x256;
    }
}

// コンパイル時のみに走る関数
const fn build_touch_diff_gain_table() -> [u16; constants::TOTAL_QT_KEYS] {
    let mut table = [TOUCH_DIFF_GAIN_DEFAULT_X256; constants::TOTAL_QT_KEYS];
    // キーごとの感度補正をここで設定する。例: 1.2倍は 307。
    set_touch_gain_if_in_bounds(&mut table, 12, 384); // 1.5倍
    set_touch_gain_if_in_bounds(&mut table, 18, 332); // 1.3倍
    set_touch_gain_if_in_bounds(&mut table, 30, 332); // 1.3倍
    set_touch_gain_if_in_bounds(&mut table, 42, 384); // 1.5倍
    set_touch_gain_if_in_bounds(&mut table, 90, 384); // 1.5倍
    table
}

const TOUCH_DIFF_GAIN_TABLE: [u16; constants::TOTAL_QT_KEYS] = build_touch_diff_gain_table();

pub struct ReadTouch {
    raw_value: [u16; constants::TOTAL_QT_KEYS],
    reference: [u16; constants::TOTAL_QT_KEYS],
    reference_adjust: [u16; constants::TOTAL_QT_KEYS],
    reference_counter: usize,
    touch_latched: [bool; constants::TOTAL_QT_KEYS],
}

impl ReadTouch {
    const CH_CONVERSION: [u8; 4] = [3, 2, 1, 0];
    const AT42_READ_TIMEOUT_MS: u64 = 2;
    const INIT_I2C_TIMEOUT_MS: u64 = 3;

    fn channel_in_device(ch: u8) -> u8 {
        let num_channels = constants::PCA9544_NUM_CHANNELS;
        if num_channels == 1 {
            0
        } else {
            ch % num_channels
        }
    }

    fn is_last_channel(ch: u8) -> bool {
        Self::channel_in_device(ch) + 1 == constants::PCA9544_NUM_CHANNELS
    }

    fn convert_channel(ch: u8) -> u8 {
        Self::CH_CONVERSION[Self::channel_in_device(ch) as usize]
    }

    fn shifted_index(raw_index: usize) -> usize {
        (raw_index + constants::TOUCH_INDEX_SHIFT) % constants::TOTAL_QT_KEYS
    }

    fn apply_touch_hysteresis(&mut self, sid: usize, signal: u16) -> u16 {
        let latched = self.touch_latched[sid];
        self.touch_latched[sid] = if latched {
            signal > TOUCH_LATCH_OFF_LIMIT
        } else {
            signal >= TOUCH_LATCH_ON_LIMIT
        };

        if self.touch_latched[sid] {
            signal
        } else {
            signal.saturating_sub(TOUCH_NOISE_FLOOR)
        }
    }

    fn apply_diff_gain(&self, sid: usize, signal: u16) -> u16 {
        let gain = TOUCH_DIFF_GAIN_TABLE[sid] as u32;
        let scaled = (signal as u32).saturating_mul(gain) / 256;
        scaled.min(u16::MAX as u32) as u16
    }

    pub fn new() -> Self {
        Self {
            raw_value: [0u16; constants::TOTAL_QT_KEYS],
            reference: [0u16; constants::TOTAL_QT_KEYS],
            reference_adjust: [0u16; constants::TOTAL_QT_KEYS],
            reference_counter: 0,
            touch_latched: [false; constants::TOTAL_QT_KEYS],
        }
    }

    pub async fn init_touch_sensors(
        &mut self,
        pca: &pca9544::Pca9544,
        at42: &mut at42qt::At42Qt1070,
        i2c: &mut I2c<'static, I2C1, i2c::Async>,
    ) -> bool {
        for ch in 0..constants::PCA9544_NUM_CHANNELS * constants::PCA9544_NUM_DEVICES {
            let dev = ch / constants::PCA9544_NUM_CHANNELS;
            let ch_in_dev = Self::convert_channel(ch);
            if with_timeout(
                Duration::from_millis(Self::INIT_I2C_TIMEOUT_MS),
                pca.select(i2c, dev, ch_in_dev),
            )
            .await
            .is_err()
            {
                return false;
            }
            if with_timeout(
                Duration::from_millis(Self::INIT_I2C_TIMEOUT_MS),
                at42.init(i2c),
            )
            .await
            .is_err()
            {
                return false;
            }
            // PCA9544のチャネルが最後のときに切断する
            if Self::is_last_channel(ch)
                && with_timeout(
                    Duration::from_millis(Self::INIT_I2C_TIMEOUT_MS),
                    pca.disconnect(i2c, dev),
                )
                .await
                .is_err()
            {
                return false;
            }
        }
        true
    }

    pub async fn set_reference(
        &mut self,
        pca: &pca9544::Pca9544,
        at42: &mut at42qt::At42Qt1070,
        i2c: &mut I2c<'static, I2C1, i2c::Async>,
    ) {
        for ch in 0..constants::PCA9544_NUM_CHANNELS * constants::PCA9544_NUM_DEVICES {
            let dev = ch / constants::PCA9544_NUM_CHANNELS;
            let ch_in_dev = Self::convert_channel(ch);
            pca.select(i2c, dev, ch_in_dev).await.ok();

            let sid = (ch as usize) * constants::AT42QT_KEYS_PER_DEVICE;
            let mut raw_data = [0u16; constants::AT42QT_KEYS_PER_DEVICE];
            let read_result = with_timeout(
                Duration::from_millis(Self::AT42_READ_TIMEOUT_MS),
                at42.read_6key(i2c, &mut raw_data, true),
            )
            .await;
            if let Ok(Ok(())) = read_result {
                for (offset, reference_raw) in raw_data
                    .iter()
                    .take(constants::AT42QT_KEYS_PER_DEVICE)
                    .enumerate()
                {
                    let shifted_sid = Self::shifted_index(sid + offset);
                    self.reference[shifted_sid] = *reference_raw;
                }
            }
            // PCA9544のチャネルが最後のときに切断する
            if Self::is_last_channel(ch) {
                pca.disconnect(i2c, dev).await.ok();
            }
        }
    }

    pub async fn touch_sensor_scan(
        &mut self,
        pca: &pca9544::Pca9544,
        at42: &mut at42qt::At42Qt1070,
        i2c: &mut I2c<'static, I2C1, i2c::Async>,
    ) {
        let work_mode_display = WORK_MODE_DISPLAY.load(Ordering::Relaxed);
        let mut data = [0u16; constants::TOTAL_QT_KEYS];
        for ch in 0..(constants::TOTAL_CH as u8) {
            let dev = ch / constants::PCA9544_NUM_CHANNELS;
            let ch_in_dev = Self::convert_channel(ch);
            pca.select(i2c, dev, ch_in_dev).await.ok();

            let start_ch = (ch as usize) * constants::AT42QT_KEYS_PER_DEVICE;
            let mut raw_data = [0u16; constants::AT42QT_KEYS_PER_DEVICE];
            let read_result = with_timeout(
                Duration::from_millis(Self::AT42_READ_TIMEOUT_MS),
                at42.read_6key(i2c, &mut raw_data, false),
            )
            .await;

            if let Ok(Ok(())) = read_result {
                for (sid, rawd) in
                    (start_ch..).zip(raw_data.iter().take(constants::AT42QT_KEYS_PER_DEVICE))
                {
                    let shifted_sid = Self::shifted_index(sid);
                    let mut raw = *rawd;
                    let old = self.raw_value[shifted_sid];
                    if old != 0 && raw > old + 200 {
                        raw -= 256; // hiからloを読む間に数値が変化した場合の対策
                    }
                    self.raw_value[shifted_sid] = raw;
                    if work_mode_display {
                        // 表示モード中は補正値を大きい方向にのみ更新する
                        let adjust = raw.saturating_sub(self.reference[shifted_sid]);
                        if adjust > self.reference_adjust[shifted_sid] {
                            self.reference_adjust[shifted_sid] = adjust;
                        }
                    }

                    let signal = raw
                        .saturating_sub(self.reference[shifted_sid])
                        .saturating_sub(self.reference_adjust[shifted_sid]);
                    let scaled_signal = self.apply_diff_gain(shifted_sid, signal);
                    data[shifted_sid] = self.apply_touch_hysteresis(shifted_sid, scaled_signal);
                }
            } else {
                // 読み取り失敗時は前回値を維持してスキャン結果を連続化する
                for sid in start_ch..(start_ch + constants::AT42QT_KEYS_PER_DEVICE) {
                    let shifted_sid = Self::shifted_index(sid);
                    let raw = self.raw_value[shifted_sid];
                    if work_mode_display {
                        let adjust = raw.saturating_sub(self.reference[shifted_sid]);
                        if adjust > self.reference_adjust[shifted_sid] {
                            self.reference_adjust[shifted_sid] = adjust;
                        }
                    }

                    let signal = raw
                        .saturating_sub(self.reference[shifted_sid])
                        .saturating_sub(self.reference_adjust[shifted_sid]);
                    let scaled_signal = self.apply_diff_gain(shifted_sid, signal);
                    data[shifted_sid] = self.apply_touch_hysteresis(shifted_sid, scaled_signal);
                }
            }
            // PCA9544のチャネルが最後のときに切断する
            if Self::is_last_channel(ch) {
                pca.disconnect(i2c, dev).await.ok();
            }
        }
        {
            // タッチセンサーの生データを Mutex で保護されたグローバル変数に保存
            let mut raw_data = TOUCH_RAW_DATA.lock().await;
            raw_data.copy_from_slice(&data);
        }

        if self.reference_counter == 0 {
            self.set_reference(pca, at42, i2c).await;
        }
        self.reference_counter = (self.reference_counter + 1) % 12;

        POINT0.store(self.raw_value[0], Ordering::Relaxed);
        POINT1.store(self.raw_value[1], Ordering::Relaxed);
        POINT2.store(self.raw_value[2], Ordering::Relaxed);
        POINT3.store(self.raw_value[3], Ordering::Relaxed);
        POINT4.store(self.raw_value[4], Ordering::Relaxed);
        POINT5.store(self.raw_value[5], Ordering::Relaxed);
    }
}
