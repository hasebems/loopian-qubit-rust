use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C1;
use portable_atomic::Ordering;

use crate::constants;
use crate::devices::{at42qt, pca9544};
use crate::{DEBUG_STATE, TOUCH_RAW_DATA};

pub struct ReadTouch {
    refference: [u16; constants::TOTAL_QT_KEYS],
}

impl ReadTouch {
    const ADRS_CONVERTION: [u8; 4] = [3, 2, 1, 0];

    pub fn new() -> Self {
        Self {
            refference: [0u16; constants::TOTAL_QT_KEYS],
        }
    }

    pub async fn init_touch_sensors(
        &mut self,
        pca: &pca9544::Pca9544,
        at42: &mut at42qt::At42Qt1070,
        i2c: &mut I2c<'static, I2C1, i2c::Async>,
    ) {
        for ch in 0..constants::PCA9544_NUM_CHANNELS * constants::PCA9544_NUM_DEVICES {
            let dev = Self::ADRS_CONVERTION[(ch / constants::PCA9544_NUM_CHANNELS) as usize];
            let ch_in_dev = ch % constants::PCA9544_NUM_CHANNELS;
            pca.select(i2c, dev, ch_in_dev).await.ok();
            at42.init(i2c).await.ok();
            if ch_in_dev == constants::PCA9544_NUM_CHANNELS - 1 {
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
        let mut data = TOUCH_RAW_DATA.lock().await;
        for ch in 0..constants::PCA9544_NUM_CHANNELS * constants::PCA9544_NUM_DEVICES {
            let dev = Self::ADRS_CONVERTION[(ch / constants::PCA9544_NUM_CHANNELS) as usize];
            let ch_in_dev = ch % constants::PCA9544_NUM_CHANNELS;
            pca.select(i2c, dev, ch_in_dev).await.ok();
            for key in 0..constants::AT42QT_KEYS_PER_DEVICE {
                if let Ok(raw_data) = at42.read_state(i2c, key, false).await {
                    let sid = (ch * constants::AT42QT_KEYS_PER_DEVICE + key) as usize;

                    if let Ok(ref_data) = at42.read_state(i2c, key, true).await {
                        self.refference[sid] = ref_data;
                    }

                    if raw_data >= self.refference[sid] {
                        data[sid] = raw_data - self.refference[sid];
                    } else {
                        data[sid] = 0;
                    }
                }
            }
            if ch_in_dev == constants::PCA9544_NUM_CHANNELS - 1 {
                pca.disconnect(i2c, dev).await.ok();
            }
        }
        DEBUG_STATE.store(data[0] as u8, Ordering::Relaxed);
    }
}
