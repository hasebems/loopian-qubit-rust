use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_time::{Duration, with_timeout};

use crate::constants;
use crate::devices::{at42qt, pca9544};

/// 全キーの生値（Key Signal）を読む。基準値の管理・ノイズ除去は touch_algo が行う（doc/touch_baseline.md §4.3）
pub struct ReadTouch {
    raw_value: [u16; constants::TOTAL_QT_KEYS],
    valid: u128, // 最後のスキャンの読み取り成功フラグ（1 bit / キー）
}

impl ReadTouch {
    const CH_CONVERSION: [u8; 4] = [0, 1, 2, 3];
    const AT42_READ_TIMEOUT_MS: u64 = 2;
    const INIT_I2C_TIMEOUT_MS: u64 = 3;
    // 前回の値とこれ以上違えば、そのキーだけ読み直す。
    // AT42QT1070 は 16bit の値の 2 バイトをまとめて固定しないので、上位を読んでから下位を読むまでの間に
    // チップが値を更新し、256 の境目をまたぐと、約 ±256 ずれた値が読める（hi/lo ずれ。データシート 4.3.2 の Note）。
    // 次の更新は 8ms 後なので、すぐに読み直せば正しい値が得られる。本当に大きく変化したときは
    // 同じ値が読めるだけで害は無い。ずれは 256 前後なので、それより少し小さくする
    const HI_LO_REREAD_DIFF: u16 = 220;

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

    pub fn new() -> Self {
        Self {
            raw_value: [0u16; constants::TOTAL_QT_KEYS],
            valid: 0,
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

    /// 全キーをスキャンし、生値と読み取り成功フラグを更新する。
    /// 読み取りに失敗したキーはフラグを 0 にし、生値は前回のままにする
    pub async fn touch_sensor_scan(
        &mut self,
        pca: &pca9544::Pca9544,
        at42: &mut at42qt::At42Qt1070,
        i2c: &mut I2c<'static, I2C1, i2c::Async>,
    ) {
        self.valid = 0;
        for ch in 0..(constants::TOTAL_CH as u8) {
            let dev = ch / constants::PCA9544_NUM_CHANNELS;
            let ch_in_dev = Self::convert_channel(ch);
            pca.select(i2c, dev, ch_in_dev).await.ok();

            let start_ch = (ch as usize) * constants::AT42QT_KEYS_PER_DEVICE;
            let mut raw_data = [0u16; constants::AT42QT_KEYS_PER_DEVICE];
            let read_result = with_timeout(
                Duration::from_millis(Self::AT42_READ_TIMEOUT_MS),
                at42.read_6key(i2c, &mut raw_data),
            )
            .await;

            if let Ok(Ok(())) = read_result {
                for (key, rawd) in raw_data
                    .iter()
                    .take(constants::AT42QT_KEYS_PER_DEVICE)
                    .enumerate()
                {
                    let sid = start_ch + key;
                    let mut raw = *rawd;
                    let old = self.raw_value[sid];
                    if old != 0 && raw.abs_diff(old) >= Self::HI_LO_REREAD_DIFF {
                        // hi/lo ずれの疑い。このキーだけ読み直す。失敗したら前回の値を使う
                        raw = match with_timeout(
                            Duration::from_millis(Self::AT42_READ_TIMEOUT_MS),
                            at42.read_1key(i2c, key as u8),
                        )
                        .await
                        {
                            Ok(Ok(value)) => value,
                            _ => old,
                        };
                    }
                    self.raw_value[sid] = raw;
                    self.valid |= 1u128 << sid;
                }
            }
            // PCA9544のチャネルが最後のときに切断する
            if Self::is_last_channel(ch) {
                pca.disconnect(i2c, dev).await.ok();
            }
        }
    }

    /// 最後のスキャンの生値（hi/lo ずれを読み直した後）と読み取り成功フラグ（1 bit / キー）。
    /// 読み取りに失敗したキーはフラグが 0 で、生値は前回のまま
    pub fn raw(&self) -> (&[u16; constants::TOTAL_QT_KEYS], u128) {
        (&self.raw_value, self.valid)
    }
}
