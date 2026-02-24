/// AT42QT1070 （ただし実体はMUXの向こうにあるので addr は固定で良い）
pub struct At42Qt1070 {}

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

    /// 1キーの状態を読み取る
    #[allow(dead_code)]
    pub async fn read_1key<I2C>(
        &mut self,
        i2c: &mut I2C,
        key: u8,
        reference: bool,
    ) -> Result<u16, I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        let mut buf = [0u8; 2];
        let wr_adrs = if reference {
            18 + key * 2
        } else {
            4 + key * 2 //  0-6
        };
        i2c.write_read(Self::ADDR, &[wr_adrs], &mut buf).await?;
        Ok(buf[0] as u16 * 256 + buf[1] as u16)
    }
    /// 6キーの状態を読み取る
    pub async fn read_6key<I2C>(
        &mut self,
        i2c: &mut I2C,
        reference: bool,
    ) -> Result<[u16; 6], I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        let mut buf = [0u8; 12];
        let wr_adrs = if reference { 18 } else { 4 };
        i2c.write_read(Self::ADDR, &[wr_adrs], &mut buf).await?;
        let mut result = [0u16; 6];
        for i in 0..6 {
            result[i] = buf[i * 2] as u16 * 256 + buf[i * 2 + 1] as u16;
        }
        Ok(result)
    }
}
