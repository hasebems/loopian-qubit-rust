use embassy_rp::Peri;
use embassy_rp::peripherals::{DMA_CH0, PIO0};
use embassy_rp::pio_programs::ws2812::PioWs2812Program;
use embassy_time::{Duration, with_timeout};
use portable_atomic::Ordering;

use crate::Irqs;
use crate::constants::*;
use crate::error::ERROR_CODE;
use crate::shared::{RINGLED_RX_BITS, TOUCH0, TOUCH1, TOUCH2, TOUCH3};
use crate::ui;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      RingLED Task: 共有状態(TOUCH0-3, RXビット)からNeopixelを制御
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn ringled_task(
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
        let rx_bits: [u32; RINGLED_RX_WORDS] =
            core::array::from_fn(|i| RINGLED_RX_BITS[i].load(Ordering::Relaxed));

        ring_led.render(&mut data, &touch_locations, &rx_bits);
        // バグ対策: NeoPixel書き込みが固着してもタスク全体が停止しないようタイムアウト保護
        let write_result = with_timeout(Duration::from_millis(8), ws2812.write(&data)).await;
        if write_result.is_err() {
            ERROR_CODE.store(44, Ordering::Relaxed);
        }
        ticker.next().await;
    }
}
