/// AT42QT1070 （ただし実体はMUXの向こうにあるので addr は固定で良い）
pub struct At42Qt1070 {}

impl At42Qt1070 {
    const ADDR: u8 = 0x1B;
    const _STATUS: u8 = 2;
    const LP_MODE: u8 = 54;
    const MAX_DUR: u8 = 55;
    const KEY_SIGNAL: u8 = 4; // Key Signal（key0 の上位バイト）。チップの基準値（18〜）は読まない
    const AVE_AKS_KEY6: u8 = 45;

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
        i2c.write(Self::ADDR, &i2cdata).await?;
        // 使っていない key6 を無効にする（AVE = 0。データシート §2.3 の推奨）。測定するキーが減り、
        // 全キーの測定時間が 8ms を超えて更新周期が延びるのを抑える
        let i2cdata = [Self::AVE_AKS_KEY6, 0];
        i2c.write(Self::ADDR, &i2cdata).await
    }

    /// 1キーの Key Signal を読み取る（key: 0-6）
    pub async fn read_1key<I2C>(&mut self, i2c: &mut I2C, key: u8) -> Result<u16, I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        let mut buf = [0u8; 2];
        let wr_adrs = Self::KEY_SIGNAL + key * 2;
        i2c.write_read(Self::ADDR, &[wr_adrs], &mut buf).await?;
        Ok(buf[0] as u16 * 256 + buf[1] as u16)
    }
    /// 6キーの Key Signal を読み取る
    pub async fn read_6key<I2C>(
        &mut self,
        i2c: &mut I2C,
        result: &mut [u16; 6],
    ) -> Result<(), I2C::Error>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        let mut buf = [0u8; 12];
        i2c.write_read(Self::ADDR, &[Self::KEY_SIGNAL], &mut buf)
            .await?;
        for i in 0..6 {
            result[i] = buf[i * 2] as u16 * 256 + buf[i * 2 + 1] as u16;
        }
        Ok(())
    }
}
