//  Created by Hasebe Masahiko on 2026/02/11.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
#![no_std]
#![no_main]

mod devices;
mod oled_demo;

use embassy_executor::Executor;
use embassy_rp::Peri;
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_time::Timer;

use defmt::{error, info, unwrap, warn};
use rp235x_hal::{self as hal};

use embassy_rp::bind_interrupts;
use embassy_rp::dma::InterruptHandler as DmaInterruptHandler;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::i2c::{self, Config as I2cConfig, I2c, InterruptHandler as I2cInterruptHandler};
use embassy_rp::peripherals::{DMA_CH0, I2C1, PIO0, USB};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::pio_programs::ws2812::PioWs2812Program;
use embassy_rp::usb::{Driver, InterruptHandler as UsbInterruptHandler};
use embassy_usb::class::midi::{MidiClass, Receiver, Sender};
use embassy_usb::{Builder, Config};

bind_interrupts!(struct Irqs {
    I2C1_IRQ => I2cInterruptHandler<I2C1>;
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    DMA_IRQ_0 => DmaInterruptHandler<DMA_CH0>;
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

use cortex_m::asm;
use portable_atomic::{AtomicU8, Ordering};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

/// Tell the Boot ROM about our application
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

// Core1 stack
static mut CORE1_STACK: Stack<4096> = Stack::new();
static EXECUTOR0: StaticCell<embassy_executor::Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<embassy_executor::Executor> = StaticCell::new();

// 診断用：Core1起動確認フラグ
#[allow(dead_code)]
static CORE1_ALIVE: AtomicU8 = AtomicU8::new(0);

// 診断用LED点滅周期（ms）
#[allow(dead_code)]
static DELAY_MS: AtomicU8 = AtomicU8::new(100);

// 進捗表示用ステージ（USBログが取れない場合の切り分け用）
// LEDは「ステージ回数だけ点滅 → 少し待つ」を繰り返します。
// 250/251 はエラー（高速点滅）。
#[allow(dead_code)]
static STAGE: AtomicU8 = AtomicU8::new(0);

fn led_on(led: &mut Output<'static>) {
    led.set_high();
}

fn led_off(led: &mut Output<'static>) {
    led.set_low();
}

#[allow(dead_code)]
enum FatalFail {
    Probe,
    Init,
    Flush,
}

fn delay_ms_blocking(ms: u32) {
    // だいたい。動作確認用（正確さ不要）
    // 150MHz想定で 150_000 cycles/ms くらい。
    asm::delay(ms.saturating_mul(150_000));
}

#[allow(dead_code)]
fn fatal_blink_forever(led: &mut Output<'static>, stage: u8, kind: FatalFail) -> ! {
    STAGE.store(stage, Ordering::Relaxed);
    loop {
        match kind {
            // 2回チカチカ → 長休み
            FatalFail::Probe => {
                for _ in 0..2 {
                    led_on(led);
                    delay_ms_blocking(80);
                    led_off(led);
                    delay_ms_blocking(120);
                }
                delay_ms_blocking(1_000);
            }
            // 3回チカチカ → 長休み
            FatalFail::Init => {
                for _ in 0..3 {
                    led_on(led);
                    delay_ms_blocking(80);
                    led_off(led);
                    delay_ms_blocking(120);
                }
                delay_ms_blocking(1_000);
            }
            // 4回チカチカ → 長休み (flushの途中でI2Cが落ちる/ノイズ等)
            FatalFail::Flush => {
                for _ in 0..4 {
                    led_on(led);
                    delay_ms_blocking(80);
                    led_off(led);
                    delay_ms_blocking(120);
                }
                delay_ms_blocking(1_000);
            }
        }
    }
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());
    info!("Program start - Dual Core Test (Core1 LED)");

    // LEDピン
    let led = Output::new(p.PIN_25, Level::Low);

    // USB Driver
    let driver = Driver::new(p.USB, Irqs);
    let mut config = Config::new(0xc0de, 0xcafe);
    config.manufacturer = Some("Embassy");
    config.product = Some("USB MIDI Device");
    config.serial_number = Some("12345678");
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

    // Core1起動
    spawn_core1(
        p.CORE1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            executor1.run(|spawner| {
                spawner.spawn(unwrap!(core1_led_task(led)));
                spawner.spawn(unwrap!(core1_oled_task(i2c)));
            });
        },
    );

    let usb = builder.build();
    let (sender, receiver) = class.split();

    // Core0もExecutorを回す（必須）
    let executor0 = EXECUTOR0.init(Executor::new());
    executor0.run(|spawner| {
        spawner.spawn(unwrap!(usb_task(usb)));
        spawner.spawn(unwrap!(midi_task(sender)));
        spawner.spawn(unwrap!(midi_rx_task(receiver)));
        // Neopixel on D0 (GP26)
        spawner.spawn(unwrap!(neopixel_task(
            common,
            sm0,
            p.DMA_CH0,
            p.PIN_26,
            ws2812_program
        )));
    });
}

#[embassy_executor::task]
async fn neopixel_task(
    mut common: embassy_rp::pio::Common<'static, PIO0>,
    sm: embassy_rp::pio::StateMachine<'static, PIO0, 0>,
    dma: Peri<'static, DMA_CH0>,
    pin: Peri<'static, embassy_rp::peripherals::PIN_26>,
    program: &'static PioWs2812Program<'static, PIO0>,
) {
    use devices::ws2812::wheel;
    use embassy_rp::pio_programs::ws2812::RgbwPioWs2812;
    use embassy_time::Ticker;
    use smart_leds::RGBW;

    info!("Neopixel task started (GP26 / RGBW)");

    // RgbwPioWs2812 is needed for RGBW
    let mut ws2812 = RgbwPioWs2812::new(&mut common, sm, dma, Irqs, pin, program);

    let mut ticker = Ticker::every(embassy_time::Duration::from_millis(20));
    let mut j: i32 = 0;

    // LEDの数
    const NUM_LEDS: usize = 6;
    let mut data = [RGBW::default(); NUM_LEDS];

    loop {
        for (i, led) in data.iter_mut().enumerate().take(NUM_LEDS) {
            let color = wheel(j.wrapping_add(i as i32 * 256 / NUM_LEDS as i32) as u8);
            // Convert RGB8 to RGBW (White is controlled by MIDI)
            let w = 0; //WHITE_LEVEL.load(Ordering::Relaxed);
            *led = RGBW {
                r: color.r,
                g: color.g,
                b: color.b,
                a: smart_leds::White(w),
            };
        }
        ws2812.write(&data).await;

        j = j.wrapping_add(1);
        ticker.next().await;
    }
}

#[embassy_executor::task]
async fn usb_task(mut usb: embassy_usb::UsbDevice<'static, Driver<'static, USB>>) {
    usb.run().await;
}

#[embassy_executor::task]
async fn midi_task(mut sender: Sender<'static, Driver<'static, USB>>) {
    info!("MIDI task started");
    let mut pitch = 60u8;
    loop {
        // Note On (Channel 0, Note 60, Velocity 64)
        info!("Note On {}", pitch);
        let packet = [0x09, 0x90, pitch, 64];
        sender.write_packet(&packet).await.unwrap();
        Timer::after_millis(500).await;

        // Note Off
        info!("Note Off {}", pitch);
        let packet = [0x08, 0x80, pitch, 64];
        sender.write_packet(&packet).await.unwrap();
        Timer::after_millis(500).await;

        pitch = if pitch < 72 { pitch + 1 } else { 60 };
    }
}

#[embassy_executor::task]
async fn midi_rx_task(mut receiver: Receiver<'static, Driver<'static, USB>>) {
    info!("MIDI RX task started");
    let mut buf = [0; 64];

    loop {
        match receiver.read_packet(&mut buf).await {
            Ok(n) => {
                for packet in buf[0..n].chunks(4) {
                    if packet.len() == 4 {
                        let status = packet[1];
                        let _note = packet[2];
                        let velocity = packet[3];

                        // Note On (Channel 0-15)
                        if (status & 0xF0) == 0x90 {
                            if velocity > 0 {
                                // Turn on White
                                //WHITE_LEVEL.store(255, Ordering::Relaxed);
                            } else {
                                // Note On with vel 0 is Note Off
                                //WHITE_LEVEL.store(0, Ordering::Relaxed);
                            }
                        }
                        // Note Off
                        else if (status & 0xF0) == 0x80 {
                            //WHITE_LEVEL.store(0, Ordering::Relaxed);
                        }
                    }
                }
            }
            Err(_e) => {
                warn!("Error reading MIDI packet");
            }
        }
    }
}

#[embassy_executor::task]
async fn core1_led_task(mut led: Output<'static>) {
    info!("Core1 LED task started");
    loop {
        led.set_high();
        Timer::after_millis(100).await; // 早めの点滅
        led.set_low();
        Timer::after_millis(100).await;
    }
}

#[embassy_executor::task]
async fn core1_oled_task(i2c: I2c<'static, I2C1, i2c::Async>) {
    use crate::devices::ssd1306::Oled;
    use crate::oled_demo::OledDemo;

    info!("Core1 OLED task started");
    match Oled::new(i2c) {
        Ok(mut oled) => {
            let mut demo = OledDemo::new();
            loop {
                match demo.tick(&mut oled) {
                    Ok(delay) => Timer::after_millis(delay).await,
                    Err(_) => {
                        error!("OLED tick failed");
                        Timer::after_millis(1000).await;
                    }
                }
            }
        }
        Err(_) => {
            error!("OLED init failed");
            loop {
                Timer::after_millis(1000).await;
            }
        }
    }
}

/// Program metadata for `picotool info`
#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [rp235x_hal::binary_info::EntryAddr; 5] = [
    rp235x_hal::binary_info::rp_cargo_bin_name!(),
    rp235x_hal::binary_info::rp_cargo_version!(),
    rp235x_hal::binary_info::rp_program_description!(c"RP2350 Template"),
    rp235x_hal::binary_info::rp_cargo_homepage_url!(),
    rp235x_hal::binary_info::rp_program_build_attribute!(),
];

// Core0: LED heartbeat (like C++ loop)
#[embassy_executor::task]
async fn heartbeat_task(mut led: Output<'static>) {
    // タスクが実行されたことを確認（すぐにLED点滅開始）
    let mut counter = 0u32;
    loop {
        counter = counter.wrapping_add(1);
        if counter.is_multiple_of(2) {
            led_on(&mut led);
        } else {
            led_off(&mut led);
        }
        embassy_time::Timer::after_millis(200).await; // 200msで高速点滅（動作確認用）
    }
}
// End of file
