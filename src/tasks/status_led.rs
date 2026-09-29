use embassy_rp::gpio::Output;
use embassy_time::Timer;
use portable_atomic::Ordering;

use crate::error::ERROR_CODE;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Status LED Task (Core0): Heartbeat LEDの点滅とエラーコードの表示
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn status_led_task(mut led: Output<'static>) {
    loop {
        let code = ERROR_CODE.load(Ordering::Relaxed);
        if code != 0 {
            // エラーコードは2桁表示に制限して、十の位→一の位の順で点滅する
            let code_2digit = code.min(99);
            let tens = code_2digit / 10;
            let ones = code_2digit % 10;

            for _ in 0..tens {
                led.set_low();
                Timer::after_millis(250).await;
                led.set_high();
                Timer::after_millis(250).await;
            }

            // 十の位と一の位の区切り
            Timer::after_millis(400).await;

            for _ in 0..ones {
                led.set_low();
                Timer::after_millis(250).await;
                led.set_high();
                Timer::after_millis(250).await;
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
