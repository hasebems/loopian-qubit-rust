use core::cell::RefCell;

use critical_section::Mutex;
use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_sync::mutex::Mutex as AsyncMutex;
use static_cell::StaticCell;

use embedded_hal::i2c::I2c as _;
use embedded_hal_bus::i2c::CriticalSectionDevice;

pub type I2c1Blocking = I2c<'static, I2C1, i2c::Blocking>;
pub type I2c1Bus = Mutex<RefCell<I2c1Blocking>>;

pub type I2c1Async = I2c<'static, I2C1, i2c::Async>;
pub type I2c1BusAsync = AsyncMutex<embassy_sync::blocking_mutex::raw::NoopRawMutex, I2c1Async>;

static I2C1_BUS: StaticCell<I2c1Bus> = StaticCell::new();
static I2C1_BUS_ASYNC: StaticCell<I2c1BusAsync> = StaticCell::new();

#[allow(dead_code)]
pub fn init_i2c1_bus(i2c: I2c1Blocking) -> &'static I2c1Bus {
    I2C1_BUS.init(Mutex::new(RefCell::new(i2c)))
}

#[allow(dead_code)]
pub fn init_i2c1_bus_async(i2c: I2c1Async) -> &'static I2c1BusAsync {
    I2C1_BUS_ASYNC.init(AsyncMutex::new(i2c))
}

#[allow(dead_code)]
fn scan_first_i2c_addr(i2c_bus: &'static I2c1Bus) -> Option<u8> {
    // 一般的な7bitアドレス範囲をスキャン
    let mut dev = CriticalSectionDevice::new(i2c_bus);
    for addr in 0x08u8..=0x77u8 {
        // embassy-rp は 0-length write をエラーにするため 1byte 送る
        // I2Cデバイスに影響が出にくいよう、SSD1306のNOP(0xE3)を使う
        if dev.write(addr, &[0x00, 0xE3]).is_ok() {
            return Some(addr);
        }
    }
    None
}
