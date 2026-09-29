use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};
use embassy_usb::class::midi::Sender;
use portable_atomic::Ordering;

use crate::constants::*;
use crate::error::ERROR_CODE;
use crate::shared::{TOUCH_RAW_DATA, WORK_MODE};
use crate::touch;

// タッチイベントのデータ構造
#[derive(Copy, Clone, Default)]
struct TouchEvent(u8, u8, u8); // (status, note, velocity)

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      QubitTouch Task: タッチセンサのスキャンとMIDIイベントの送信
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn qubit_touch_task(mut sender: Sender<'static, Driver<'static, USB>>) {
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
