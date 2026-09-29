use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_time::{Duration, Instant, Ticker, with_timeout};
use portable_atomic::Ordering;

use crate::constants::*;
use crate::devices;
use crate::error;
use crate::shared::{ANALYSIS_TIME, PERIOD_OVERRUN, SCAN_TIME, WORK_MODE};
use crate::tasks::midi::queue_midi;
use crate::touch;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Touch Task (Core1): I2C1 を専有し、スキャン → 解析 → ノートイベントの生成を
//      SCAN_PERIOD_MS 毎に行う。ノートイベントは MIDI 送信キューに入れる
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn touch_task(mut i2c: I2c<'static, I2C1, i2c::Async>) {
    use core::cell::Cell;
    use touch::qtouch::QubitTouch;

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
        error::set(error::TOUCH_INIT_TIMEOUT);
    }

    // コールバック内で使う動作モード（解析の直前に更新する）
    let current_mode = Cell::new(WorkMode::Piano);
    let mut qt = QubitTouch::new(|status, note, velocity, _location| {
        // MIDIコールバック: タッチイベントをMIDIパケットに変換して送信キューに入れる
        let status = status & 0xf0; // コマンド部分
        let midi_channel = if current_mode.get() == WorkMode::Piano {
            MIDI_CH_FLOW
        } else {
            MIDI_CH_VIOLIN
        };
        let status = if status == RINGLED_CMD_TX_MOVED {
            0x80 | midi_channel // 移動イベントはNote Offとして扱う
        } else {
            status | midi_channel
        };
        queue_midi([status >> 4, status, note, velocity]);
    });

    let period = Duration::from_millis(SCAN_PERIOD_MS);
    let mut ticker = Ticker::every(period);
    let mut frame = 0u32;
    let mut touch_values = [0u16; TOTAL_QT_KEYS];

    // Task Loop
    loop {
        ticker.next().await;
        let cycle_start = Instant::now();

        // タッチセンサのスキャン
        read_touch
            .touch_sensor_scan(&pca, &mut at42, &mut i2c, &mut touch_values)
            .await;
        SCAN_TIME.record(cycle_start.elapsed().as_micros() as u32);

        // 解析: QubitTouch は 10ms 毎に呼ばれる前提なので、ANALYSIS_DIVIDER フレームに 1 回行う
        if frame.is_multiple_of(ANALYSIS_DIVIDER) {
            let analysis_start = Instant::now();
            let work_mode = WORK_MODE
                .load(Ordering::Relaxed)
                .try_into()
                .unwrap_or(WorkMode::Piano);
            current_mode.set(work_mode);

            for (ch, tv) in touch_values.iter().enumerate() {
                qt.set_value(ch, *tv);
            }
            qt.seek_and_update_touch_point(work_mode);

            qt.lighten_leds(|_location, _intensity| {
                // LEDの明るさをタッチの強さに応じて変化させる
                //WHITE_LEVEL.store(intensity as u8, Ordering::Relaxed);
            });
            ANALYSIS_TIME.record(analysis_start.elapsed().as_micros() as u32);
        }
        frame = frame.wrapping_add(1);

        // 周期を超えたときは、遅れた周期を取り戻さずに捨てる。
        // Ticker は期限を過ぎていると待たずに連続して完了するため、そのままだと
        // QubitTouch がほぼ 0ms 間隔で呼ばれ、時間の計算が狂う
        if cycle_start.elapsed() > period {
            PERIOD_OVERRUN.fetch_add(1, Ordering::Relaxed);
            ticker.reset();
        }
    }
}
