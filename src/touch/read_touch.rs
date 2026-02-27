use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C1;
use portable_atomic::Ordering;

use crate::{ERROR_CODE, TOUCH_RAW_DATA};
use crate::constants;
use crate::devices::{at42qt, pca9544};
use crate::{POINT0, POINT1, POINT2, POINT3};

pub struct ReadTouch {
    refference: [u16; constants::TOTAL_QT_KEYS],
    refference_counter: usize,
}

impl ReadTouch {
    const CH_CONVERTION: [u8; 4] = [3, 2, 1, 0];

    pub fn new() -> Self {
        Self {
            refference: [0u16; constants::TOTAL_QT_KEYS],
            refference_counter: 0,
        }
    }

    pub async fn init_touch_sensors(
        &mut self,
        pca: &pca9544::Pca9544,
        at42: &mut at42qt::At42Qt1070,
        i2c: &mut I2c<'static, I2C1, i2c::Async>,
    ) {
        for ch in 0..constants::PCA9544_NUM_CHANNELS * constants::PCA9544_NUM_DEVICES {
            let dev = ch / constants::PCA9544_NUM_CHANNELS;
            let ch_in_dev = Self::CH_CONVERTION[(ch % constants::PCA9544_NUM_CHANNELS) as usize];
            pca.select(i2c, dev, ch_in_dev).await.ok();
            at42.init(i2c).await.ok();
            // PCA9544のチャネルが最後のときに切断する
            if ch % constants::PCA9544_NUM_CHANNELS == constants::PCA9544_NUM_CHANNELS - 1 {
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
        let mut data = [0u16; constants::TOTAL_QT_KEYS];
        for ch in 0..(constants::TOTAL_CH as u8) {
            let dev = ch / constants::PCA9544_NUM_CHANNELS;
            let ch_in_dev = Self::CH_CONVERTION[(ch % constants::PCA9544_NUM_CHANNELS) as usize];
            pca.select(i2c, dev, ch_in_dev).await.ok();

            let mut raw_data = [0u16; constants::AT42QT_KEYS_PER_DEVICE as usize];
            if let Ok(()) = at42.read_6key(i2c, &mut raw_data, false).await {
                for key in 0..constants::AT42QT_KEYS_PER_DEVICE {
                    let sid = (ch * constants::AT42QT_KEYS_PER_DEVICE + key) as usize;
                    if raw_data[key as usize] >= self.refference[sid] {
                        data[sid] = raw_data[key as usize] - self.refference[sid];
                    } else {
                    if data[sid] > 10 {
                        POINT0.store(sid as u16, Ordering::Relaxed);
                        POINT1.store(self.refference[sid], Ordering::Relaxed);
                        POINT2.store(old, Ordering::Relaxed);
                        POINT3.store(raw, Ordering::Relaxed);
                    }
                }
            }
            // PCA9544のチャネルが最後のときに切断する
            if ch % constants::PCA9544_NUM_CHANNELS == constants::PCA9544_NUM_CHANNELS - 1 {
                pca.disconnect(i2c, dev).await.ok();
            }
        }
        {
            // タッチセンサーの生データを Mutex で保護されたグローバル変数に保存
            let mut raw_data = TOUCH_RAW_DATA.lock().await;
            raw_data.copy_from_slice(&data);
        }

        if self.refference_counter == 0 {
            for ch in 0..constants::PCA9544_NUM_CHANNELS * constants::PCA9544_NUM_DEVICES {
                let dev = ch / constants::PCA9544_NUM_CHANNELS;
                let ch_in_dev =
                    Self::CH_CONVERTION[(ch % constants::PCA9544_NUM_CHANNELS) as usize];
                pca.select(i2c, dev, ch_in_dev).await.ok();

                let mut raw_data = [0u16; constants::AT42QT_KEYS_PER_DEVICE as usize];
                if let Ok(()) = at42.read_6key(i2c, &mut raw_data, true).await {
                    for key in 0..constants::AT42QT_KEYS_PER_DEVICE {
                        let sid = (ch * constants::AT42QT_KEYS_PER_DEVICE + key) as usize;
                        let raw = raw_data[key as usize];
                        //let old = self.refference[sid];
                        self.refference[sid] = raw;//if raw - old > 50 {
                        //    ERROR_CODE.store(70 + sid as u8, Ordering::Relaxed);
                        //    old + 50
                        //} else {
                        //    raw
                        //};
                    }
                }
                // PCA9544のチャネルが最後のときに切断する
                if ch % constants::PCA9544_NUM_CHANNELS == constants::PCA9544_NUM_CHANNELS - 1 {
                    pca.disconnect(i2c, dev).await.ok();
                }
            }
        }
        self.refference_counter = (self.refference_counter + 1) % 24;

        //POINT0.store(self.raw_value[64], Ordering::Relaxed);
        //POINT1.store(self.raw_value[65], Ordering::Relaxed);
        //POINT2.store(self.raw_value[66], Ordering::Relaxed);
        //POINT3.store(self.raw_value[67], Ordering::Relaxed);
    }
}
