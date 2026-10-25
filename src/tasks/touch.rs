use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_time::{Duration, Instant, Ticker, with_timeout};
use portable_atomic::Ordering;

use crate::constants::*;
use crate::devices;
use crate::error;
use crate::shared::{
    ANALYSIS_TIME, PERIOD_OVERRUN, POINT0, POINT1, POINT2, POINT3, POINT4, POINT5, SCAN_TIME,
    SETTING_MODE, WORK_MODE, scan_period_us,
};
use crate::tasks::midi::queue_midi;
use crate::touch;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Touch Task (Core1): I2C1 を専有し、スキャン → 信号処理（touch_algo）→ 解析 →
//      ノートイベントの生成を scan_period_us() 毎に行う。ノートイベントは MIDI 送信キューに入れる
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn touch_task(mut i2c: I2c<'static, I2C1, i2c::Async>) {
    use core::cell::Cell;
    use touch::qtouch::QubitTouch;
    use touch_algo::{KeyState, Params, TouchSignal};

    // 初期化は失敗しても全チャネルを続けるので、すべての I2C がタイムアウト（3ms）しても終わる時間にする。
    // 96 キーで 16ch ×（切替＋初期化）＋ 4 台の切断 = 約 108ms
    const TOUCH_INIT_TIMEOUT_MS: u64 = 150;

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
        // デバッグ用: PC に Note のイベントを送る
        #[cfg(feature = "debug_stream")]
        queue_note_event(status, note, velocity, _location);
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

    // スキャン周期。debug_stream では PC のコマンドで変わるので、毎周期確かめる
    let mut period_us = scan_period_us();
    let mut period = Duration::from_micros(period_us as u64);
    let mut divider = analysis_divider(period_us);
    let mut ticker = Ticker::every(period);
    let mut frame = 0u32;
    // 信号処理（ノイズ除去・基準値。doc/touch_baseline.md §3）。タスクの future は静的に確保されるので、
    // 96 キー分（数 KB）でもスタックは使わない
    let mut signal = TouchSignal::new([KeyState::INITIAL; TOTAL_QT_KEYS], Params::DEFAULT);
    // 起動直後はチップが自身を校正しているので、この時刻までは信号処理にも解析にも渡さない
    let analysis_start_at = Instant::now() + Duration::from_millis(TOUCH_STARTUP_SETTLE_MS);

    // Task Loop
    loop {
        ticker.next().await;
        let cycle_start = Instant::now();
        if scan_period_us() != period_us {
            // 周期が変わったら Ticker を作り直し、解析の間引きも計算し直す
            period_us = scan_period_us();
            period = Duration::from_micros(period_us as u64);
            divider = analysis_divider(period_us);
            ticker = Ticker::every(period);
        }

        // タッチセンサのスキャン
        read_touch
            .touch_sensor_scan(&pca, &mut at42, &mut i2c)
            .await;
        SCAN_TIME.record(cycle_start.elapsed().as_micros() as u32);

        // デバッグ用: 生値を PC への送信キューに入れる（PC が start を送った後だけ）
        #[cfg(feature = "debug_stream")]
        queue_debug_frame(&read_touch, frame, cycle_start);

        // 信号処理と解析: QubitTouch と touch_algo のパラメータは 10ms 毎（8〜10ms）に呼ばれる前提なので、
        // divider フレームに 1 回行う（20ms 周期では毎回）。起動直後の TOUCH_STARTUP_SETTLE_MS の間は行わない
        if cycle_start >= analysis_start_at && frame.is_multiple_of(divider) {
            let analysis_start = Instant::now();
            let work_mode = WORK_MODE
                .load(Ordering::Relaxed)
                .try_into()
                .unwrap_or(WorkMode::Piano);
            current_mode.set(work_mode);

            // 設定画面にいる間は校正（基準値の速い追従と noise_floor の計測）
            let (raw, valid) = read_touch.raw();
            signal.process(
                cycle_start.as_micros() as u32,
                raw,
                valid,
                SETTING_MODE.load(Ordering::Relaxed),
            );
            let keys = signal.keys();
            for (ch, key) in keys.iter().enumerate() {
                qt.set_value(ch, key.output());
            }
            // OLED の表示用: キー 0〜5 の output
            for (point, key) in [&POINT0, &POINT1, &POINT2, &POINT3, &POINT4, &POINT5]
                .into_iter()
                .zip(keys)
            {
                point.store(key.output(), Ordering::Relaxed);
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

/// 最後のスキャンの生値を DEBUG_FRAMES に入れる（待たない）。あふれたら捨てて数える
#[cfg(feature = "debug_stream")]
fn queue_debug_frame(read_touch: &touch::read_touch::ReadTouch, seq: u32, scan_start: Instant) {
    use crate::shared::{DEBUG_DROPPED, DEBUG_FRAMES, DEBUG_STREAMING, DebugFrame};

    if !DEBUG_STREAMING.load(Ordering::Relaxed) {
        return;
    }
    let (raw, valid) = read_touch.raw();
    let debug_frame = DebugFrame {
        seq,
        time_us: scan_start.as_micros() as u32,
        valid,
        raw: *raw,
    };
    if DEBUG_FRAMES.try_send(debug_frame).is_err() {
        DEBUG_DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Note On/Off/Moved を DEBUG_EVENTS に入れる（PC がポートを開いている間だけ。待たない）
/// status は RINGLED_CMD_TX_ON / OFF / MOVED、location はキー位置
#[cfg(feature = "debug_stream")]
fn queue_note_event(status: u8, note: u8, velocity: u8, location: f32) {
    use crate::tasks::debug_stream::{
        EVENT_NOTE_MOVED, EVENT_NOTE_OFF, EVENT_NOTE_ON, queue_event,
    };

    let kind = match status {
        RINGLED_CMD_TX_ON => EVENT_NOTE_ON,
        RINGLED_CMD_TX_OFF => EVENT_NOTE_OFF,
        RINGLED_CMD_TX_MOVED => EVENT_NOTE_MOVED,
        _ => return,
    };
    let location = (location * 100.0) as u16; // TOUCH0-3 と同じ ×100 の単位
    let [lo, hi] = location.to_le_bytes();
    queue_event(kind, [note, velocity, lo, hi]);
}
