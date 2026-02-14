pub struct Pca9544 {}

impl Pca9544 {
    const ADDR: u8 = 0x74;
    pub const fn new() -> Self {
        Self {}
    }

    pub async fn select<I2C>(&self, i2c: &mut I2C, dev: u8, ch: u8) -> Result<(), I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        i2c.write(Self::ADDR + dev, &[0x04 + ch]).await
    }
}
