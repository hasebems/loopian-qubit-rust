use embassy_time::{Duration, Instant, Ticker, Timer};
use portable_atomic::Ordering;

use crate::constants::*;
use crate::shared::{TOUCH_RAW_DATA, WORK_MODE};
use crate::tasks::midi::queue_midi;
use crate::touch;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      QubitTouch Task: タッチデータを解析し、ノートイベントを MIDI 送信キューに入れる
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn qubit_touch_task() {
    use core::cell::Cell;
    use touch::qtouch::QubitTouch;

    // コールバック内で使う動作モード（ループの先頭で更新する）
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

    let mut loop_times = 0u64;
    let mut total_time = 0u64;
    let mut _ticker = Ticker::every(embassy_time::Duration::from_millis(10));

    loop {
        // タッチスキャンは10msごとに実行
        Timer::after(Duration::from_millis(10)).await;
        // ticker.next().await; // タッチスキャンはtickerに合わせて実行
        let start = Instant::now();
        let work_mode = WORK_MODE
            .load(Ordering::Relaxed)
            .try_into()
            .unwrap_or(WorkMode::Piano);
        current_mode.set(work_mode);

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
