use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_time::{Duration, Instant, with_timeout};
use portable_atomic::Ordering;

use crate::devices;
use crate::error::ERROR_CODE;
use crate::shared::ELAPSED_TIME;
use crate::touch;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Touch Scan Task (Core1): I2C1 を専有してタッチセンサをスキャンする
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn touch_scan_task(mut i2c: I2c<'static, I2C1, i2c::Async>) {
    const TOUCH_INIT_TIMEOUT_MS: u64 = 80;

    // AT42QT1070 と PCA9544 の生成
    let pca = devices::pca9544::Pca9544::new();
    let mut at42 = devices::at42qt::At42Qt1070::new();

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

    let start = Instant::now();

    // Task Loop
    loop {
        // タッチセンサのスキャンとイベント処理
        read_touch
            .touch_sensor_scan(&pca, &mut at42, &mut i2c)
            .await;

        // 他のタスクに処理を譲る
        embassy_futures::yield_now().await;

        // touch_scan_task起動からの経過時間(us)
        let elapsed_time = start.elapsed().as_micros();
        ELAPSED_TIME.store(elapsed_time, Ordering::Relaxed);
    }
}
