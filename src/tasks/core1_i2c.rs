use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_time::{Duration, Instant, with_timeout};
use portable_atomic::Ordering;

use crate::devices;
use crate::error::ERROR_CODE;
use crate::shared::{BUFFER_FROM_DISPLAY, BUFFER_TO_DISPLAY, ELAPSED_TIME};
use crate::touch;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Core1 I2C Task: タッチセンサとOLED Device の処理
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn core1_i2c_task(mut i2c: I2c<'static, I2C1, i2c::Async>) {
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
