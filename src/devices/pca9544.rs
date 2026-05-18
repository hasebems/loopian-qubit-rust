use crate::constants;

pub struct Pca9544 {}

impl Pca9544 {
    const ADDR: u8 = 0x70;
    pub const fn new() -> Self {
        Self {}
    }

    pub async fn select<I2C>(&self, i2c: &mut I2C, dev: u8, ch: u8) -> Result<(), I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        #[cfg(not(feature = "test_mode"))]
        let ch = ch % constants::PCA9544_NUM_CHANNELS;
        i2c.write(Self::ADDR + dev, &[0x04 + ch]).await
    }
    pub async fn disconnect<I2C>(&self, i2c: &mut I2C, dev: u8) -> Result<(), I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        i2c.write(Self::ADDR + dev, &[0x00]).await
    }
}
