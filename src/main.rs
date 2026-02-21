//  Created by Hasebe Masahiko on 2026/02/11.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
#![no_std]
#![no_main]

mod constants;
mod devices;
mod touch;
mod ui;

use embassy_executor::Executor;
use embassy_rp::Peri;
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_time::Timer;

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

// パニックハンドラ: エラーカウントを最大値にして永久ループ
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    ERROR_COUNT.store(255, Ordering::Relaxed);
    loop {
        asm::nop();
    }
}

/// Tell the Boot ROM about our application
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

// Core1 stack
static mut CORE1_STACK: Stack<{ constants::CORE1_STACK_SIZE }> = Stack::new();
static EXECUTOR0: StaticCell<embassy_executor::Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<embassy_executor::Executor> = StaticCell::new();

// OLEDバッファ転送用チャンネル（ダブルバッファリング）
use devices::ssd1306::OledBuffer;

static BUFFER_TO_DISPLAY: Channel<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    OledBuffer,
    2,
> = Channel::new();
static BUFFER_FROM_DISPLAY: Channel<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    OledBuffer,
    2,
> = Channel::new();

// デバッグ用状態フラグ（LED点滅パターンで表示）
// 0: 正常動作, 1-9: 各種状態, 10以上: エラー
pub static DEBUG_STATE: AtomicU8 = AtomicU8::new(0);
static ERROR_COUNT: AtomicU8 = AtomicU8::new(0);

// タッチセンサの生データ格納用（16bit/key）
pub static TOUCH_RAW_DATA: Mutex<
    CriticalSectionRawMutex,
    [u16; (constants::PCA9544_NUM_CHANNELS
        * constants::PCA9544_NUM_DEVICES
        * constants::AT42QT_KEYS_PER_DEVICE) as usize],
> = Mutex::new(
    [0u16;
        (constants::PCA9544_NUM_CHANNELS
            * constants::PCA9544_NUM_DEVICES
            * constants::AT42QT_KEYS_PER_DEVICE) as usize],
);

// RINGLED用メッセージチャンネル
static RINGLED_MESSAGE: Channel<
    CriticalSectionRawMutex,
    (u8, f32),
    { constants::RINGLED_MESSAGE_SIZE },
> = Channel::new();

// タッチイベントのデータ構造
#[derive(Copy, Clone, Default)]
struct TouchEvent(u8, u8, u8, f32); // (status, note, velocity, location)

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());

    // LEDピン
    let led = Output::new(p.PIN_25, Level::Low);

    // USB Driver
    let driver = Driver::new(p.USB, Irqs);
    let mut config = Config::new(0x1209, 0x3690); // Vendor ID / Product ID
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
        ERROR_COUNT.store(1, Ordering::Relaxed);
    }
    if BUFFER_FROM_DISPLAY.try_send(OledBuffer::new()).is_err() {
        ERROR_COUNT.store(2, Ordering::Relaxed);
    }

    // Core1起動
    spawn_core1(
        p.CORE1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            executor1.run(|spawner| {
                match core1_led_task(led) {
                    Ok(token) => spawner.spawn(token),
                    Err(_) => ERROR_COUNT.store(10, Ordering::Relaxed),
                }
                match core1_i2c_task(i2c) {
                    Ok(token) => spawner.spawn(token),
                    Err(_) => ERROR_COUNT.store(11, Ordering::Relaxed),
                }
                match core1_oled_ui_task() {
                    Ok(token) => spawner.spawn(token),
                    Err(_) => ERROR_COUNT.store(12, Ordering::Relaxed),
                }
            });
        },
    );

    let usb = builder.build();
    let (sender, receiver) = class.split();

    // Core0もExecutorを回す（必須）
    let executor0 = EXECUTOR0.init(Executor::new());
    executor0.run(|spawner| {
        match qubit_touch_task(sender) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_COUNT.store(20, Ordering::Relaxed),
        }
        match usb_task(usb) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_COUNT.store(21, Ordering::Relaxed),
        }
        match midi_rx_task(receiver) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_COUNT.store(22, Ordering::Relaxed),
        }
        match ringled_task(common, sm0, p.DMA_CH0, p.PIN_26, ws2812_program) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_COUNT.store(23, Ordering::Relaxed),
        }
    });
}

#[embassy_executor::task]
async fn ringled_task(
    mut common: embassy_rp::pio::Common<'static, PIO0>,
    sm: embassy_rp::pio::StateMachine<'static, PIO0, 0>,
    dma: Peri<'static, DMA_CH0>,
    pin: Peri<'static, embassy_rp::peripherals::PIN_26>,
    program: &'static PioWs2812Program<'static, PIO0>,
) {
    // Neopixel on D0 (GP26)
    use devices::ws2812::wheel;
    use embassy_rp::pio_programs::ws2812::RgbwPioWs2812;
    use embassy_time::Ticker;
    use smart_leds::RGBW;

    // RgbwPioWs2812 is needed for RGBW
    let mut ws2812 = RgbwPioWs2812::new(&mut common, sm, dma, Irqs, pin, program);

    let mut ticker = Ticker::every(embassy_time::Duration::from_millis(20));
    let mut speed: i32 = 0;

    // LEDの数
    const NUM_LEDS: usize = constants::TOTAL_QT_KEYS;
    let mut data = [RGBW::default(); NUM_LEDS];
    let mut rxkey_state = [false; NUM_LEDS]; // 受信したNote On/Offの状態を保持
    let mut txkey_state = [false; NUM_LEDS]; // 送信したNote On/Offの状態を保持

    loop {
        let (cmd, location) = RINGLED_MESSAGE
            .try_receive()
            .unwrap_or((constants::RINGLED_CMD_NONE, 0.0)); // ここは止まらない
        let num = (location as u8)
            .saturating_sub(constants::KEYBD_LO)
            .clamp(0, 5);
        match cmd {
            constants::RINGLED_CMD_RX_ON => rxkey_state[num as usize] = true,
            constants::RINGLED_CMD_RX_OFF => rxkey_state[num as usize] = false,
            constants::RINGLED_CMD_TX_ON => txkey_state[num as usize] = true,
            constants::RINGLED_CMD_TX_OFF => txkey_state[num as usize] = false,
            _ => {}
        }
        for (i, led) in data.iter_mut().enumerate().take(NUM_LEDS) {
            let color = wheel(speed.wrapping_add(i as i32 * 16) as u8);
            // Convert RGB8 to RGBW (White is controlled by MIDI)
            *led = RGBW {
                r: color.r,
                g: color.g,
                b: color.b,
                a: smart_leds::White(if rxkey_state[i] { 255 } else { 0 }),
            };
        }
        ws2812.write(&data).await;

        speed = speed.wrapping_add(2); // 色の変化速度
        ticker.next().await;
    }
}

#[embassy_executor::task]
async fn qubit_touch_task(mut sender: Sender<'static, Driver<'static, USB>>) {
    use core::cell::RefCell;
    use touch::qtouch::QubitTouch;
    let send_buffer = RefCell::new([TouchEvent::default(); 8]);
    let send_index = RefCell::new(0);
    let mut qt = QubitTouch::new(|status, note, velocity, location| {
        // MIDIコールバック: タッチイベントをMIDIパケットに変換して送信
        let packet = TouchEvent(status, note, velocity, location);
        let mut buf = send_buffer.borrow_mut();
        let mut idx = send_index.borrow_mut();
        if *idx < buf.len() {
            buf[*idx] = packet;
            *idx += 1;
        } else {
            // バッファオーバーフローの場合はエラーカウントをインクリメント
            ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    });
    loop {
        // タッチスキャンは10msごとに実行
        Timer::after(embassy_time::Duration::from_millis(10)).await;
        {
            // タッチセンサの生データを取得してQubitTouchにセット
            let data = TOUCH_RAW_DATA.lock().await;
            for ch in 0..constants::TOTAL_QT_KEYS {
                qt.set_value(ch, data[ch]);
            }
        }
        qt.seek_and_update_touch_point();
        let idx = *send_index.borrow();
        const MAX_EVENT: usize = 8;
        if idx == 0 {
            // no event
        } else if idx < MAX_EVENT {
            // await前にバッファをコピーして借用を解放
            let mut packets = [TouchEvent::default(); MAX_EVENT];
            {
                let buf = send_buffer.borrow();
                packets[0..idx].copy_from_slice(&buf[0..idx]);
            }
            for packet in packets.iter().take(idx) {
                if let Err(_) = sender
                    .write_packet(&[(packet.0 & 0xf0) >> 4, packet.0, packet.1, packet.2])
                    .await
                {
                    // エラーハンドリング
                    ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
                }
                RINGLED_MESSAGE.send((packet.0, packet.3)).await;
            }
            *send_index.borrow_mut() = 0;
        } else {
            // バッファオーバーフロー
            ERROR_COUNT.store(15, Ordering::Relaxed);
        }
        qt.lighten_leds(|_location, _intensity| {
            // LEDの明るさをタッチの強さに応じて変化させる
            //WHITE_LEVEL.store(intensity as u8, Ordering::Relaxed);
        });
    }
}

#[embassy_executor::task]
async fn usb_task(mut usb: embassy_usb::UsbDevice<'static, Driver<'static, USB>>) {
    usb.run().await;
}

#[embassy_executor::task]
async fn midi_rx_task(mut receiver: Receiver<'static, Driver<'static, USB>>) {
    let mut buf = [0; 64];

    loop {
        match receiver.read_packet(&mut buf).await {
            Ok(n) => {
                for packet in buf[0..n].chunks(4) {
                    if packet.len() == 4 {
                        let status = packet[1];
                        let note = packet[2];
                        let velocity = packet[3];

                        // Note On (Channel 0-15)
                        if (status & 0xF0) == 0x90 {
                            if velocity > 0 {
                                RINGLED_MESSAGE
                                    .send((constants::RINGLED_CMD_RX_ON, note as f32))
                                    .await;
                            } else {
                                RINGLED_MESSAGE
                                    .send((constants::RINGLED_CMD_RX_OFF, note as f32))
                                    .await;
                            }
                        }
                        // Note Off
                        else if (status & 0xF0) == 0x80 {
                            RINGLED_MESSAGE
                                .send((constants::RINGLED_CMD_RX_OFF, note as f32))
                                .await;
                        }
                    }
                }
            }
            Err(_e) => {
                // エラーカウント
                ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[embassy_executor::task]
async fn core1_led_task(mut led: Output<'static>) {
    loop {
        led.set_high();
        Timer::after_millis(100).await;
        led.set_low();
        Timer::after_millis(900).await;
    }
}

#[embassy_executor::task]
async fn core1_i2c_task(mut i2c: I2c<'static, I2C1, i2c::Async>) {
    // AT42QT1070 と PCA9544 の生成
    let pca = devices::pca9544::Pca9544::new();
    let mut at42 = devices::at42qt::At42Qt1070::new();

    // OLED初期化（I2Cを保持しない）
    use crate::devices::ssd1306::Oled;
    let mut oled = Oled::new();

    // --- init phase ---
    let mut read_touch = touch::read_touch::ReadTouch::new(); // タッチイベントの状態を保持する構造体を生成
    read_touch.init_touch_sensors(&pca, &mut at42, &mut i2c).await;

    // OLED初期化
    if oled.init(&mut i2c).is_err() {
        ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    // Task Loop
    loop {
        // OLED更新:UIタスクから描画済みバッファを受信（非ブロッキング）
        if let Ok(buffer) = BUFFER_TO_DISPLAY.try_receive() {
            if oled.flush_buffer(&buffer, &mut i2c).is_err() {
                ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
            }

            // バッファを返却
            if BUFFER_FROM_DISPLAY.try_send(buffer).is_err() {
                ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }

        // タッチセンサのスキャンとイベント処理
        read_touch.touch_sensor_scan(&pca, &mut at42, &mut i2c).await;

        // 他のタスクに処理を譲る
        embassy_futures::yield_now().await;
    }
}

#[embassy_executor::task]
async fn core1_oled_ui_task() {
    use ui::oled_display::OledDemo;

    let mut demo = OledDemo::new();
    let mut counter = 0u8;

    loop {
        // 空バッファを受信
        let mut buffer = BUFFER_FROM_DISPLAY.receive().await;

        // 描画
        let state = DEBUG_STATE.load(Ordering::Relaxed);
        let errors = ERROR_COUNT.load(Ordering::Relaxed);
        let delay = demo.tick(&mut buffer, state, errors, counter);
        counter = counter.wrapping_add(1);

        // 描画済みバッファを送信
        BUFFER_TO_DISPLAY.send(buffer).await;

        // 次のステップまで待機
        Timer::after_millis(delay).await;
    }
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
