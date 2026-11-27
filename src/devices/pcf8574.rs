//  Created by Hasebe Masahiko on 2026/11/25.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! PCF8574（8 ビットの GPIO エキスパンダー）。スイッチの読み込みに使う（doc/hw96_firmware261114.md §4）
//!
//! レジスタは無く、1 バイト書くとピンの出力（1 = 弱いプルアップの入力）、1 バイト読むとピンの状態になる。
//! 他のドライバと同じく I2C を所有せず、呼び出し毎に借りる
use embedded_hal_async::i2c::I2c;

pub struct Pcf8574 {
    addr: u8,
}

impl Pcf8574 {
    /// A2〜A0 をすべて GND にしたときのアドレス（7 ビット。doc/hw_modify.md §8.2）
    const DEFAULT_ADDR: u8 = 0x20;

    pub const fn new() -> Self {
        Self {
            addr: Self::DEFAULT_ADDR,
        }
    }

    /// 全ピンを入力にする（電源投入時も入力だが、念のため書く）
    pub async fn init<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), I2C::Error> {
        i2c.write(self.addr, &[0xFF]).await
    }

    /// ピンの状態を読む。ビット n が Pn
    pub async fn read<I2C: I2c>(&self, i2c: &mut I2C) -> Result<u8, I2C::Error> {
        let mut buf = [0u8; 1];
        i2c.read(self.addr, &mut buf).await?;
        Ok(buf[0])
    }
}
