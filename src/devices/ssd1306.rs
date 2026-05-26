use embedded_graphics::{pixelcolor::BinaryColor, prelude::*};

const DISPLAY_WIDTH: usize = 128;
const DISPLAY_HEIGHT: usize = 64;
const BUFFER_SIZE: usize = (DISPLAY_WIDTH * DISPLAY_HEIGHT) / 8;

/// フレームバッファ（描画用、1024バイト）
#[derive(Debug)]
pub struct OledBuffer {
    pub data: [u8; BUFFER_SIZE],
}

impl OledBuffer {
    /// 新規バッファを作成（ゼロクリア）
    pub fn new() -> Self {
        Self {
            data: [0; BUFFER_SIZE],
        }
    }

    /// バッファをクリア
    pub fn clear(&mut self) {
        self.data.fill(0);
    }
}

impl Default for OledBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// SSD1306 OLEDディスプレイドライバ（I2C非保持版）
pub struct Oled {
    addr: u8,
}

impl Oled {
    /// デフォルトアドレス(0x3C)で新規作成
    pub fn new() -> Self {
        Self::new_at(0x3C)
    }

    /// 指定アドレスで新規作成
    pub fn new_at(addr: u8) -> Self {
        Self { addr }
    }

    /// ディスプレイを初期化（I2Cを借用）
    pub async fn init<I2C>(&mut self, i2c: &mut I2C) -> Result<(), display_interface::DisplayError>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        // SSD1306初期化シーケンス
        self.send_commands(
            i2c,
            &[
                0xAE, // Display off
                0xD5, 0x80, // Set display clock divide ratio/oscillator frequency
                0xA8, 0x3F, // Set multiplex ratio (1 to 64)
                0xD3, 0x00, // Set display offset
                0x40, // Set start line address
                0x8D, 0x14, // Enable charge pump
                0x20, 0x00, // Set memory addressing mode (horizontal)
                0xA1, // Set segment re-map
                0xC8, // Set COM output scan direction
                0xDA, 0x12, // Set COM pins hardware configuration
                0x81, 0xFF, // Set contrast control
                0xD9, 0xF1, // Set pre-charge period
                0xDB, 0x40, // Set VCOMH deselect level
                0xA4, // Entire display on (resume to RAM content display)
                0xA6, // Set normal display (not inverted)
                0xAF, // Display on
            ],
        )
        .await?;

        Ok(())
    }

    /// 複数のコマンドを送信
    async fn send_commands<I2C>(
        &self,
        i2c: &mut I2C,
        commands: &[u8],
    ) -> Result<(), display_interface::DisplayError>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        let mut buf = [0u8; 32];
        buf[0] = 0x00; // Command mode

        for chunk in commands.chunks(31) {
            buf[1..=chunk.len()].copy_from_slice(chunk);
            i2c.write(self.addr, &buf[..=chunk.len()])
                .await
                .map_err(|_| display_interface::DisplayError::BusWriteError)?;
        }
        Ok(())
    }

    /// バッファをディスプレイに転送
    pub async fn flush_buffer<I2C>(
        &self,
        buffer: &OledBuffer,
        i2c: &mut I2C,
    ) -> Result<(), display_interface::DisplayError>
    where
        I2C: embedded_hal_async::i2c::I2c,
    {
        // ページアドレス範囲を設定（Page0〜Page7）
        self.send_commands(i2c, &[0x22, 0x00, 0x07]).await?;

        // カラムアドレス範囲を設定（0〜127）
        self.send_commands(i2c, &[0x21, 0x00, 0x7F]).await?;

        // データを送信（チャンク単位で）
        const CHUNK_SIZE: usize = 31;
        for chunk in buffer.data.chunks(CHUNK_SIZE) {
            let mut data = [0u8; CHUNK_SIZE + 1];
            data[0] = 0x40; // Data mode
            data[1..=chunk.len()].copy_from_slice(chunk);
            i2c.write(self.addr, &data[..=chunk.len()])
                .await
                .map_err(|_| display_interface::DisplayError::BusWriteError)?;
        }

        Ok(())
    }
}

impl Default for Oled {
    fn default() -> Self {
        Self::new()
    }
}

/// embedded-graphics の DrawTarget トレイト実装（OledBuffer用）
impl DrawTarget for OledBuffer {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels.into_iter() {
            let x = coord.x;
            let y = coord.y;

            if x >= 0 && x < DISPLAY_WIDTH as i32 && y >= 0 && y < DISPLAY_HEIGHT as i32 {
                let x = x as usize;
                let y = y as usize;
                let byte_idx = x + (y / 8) * DISPLAY_WIDTH;
                let bit_idx = y % 8;

                if byte_idx < BUFFER_SIZE {
                    match color {
                        BinaryColor::On => self.data[byte_idx] |= 1 << bit_idx,
                        BinaryColor::Off => self.data[byte_idx] &= !(1 << bit_idx),
                    }
                }
            }
        }
        Ok(())
    }
}

impl OriginDimensions for OledBuffer {
    fn size(&self) -> Size {
        Size::new(DISPLAY_WIDTH as u32, DISPLAY_HEIGHT as u32)
    }
}
