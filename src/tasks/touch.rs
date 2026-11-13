use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_time::{Duration, Instant, Ticker, with_timeout};
use portable_atomic::Ordering;

use crate::constants::*;
use crate::devices;
use crate::error;
use crate::shared::{
    ANALYSIS_TIME, ANY_TOUCH, DEBUG_VALUE, PERIOD_OVERRUN, POINT0, POINT1, POINT2, POINT3, POINT4,
    POINT5, SCAN_TIME, SETTING_MODE, TOUCH_CALIBRATING, TOUCH0, TOUCH1, TOUCH2, TOUCH3,
    scan_period_us,
};
use crate::tasks::midi::queue_midi;
use crate::touch;
use touch_algo::playinfo::STATUS_NOTE_MOVED;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Touch Task (Core1): I2C1 を専有し、スキャン → 信号処理（touch_algo）→ 解析 →
//      ノートイベントの生成を scan_period_us() 毎に行う。ノートイベントは MIDI 送信キューに入れる
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn touch_task(mut i2c: I2c<'static, I2C1, i2c::Async>) {
    use touch_algo::electrode::{ElectrodeSignal, ElectrodeState, Params, State};
    use touch_algo::playinfo::PlayInfo;
    use touch_algo::touch::TouchTracker;

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

    // タッチの解析は 2 つの層: depth・position（TouchTracker）→ 演奏情報（PlayInfo）。doc/touch_analysis.md §5.6
    let mut tracker = TouchTracker::new([0u16; TOTAL_QT_KEYS], TOUCH_RING);
    let mut play = PlayInfo::new(|status, note, velocity, _location| {
        // MIDIコールバック: タッチイベントをMIDIパケットに変換して送信キューに入れる
        let status = status & 0xf0; // コマンド部分
        // デバッグ用: PC に Note のイベントを送る
        #[cfg(feature = "debug_stream")]
        queue_note_event(status, note, velocity, _location);
        // 動作モードは無い（2026-11-13）。ノートは MIDI_CH_VIOLIN で送る
        let status = if status == STATUS_NOTE_MOVED {
            0x80 | MIDI_CH_VIOLIN // 移動イベントはNote Offとして扱う
        } else {
            status | MIDI_CH_VIOLIN
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
    let mut signal =
        ElectrodeSignal::new([ElectrodeState::INITIAL; TOTAL_QT_KEYS], Params::DEFAULT);
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

        // 信号処理と解析: TouchTracker・PlayInfo と touch_algo のパラメータは 10ms 毎（8〜10ms）に呼ばれる前提なので、
        // divider フレームに 1 回行う（20ms 周期では毎回）。起動直後の TOUCH_STARTUP_SETTLE_MS の間は行わない
        if cycle_start >= analysis_start_at && frame.is_multiple_of(divider) {
            let analysis_start = Instant::now();
            // 設定画面にいる間は校正（基準値の速い追従と noise_floor の計測）
            let (raw, valid) = read_touch.raw();
            signal.process(
                cycle_start.as_micros() as u32,
                raw,
                valid,
                SETTING_MODE.load(Ordering::Relaxed),
            );
            // 起動時の校正（初期取得・noise_floor の自動計測）が終わったら、OLED の表示を消す
            if matches!(signal.state(), State::Running | State::Calibrating) {
                TOUCH_CALIBRATING.store(false, Ordering::Relaxed);
            }
            let keys = signal.keys();
            for (ch, key) in keys.iter().enumerate() {
                tracker.set_value(ch, key.output());
            }
            // OLED の表示用: キー 0〜5 の output
            for (point, key) in [&POINT0, &POINT1, &POINT2, &POINT3, &POINT4, &POINT5]
                .into_iter()
                .zip(keys)
            {
                point.store(key.output(), Ordering::Relaxed);
            }
            tracker.update();
            play.update(&tracker);
            publish_touch_state(&tracker, &play);

            tracker.lighten_leds(|_location, _intensity| {
                // LEDの明るさをタッチの強さに応じて変化させる
                //WHITE_LEVEL.store(intensity as u8, Ordering::Relaxed);
            });
            ANALYSIS_TIME.record(analysis_start.elapsed().as_micros() as u32);
        }
        frame = frame.wrapping_add(1);

        // 周期を超えたときは、遅れた周期を取り戻さずに捨てる。
        // Ticker は期限を過ぎていると待たずに連続して完了するため、そのままだと
        // タッチの解析がほぼ 0ms 間隔で呼ばれ、時間の計算が狂う
        if cycle_start.elapsed() > period {
            PERIOD_OVERRUN.fetch_add(1, Ordering::Relaxed);
            ticker.reset();
        }
    }
}

/// 解析の結果を共有状態に書く: タッチ位置 ×100（未タッチは 10000）→ ringled_task・OLED、
/// 触れているか → pressure_task、ビブラート → OLED（page 1。触れていないときは 0）
fn publish_touch_state<F, P>(
    tracker: &touch_algo::touch::TouchTracker<P>,
    play: &touch_algo::playinfo::PlayInfo<F>,
) where
    F: Fn(u8, u8, u8, f32),
    P: AsRef<[u16]> + AsMut<[u16]>,
{
    for (id, slot) in [&TOUCH0, &TOUCH1, &TOUCH2, &TOUCH3].into_iter().enumerate() {
        if let Some(tp) = tracker.touch_point(id) {
            // 触れていないときの位置は 100.0 なので 10000 になる
            slot.store((tp.get_location() * 100.0) as i32, Ordering::Relaxed);
        }
    }
    let touched = tracker.is_any_touched();
    ANY_TOUCH.store(touched, Ordering::Relaxed);
    let vibrato = if touched { play.vibrato() } else { 0 };
    DEBUG_VALUE.store(vibrato as u32, Ordering::Relaxed);
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
/// status は STATUS_NOTE_ON / OFF / MOVED、location はキー位置
#[cfg(feature = "debug_stream")]
fn queue_note_event(status: u8, note: u8, velocity: u8, location: f32) {
    use crate::tasks::debug_stream::{
        EVENT_NOTE_MOVED, EVENT_NOTE_OFF, EVENT_NOTE_ON, queue_event,
    };
    use touch_algo::playinfo::{STATUS_NOTE_OFF, STATUS_NOTE_ON};

    let kind = match status {
        STATUS_NOTE_ON => EVENT_NOTE_ON,
        STATUS_NOTE_OFF => EVENT_NOTE_OFF,
        STATUS_NOTE_MOVED => EVENT_NOTE_MOVED,
        _ => return,
    };
    let location = (location * 100.0) as u16; // TOUCH0-3 と同じ ×100 の単位
    let [lo, hi] = location.to_le_bytes();
    queue_event(kind, [note, velocity, lo, hi]);
}
