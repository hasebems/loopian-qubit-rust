//  Created by Hasebe Masahiko on 2026/02/11.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
#![no_std]
#![no_main]

mod constants;
mod devices;
mod error;
mod shared;
mod tasks;
mod touch;
mod ui;

use cortex_m::asm;
use portable_atomic::Ordering;
use static_cell::StaticCell;

use embassy_executor::Executor;
use embassy_rp::multicore::{Stack, spawn_core1};

use rp235x_hal::{self as hal};

use embassy_rp::adc::InterruptHandler as AdcInterruptHandler;
use embassy_rp::bind_interrupts;
use embassy_rp::dma::InterruptHandler as DmaInterruptHandler;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{Config as I2cConfig, I2c, InterruptHandler as I2cInterruptHandler};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, I2C1, PIO0, USB};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::pio_programs::ws2812::PioWs2812Program;
use embassy_rp::usb::{Driver, InterruptHandler as UsbInterruptHandler};
use embassy_usb::class::midi::MidiClass;
use embassy_usb::{Builder, Config};

use crate::constants::*;
use crate::devices::ssd1306::OledBuffer;
use crate::error::ERROR_CODE;
use crate::shared::BUFFER_FROM_DISPLAY;

bind_interrupts!(pub struct Irqs {
    ADC_IRQ_FIFO => AdcInterruptHandler;
    I2C1_IRQ => I2cInterruptHandler<I2C1>;
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    DMA_IRQ_0 => DmaInterruptHandler<DMA_CH0>, DmaInterruptHandler<DMA_CH1>;
});

macro_rules! make_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: StaticCell<$t> = StaticCell::new();
        #[allow(unused_unsafe)]
        unsafe {
            STATIC_CELL.init($val)
        }
    }};
}

// パニックハンドラ: エラーカウントを最大値にして永久ループ
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    ERROR_CODE.store(255, Ordering::Relaxed);
    loop {
        asm::nop();
    }
}

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Global static variables
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
/// Tell the Boot ROM about our application
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

// Core1 stack
static mut CORE1_STACK: Stack<{ CORE1_STACK_SIZE }> = Stack::new();
static EXECUTOR0: StaticCell<embassy_executor::Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<embassy_executor::Executor> = StaticCell::new();

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Main entry point
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());

    // LEDピン
    // XIAO RP系の内蔵LEDは Active Low 想定: High=消灯, Low=点灯
    let led = Output::new(p.PIN_25, Level::High);

    // Switchピン
    let switch1 = Input::new(p.PIN_2, Pull::Up);
    let switch2 = Input::new(p.PIN_4, Pull::Up);

    // ADC
    let (adc, adc_a1, adc_a2, adc_a3) = (
        embassy_rp::adc::Adc::new(p.ADC, Irqs, embassy_rp::adc::Config::default()),
        embassy_rp::adc::Channel::new_pin(p.PIN_26, Pull::None),
        embassy_rp::adc::Channel::new_pin(p.PIN_27, Pull::None),
        embassy_rp::adc::Channel::new_pin(p.PIN_28, Pull::None),
    );
    let adc_dma = embassy_rp::dma::Channel::new(p.DMA_CH1, Irqs);

    // USB Driver
    let driver = Driver::new(p.USB, Irqs);
    let mut config = Config::new(0x1209, 0x3691); // Vendor ID / Product ID
    config.manufacturer = Some("Kigakudoh");
    config.product = Some("Loopian::QUBIT");
    config.serial_number = Some("000000");
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    // Buffers
    let config_descriptor = make_static!([u8; 256], [0; 256]);
    let bos_descriptor = make_static!([u8; 256], [0; 256]);
    let msos_descriptor = make_static!([u8; 256], [0; 256]);
    let control_buf = make_static!([u8; 64], [0; 64]);

    let mut builder = Builder::new(
        driver,
        config,
        config_descriptor,
        bos_descriptor,
        msos_descriptor,
        control_buf,
    );

    // Midi Class
    let class = MidiClass::new(&mut builder, 1, 1, 64);

    let mut i2c_config = I2cConfig::default();
    i2c_config.frequency = 400_000;
    let i2c = I2c::new_async(p.I2C1, p.PIN_7, p.PIN_6, Irqs, i2c_config);

    // PIO / Neopixel
    let Pio {
        mut common, sm0, ..
    } = Pio::new(p.PIO0, Irqs);
    let ws2812_program = make_static!(
        PioWs2812Program<'static, PIO0>,
        PioWs2812Program::new(&mut common)
    );

    // 初期バッファを準備してチャンネルに投入（Core1起動前に実行）
    // 2つのバッファを確実に投入
    if BUFFER_FROM_DISPLAY.try_send(OledBuffer::new()).is_err() {
        ERROR_CODE.store(11, Ordering::Relaxed);
    }
    if BUFFER_FROM_DISPLAY.try_send(OledBuffer::new()).is_err() {
        ERROR_CODE.store(12, Ordering::Relaxed);
    }

    // Core1起動
    spawn_core1(
        p.CORE1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            executor1.run(|spawner| {
                match tasks::status_led::core1_led_task(led) {
                    Ok(token) => spawner.spawn(token),
                    Err(_) => ERROR_CODE.store(21, Ordering::Relaxed),
                }
                match tasks::core1_i2c::core1_i2c_task(i2c) {
                    Ok(token) => spawner.spawn(token),
                    Err(_) => ERROR_CODE.store(22, Ordering::Relaxed),
                }
                match tasks::ui::core1_oled_ui_task(switch1, switch2) {
                    Ok(token) => spawner.spawn(token),
                    Err(_) => ERROR_CODE.store(23, Ordering::Relaxed),
                }
            });
        },
    );

    let usb = builder.build();
    let (sender, receiver) = class.split();

    // Core0もExecutorを回す（必須）
    let executor0 = EXECUTOR0.init(Executor::new());
    executor0.run(|spawner| {
        match tasks::touch::qubit_touch_task(sender) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_CODE.store(31, Ordering::Relaxed),
        }
        match usb_task(usb) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_CODE.store(32, Ordering::Relaxed),
        }
        match tasks::midi::midi_rx_task(receiver) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_CODE.store(33, Ordering::Relaxed),
        }
        match tasks::ringled::ringled_task(common, sm0, p.DMA_CH0, p.PIN_5, ws2812_program) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_CODE.store(34, Ordering::Relaxed),
        }
        match tasks::pressure::adc_task(adc, adc_a1, adc_a2, adc_a3, adc_dma) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_CODE.store(35, Ordering::Relaxed),
        }
    });
}

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      USB Task: USBデバイスの処理
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
async fn usb_task(mut usb: embassy_usb::UsbDevice<'static, Driver<'static, USB>>) {
    usb.run().await;
}

/// Program metadata for `picotool info`
#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [rp235x_hal::binary_info::EntryAddr; 5] = [
    rp235x_hal::binary_info::rp_cargo_bin_name!(),
    rp235x_hal::binary_info::rp_cargo_version!(),
    rp235x_hal::binary_info::rp_program_description!(c"Loopian::QUBIT"),
    rp235x_hal::binary_info::rp_cargo_homepage_url!(),
    rp235x_hal::binary_info::rp_program_build_attribute!(),
];
// End of file
