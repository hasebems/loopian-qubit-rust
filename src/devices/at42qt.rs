/// AT42QT1070 （ただし実体はMUXの向こうにあるので addr は固定で良い）
pub struct At42Qt1070 {
}

impl At42Qt1070 {
    const ADDR: u8 = 0x1B;
    const _STATUS: u8 = 2;
    const LP_MODE: u8 = 54;
    const MAX_DUR: u8 = 55;

    pub const fn new() -> Self {
        Self {}
    }

    /// 初期化
    pub async fn init<I2C>(&mut self, i2c: &mut I2C) -> Result<(), I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        let i2cdata = [Self::LP_MODE, 0];
        i2c.write(Self::ADDR, &i2cdata).await?;
        let i2cdata = [Self::MAX_DUR, 0];
        i2c.write(Self::ADDR, &i2cdata).await
    }

    /// 7キー分などの「現在状態ビット」を返す（例：bit0..bit6）
    pub async fn read_state<I2C>(&mut self, i2c: &mut I2C, key: u8, reference: bool) -> Result<u16, I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        let mut buf = [0u8; 2];
        let wr_adrs = if reference {
            18 + key * 2
        } else {
            4 + key * 2  //  0-6
        };  
        i2c.write_read(Self::ADDR, &[wr_adrs], &mut buf).await?;
        Ok(buf[0] as u16 * 256 + buf[1] as u16)
    }
}
