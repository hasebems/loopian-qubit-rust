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

use core::sync::atomic::AtomicBool;

use cortex_m::asm;
use portable_atomic::{AtomicI32, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};
use static_cell::StaticCell;

use embassy_executor::Executor;
use embassy_rp::Peri;
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};

use rp235x_hal::{self as hal};

use embassy_rp::adc::InterruptHandler as AdcInterruptHandler;
use embassy_rp::bind_interrupts;
use embassy_rp::dma::InterruptHandler as DmaInterruptHandler;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{self, Config as I2cConfig, I2c, InterruptHandler as I2cInterruptHandler};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, I2C1, PIO0, USB};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::pio_programs::ws2812::PioWs2812Program;
use embassy_rp::usb::{Driver, InterruptHandler as UsbInterruptHandler};
use embassy_usb::class::midi::{MidiClass, Receiver, Sender};
use embassy_usb::{Builder, Config};

use crate::constants::*;

bind_interrupts!(struct Irqs {
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
pub static ERROR_CODE: AtomicU8 = AtomicU8::new(0);
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    ERROR_CODE.store(255, Ordering::Relaxed);
    loop {
        asm::nop();
    }
}
// ERROR CODE 一覧(一の位も十の位も1-9の範囲)
// 11: BUFFER_FROM_DISPLAYの初期投入に失敗
// 12: BUFFER_FROM_DISPLAYの初期投入に失敗（2回目）
// 13: ADC値取得エラー
// 14: Touch Sensor初期化タイムアウト
// 21: Core1 LED Taskの起動に失敗
// 22: Core1 I2C Taskの起動に失敗
// 23: Core1 OLED UI Taskの起動に失敗
// 31: QubitTouch Taskの起動に失敗
// 32: USB Taskの起動に失敗
// 33: MIDI RX Taskの起動に失敗
// 34: RingLED Taskの起動に失敗
// 35: ADC Taskの起動に失敗
// 41: タッチイベントのバッファオーバーフロー
// 42: MIDIイベントの送信失敗（USB未接続など）
// 43: MIDIイベントのバッファオーバーフロー
// 44: RingLEDへの書き込みのタイムアウト
// 51: OLED初期化エラー
// 52: 描画バッファ受信エラー
// 53: 描画バッファ返却エラー
// 54: MIDIイベントの受信エラー

// タッチイベントのデータ構造
#[derive(Copy, Clone, Default)]
struct TouchEvent(u8, u8, u8); // (status, note, velocity)

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

// OLEDバッファ転送用チャンネル（ダブルバッファリング）
use devices::ssd1306::OledBuffer;
use embassy_sync::channel::Channel;

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

// 表示用変数
pub static POINT0: AtomicU16 = AtomicU16::new(0);
pub static POINT1: AtomicU16 = AtomicU16::new(0);
pub static POINT2: AtomicU16 = AtomicU16::new(0);
pub static POINT3: AtomicU16 = AtomicU16::new(0);
pub static POINT4: AtomicU16 = AtomicU16::new(0);
pub static POINT5: AtomicU16 = AtomicU16::new(0);
pub static TOUCH0: AtomicI32 = AtomicI32::new(10000);
pub static TOUCH1: AtomicI32 = AtomicI32::new(10000);
pub static TOUCH2: AtomicI32 = AtomicI32::new(10000);
pub static TOUCH3: AtomicI32 = AtomicI32::new(10000);
pub static RINGLED_RX_BITS: AtomicU32 = AtomicU32::new(0); // 受信Note On/Off状態(1bit/LED)
pub static ELAPSED_TIME: AtomicU64 = AtomicU64::new(0); // タッチスキャンの経過時間（us）
pub static AD_VALUE0: AtomicU32 = AtomicU32::new(0); // ADCの値(A0)
pub static AD_VALUE1: AtomicU32 = AtomicU32::new(0); // ADCの値(A1)
pub static AD_VALUE2: AtomicU32 = AtomicU32::new(0); // ADCの値(B0)
pub static AD_VALUE3: AtomicU32 = AtomicU32::new(0); // ADCの値(B1)
pub static PRESSURE: AtomicU32 = AtomicU32::new(0); // 圧力計算結果
pub static WORK_MODE: AtomicU8 = AtomicU8::new(0); // 動作モード（Piano/Violin）
pub static WORK_MODE_DISPLAY: AtomicBool = AtomicBool::new(false); // 動作モード変更表示状態
pub static DEBUG_VALUE: AtomicU32 = AtomicU32::new(0); // デバッグ用
pub static ANY_TOUCH: AtomicBool = AtomicBool::new(false);

// タッチセンサの生データ格納用（16bit/key）
pub static TOUCH_RAW_DATA: Mutex<
    CriticalSectionRawMutex,
    [u16; (PCA9544_NUM_CHANNELS * PCA9544_NUM_DEVICES) as usize * AT42QT_KEYS_PER_DEVICE],
> = Mutex::new(
    [0u16; (PCA9544_NUM_CHANNELS * PCA9544_NUM_DEVICES) as usize * AT42QT_KEYS_PER_DEVICE],
);

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
                match core1_led_task(led) {
                    Ok(token) => spawner.spawn(token),
                    Err(_) => ERROR_CODE.store(21, Ordering::Relaxed),
                }
                match core1_i2c_task(i2c) {
                    Ok(token) => spawner.spawn(token),
                    Err(_) => ERROR_CODE.store(22, Ordering::Relaxed),
                }
                match core1_oled_ui_task(switch1, switch2) {
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
        match qubit_touch_task(sender) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_CODE.store(31, Ordering::Relaxed),
        }
        match usb_task(usb) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_CODE.store(32, Ordering::Relaxed),
        }
        match midi_rx_task(receiver) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_CODE.store(33, Ordering::Relaxed),
        }
        match ringled_task(common, sm0, p.DMA_CH0, p.PIN_5, ws2812_program) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_CODE.store(34, Ordering::Relaxed),
        }
        match adc_task(adc, adc_a1, adc_a2, adc_a3, adc_dma) {
            Ok(token) => spawner.spawn(token),
            Err(_) => ERROR_CODE.store(35, Ordering::Relaxed),
        }
    });
}

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      RingLED Task: 共有状態(TOUCH0-3, RXビット)からNeopixelを制御
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
async fn ringled_task(
    mut common: embassy_rp::pio::Common<'static, PIO0>,
    sm: embassy_rp::pio::StateMachine<'static, PIO0, 0>,
    dma: Peri<'static, DMA_CH0>,
    pin: Peri<'static, embassy_rp::peripherals::PIN_5>,
    program: &'static PioWs2812Program<'static, PIO0>,
) {
    // Neopixel on D0 (GP26)
    use embassy_rp::pio_programs::ws2812::RgbwPioWs2812;
    use embassy_time::Ticker;
    use smart_leds::RGBW;
    use ui::ringled::RingLed;

    // RgbwPioWs2812 is needed for RGBW
    let mut ws2812 = RgbwPioWs2812::new(&mut common, sm, dma, Irqs, pin, program);
    let mut ring_led = RingLed::new();
    let mut ticker = Ticker::every(embassy_time::Duration::from_millis(20));

    let mut data = [RGBW::default(); NUM_LEDS];
    loop {
        let to_touch_location = |v: i32| -> Option<f32> {
            if (0..10000).contains(&v) {
                Some(v as f32 / 100.0)
            } else {
                None
            }
        };
        let touch_locations = [
            to_touch_location(TOUCH0.load(Ordering::Relaxed)),
            to_touch_location(TOUCH1.load(Ordering::Relaxed)),
            to_touch_location(TOUCH2.load(Ordering::Relaxed)),
            to_touch_location(TOUCH3.load(Ordering::Relaxed)),
        ];
        let rx_bits = RINGLED_RX_BITS.load(Ordering::Relaxed);

        ring_led.render(&mut data, &touch_locations, rx_bits);
        // バグ対策: NeoPixel書き込みが固着してもタスク全体が停止しないようタイムアウト保護
        let write_result = with_timeout(Duration::from_millis(8), ws2812.write(&data)).await;
        if write_result.is_err() {
            ERROR_CODE.store(44, Ordering::Relaxed);
        }
        ticker.next().await;
    }
}

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      QubitTouch Task: タッチセンサのスキャンとMIDIイベントの送信
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
async fn qubit_touch_task(mut sender: Sender<'static, Driver<'static, USB>>) {
    use core::cell::RefCell;
    use touch::pressure::{PressureMidiState, send_pressure_cc11_if_needed};
    use touch::qtouch::QubitTouch;

    let send_buffer = RefCell::new([TouchEvent::default(); 8]);
    let send_index = RefCell::new(0);
    let mut pressure_midi = PressureMidiState::new();
    let mut qt = QubitTouch::new(|status, note, velocity, _location| {
        // MIDIコールバック: タッチイベントをMIDIパケットに変換して送信
        let packet = TouchEvent(status, note, velocity);
        let mut buf = send_buffer.borrow_mut();
        let mut idx = send_index.borrow_mut();
        if *idx < buf.len() {
            buf[*idx] = packet;
            *idx += 1;
        } else {
            // バッファオーバーフローの場合はエラーカウントをインクリメント
            ERROR_CODE.store(41, Ordering::Relaxed);
        }
    });

    let mut loop_times = 0u64;
    let mut total_time = 0u64;
    let mut _ticker = Ticker::every(embassy_time::Duration::from_millis(10));

    loop {
        // タッチスキャンは10msごとに実行
        Timer::after(embassy_time::Duration::from_millis(10)).await;
        // ticker.next().await; // タッチスキャンはtickerに合わせて実行
        let start = Instant::now();
        let work_mode = WORK_MODE
            .load(Ordering::Relaxed)
            .try_into()
            .unwrap_or(WorkMode::Piano);

        // タッチセンサの生データを取得してQubitTouchにセット
        // ロック保持時間を最小化し、以降の await をロック外で実行する
        let mut touch_values = [0u16; TOTAL_QT_KEYS];
        {
            let data = TOUCH_RAW_DATA.lock().await;
            touch_values.copy_from_slice(&*data);
        }
        for (ch, tv) in touch_values.iter().enumerate() {
            qt.set_value(ch, *tv);
        }
        qt.seek_and_update_touch_point(work_mode);

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
                let status = packet.0 & 0xf0; // コマンド部分
                let midi_channel = if work_mode == WorkMode::Piano {
                    MIDI_CH_FLOW
                } else {
                    MIDI_CH_VIOLIN
                };
                let status = if status == RINGLED_CMD_TX_MOVED {
                    0x80 | midi_channel // 移動イベントはNote Offとして扱う
                } else {
                    status | midi_channel
                };
                let result = with_timeout(
                    Duration::from_millis(MIDI_TX_TIMEOUT_MS),
                    sender.write_packet(&[status >> 4, status, packet.1, packet.2]),
                )
                .await;
                if result.is_err() {
                    // タイムアウトまたは送信エラー（USB未接続時など）
                    ERROR_CODE.store(42, Ordering::Relaxed);
                }
            }
            *send_index.borrow_mut() = 0;
        } else {
            // バッファオーバーフロー
            ERROR_CODE.store(43, Ordering::Relaxed);
            *send_index.borrow_mut() = 0;
        }

        if send_pressure_cc11_if_needed(&mut sender, &mut pressure_midi, work_mode)
            .await
            .is_err()
        {
            ERROR_CODE.store(42, Ordering::Relaxed);
        }

        qt.lighten_leds(|_location, _intensity| {
            // LEDの明るさをタッチの強さに応じて変化させる
            //WHITE_LEVEL.store(intensity as u8, Ordering::Relaxed);
        });

        // 時間計測
        loop_times = loop_times.wrapping_add(1);
        total_time = total_time.wrapping_add(start.elapsed().as_micros());
        //ELAPSED_TIME.store(total_time / loop_times, Ordering::Relaxed);
    }
}

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      USB Task: USBデバイスの処理
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
async fn usb_task(mut usb: embassy_usb::UsbDevice<'static, Driver<'static, USB>>) {
    usb.run().await;
}

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      MIDI RX Task: USB経由で受信したMIDIイベントの処理
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
async fn midi_rx_task(mut receiver: Receiver<'static, Driver<'static, USB>>) {
    let mut buf = [0; 64];

    let set_rx_led = |note: u8, on: bool| {
        let led = (note as usize).min(NUM_LEDS - 1);
        let bit = 1u32 << led;
        if on {
            RINGLED_RX_BITS.fetch_or(bit, Ordering::Relaxed);
        } else {
            RINGLED_RX_BITS.fetch_and(!bit, Ordering::Relaxed);
        }
    };

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
                                set_rx_led(note, true);
                            } else {
                                set_rx_led(note, false);
                            }
                        }
                        // Note Off
                        else if (status & 0xF0) == 0x80 {
                            set_rx_led(note, false);
                        }
                    }
                }
            }
            Err(_e) => {
                // エラーカウント
                ERROR_CODE.store(54, Ordering::Relaxed);
            }
        }
    }
}

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Core1 LED Task: Heartbeat LEDの点滅
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
async fn core1_led_task(mut led: Output<'static>) {
    loop {
        let code = ERROR_CODE.load(Ordering::Relaxed);
        if code != 0 {
            // エラーコードは2桁表示に制限して、十の位→一の位の順で点滅する
            let code_2digit = code.min(99);
            let tens = code_2digit / 10;
            let ones = code_2digit % 10;

            for _ in 0..tens {
                led.set_low();
                Timer::after_millis(150).await;
                led.set_high();
                Timer::after_millis(150).await;
            }

            // 十の位と一の位の区切り
            Timer::after_millis(400).await;

            for _ in 0..ones {
                led.set_low();
                Timer::after_millis(150).await;
                led.set_high();
                Timer::after_millis(150).await;
            }

            // 次の表示シーケンスまで待機
            Timer::after_millis(1200).await;
        } else {
            // 正常時の点滅パターン
            led.set_low();
            Timer::after_millis(500).await;
            led.set_high();
            Timer::after_millis(500).await;
        }
    }
}

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      ADC Task (Core0): GP27/GP28 の連続サンプリング
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
async fn adc_task(
    mut adc: embassy_rp::adc::Adc<'static, embassy_rp::adc::Async>,
    adc_a1: embassy_rp::adc::Channel<'static>,
    adc_a2: embassy_rp::adc::Channel<'static>,
    adc_a3: embassy_rp::adc::Channel<'static>,
    mut adc_dma: embassy_rp::dma::Channel<'static>,
) {
    const ADC_READ_TIMEOUT_MS: u64 = 20;

    let mut channels = [adc_a1, adc_a2, adc_a3];
    let mut ad_value = [0u16; 3];
    let mut adc_counter = 0u32;
    let mut baseline_index = 0usize;
    let mut baseline_history =
        [[0u16; touch::pressure::PRESSURE_BASELINE_WINDOW]; MAX_ADC_CHANNELS];
    let mut baseline_sums = [0u64; MAX_ADC_CHANNELS];
    let mut samples = [0u32; MAX_ADC_CHANNELS];

    loop {
        // ADC読み込み準備時間を確保
        Timer::after_millis(10).await; // 1sensorあたり10msec

        let read_result = with_timeout(
            Duration::from_millis(ADC_READ_TIMEOUT_MS),
            adc.read_many_multichannel(&mut channels, &mut ad_value, 0, &mut adc_dma),
        )
        .await;

        match read_result {
            Ok(adc_result) => match adc_result {
                Ok(()) => {
                    AD_VALUE0.store(ad_value[0] as u32, Ordering::Relaxed);
                    AD_VALUE1.store(ad_value[1] as u32, Ordering::Relaxed);
                    AD_VALUE2.store(ad_value[2] as u32, Ordering::Relaxed);
                    samples[0] = ad_value[0] as u32;
                    samples[1] = ad_value[1] as u32;
                    samples[2] = ad_value[2] as u32;
                }
                Err(_) => {
                    // ADC内部エラー
                    ERROR_CODE.store(13, Ordering::Relaxed);
                }
            },
            Err(_) => {
                // タイムアウトエラー
                ERROR_CODE.store(13, Ordering::Relaxed);
            }
        }

        // 圧力を計算
        let wmd = WORK_MODE_DISPLAY.load(Ordering::Relaxed);
        touch::pressure::update_pressure(
            &samples,
            &mut baseline_history,
            &mut baseline_sums,
            &mut baseline_index,
            adc_counter,
            wmd,
        );
        adc_counter = adc_counter.wrapping_add(1);
    }
}

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Core1 I2C Task: タッチセンサとOLED Device の処理
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
async fn core1_i2c_task(mut i2c: I2c<'static, I2C1, i2c::Async>) {
    const TOUCH_INIT_TIMEOUT_MS: u64 = 80;
    const OLED_INIT_TIMEOUT_MS: u64 = 50;

    // AT42QT1070 と PCA9544 の生成
    let pca = devices::pca9544::Pca9544::new();
    let mut at42 = devices::at42qt::At42Qt1070::new();

    // OLED初期化（I2Cを保持しない）
    use crate::devices::ssd1306::Oled;
    let mut oled = Oled::new();

    // --- init phase ---
    let mut read_touch = touch::read_touch::ReadTouch::new(); // タッチイベントの状態を保持する構造体を生成
    let touch_init_result = with_timeout(
        Duration::from_millis(TOUCH_INIT_TIMEOUT_MS),
        read_touch.init_touch_sensors(&pca, &mut at42, &mut i2c),
    )
    .await;
    if !matches!(touch_init_result, Ok(true)) {
        ERROR_CODE.store(14, Ordering::Relaxed);
    }

    // OLED初期化
    let oled_init_result = with_timeout(
        Duration::from_millis(OLED_INIT_TIMEOUT_MS),
        oled.init(&mut i2c),
    )
    .await;
    if !matches!(oled_init_result, Ok(Ok(()))) {
        ERROR_CODE.store(51, Ordering::Relaxed);
    }

    let start = Instant::now();

    // Task Loop
    loop {
        // OLED更新:UIタスクから描画済みバッファを受信（非ブロッキング）
        if let Ok(buffer) = BUFFER_TO_DISPLAY.try_receive() {
            if oled.flush_buffer(&buffer, &mut i2c).await.is_err() {
                ERROR_CODE.store(52, Ordering::Relaxed);
            }

            // バッファを返却
            if BUFFER_FROM_DISPLAY.try_send(buffer).is_err() {
                ERROR_CODE.store(53, Ordering::Relaxed);
            }
        }

        // タッチセンサのスキャンとイベント処理
        read_touch
            .touch_sensor_scan(&pca, &mut at42, &mut i2c)
            .await;

        // 他のタスクに処理を譲る
        embassy_futures::yield_now().await;

        // core1_i2c_task起動からの経過時間(us)
        let elapsed_time = start.elapsed().as_micros();
        ELAPSED_TIME.store(elapsed_time, Ordering::Relaxed);
    }
}

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Core1 OLED UI Task: OLEDディスプレイの更新
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
async fn core1_oled_ui_task(switch1: Input<'static>, switch2: Input<'static>) {
    use ui::oled_display::GraphicsDisplay;

    let mut gui = GraphicsDisplay::new();
    let mut counter = 0u32;
    let mut ui_page = 0u8;

    let mut switch1_prev = false;
    let mut switch2_prev = false;

    // 初期画面表示
    let mut buffer = BUFFER_FROM_DISPLAY.receive().await;
    gui.draw_bringup_screen(&mut buffer);
    BUFFER_TO_DISPLAY.send(buffer).await;

    loop {
        // 次のステップまで待機(10fps想定)
        Timer::after_millis(100).await;

        // 空バッファを受信
        buffer = BUFFER_FROM_DISPLAY.receive().await;

        // スイッチの状態を取得
        let switch_r_state = switch1.is_low();
        let switch_l_state = switch2.is_low();
        if (switch_r_state != switch1_prev) && switch_r_state {
            if switch_l_state {
                // 両方のスイッチが同時に押された場合は、設定画面に直接遷移
                ui_page = 4;
                // 設定変更時にエラーコードをリセットする
                ERROR_CODE.store(0, Ordering::Relaxed);
                WORK_MODE_DISPLAY.store(true, Ordering::Relaxed);
            } else if ui_page == 3 || ui_page == 4 {
                ui_page = 0;
                WORK_MODE_DISPLAY.store(false, Ordering::Relaxed);
            } else {
                ui_page += 1;
            }
            gui.change_page(ui_page); // ページ切替をGUIに通知
        }
        if (switch_l_state != switch2_prev) && switch_l_state {
            if switch_r_state {
                // 両方のスイッチが同時に押された場合は、設定画面に直接遷移
                ui_page = 4;
                // 設定変更時にエラーコードをリセットする
                ERROR_CODE.store(0, Ordering::Relaxed);
                WORK_MODE_DISPLAY.store(true, Ordering::Relaxed);
            } else if ui_page == 4 {
                WORK_MODE.store(
                    (WORK_MODE.load(Ordering::Relaxed) + 1) % 2,
                    Ordering::Relaxed,
                ); // 動作モードを切り替え
            } else if ui_page == 0 {
                ui_page = 3;
            } else {
                ui_page -= 1;
            }
            gui.change_page(ui_page); // ページ切替をGUIに通知
        }
        switch1_prev = switch_r_state;
        switch2_prev = switch_l_state;

        // 描画
        gui.tick(&mut buffer, counter);
        counter = counter.wrapping_add(1);

        // 描画済みバッファを送信
        BUFFER_TO_DISPLAY.send(buffer).await;
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
