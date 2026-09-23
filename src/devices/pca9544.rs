#[cfg(not(feature = "no_pca9544"))]
use crate::constants;

pub struct Pca9544 {}

impl Pca9544 {
    #[cfg(not(feature = "no_pca9544"))]
    const ADDR: u8 = 0x70;
    pub const fn new() -> Self {
        Self {}
    }

    #[cfg(not(feature = "no_pca9544"))]
    pub async fn select<I2C>(&self, i2c: &mut I2C, dev: u8, ch: u8) -> Result<(), I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        let num_channels = constants::PCA9544_NUM_CHANNELS;
        let ch = if num_channels == 1 {
            0
        } else {
            ch % num_channels
        };
        i2c.write(Self::ADDR + dev, &[0x04 + ch]).await
    }
    #[cfg(not(feature = "no_pca9544"))]
    pub async fn disconnect<I2C>(&self, i2c: &mut I2C, dev: u8) -> Result<(), I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        i2c.write(Self::ADDR + dev, &[0x00]).await
    }

    // PCA9544 無し構成: AT42QT1070 が I2C に直結されているので、切替・切断は何もしない
    #[cfg(feature = "no_pca9544")]
    pub async fn select<I2C>(&self, _i2c: &mut I2C, _dev: u8, _ch: u8) -> Result<(), I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        Ok(())
    }
    #[cfg(feature = "no_pca9544")]
    pub async fn disconnect<I2C>(&self, _i2c: &mut I2C, _dev: u8) -> Result<(), I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        Ok(())
    }
}
