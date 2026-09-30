use embassy_rp::gpio::Input;
use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C0;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use portable_atomic::Ordering;

use crate::devices::ssd1306::{Oled, OledBuffer};
use crate::error;
use crate::shared::{RINGLED_RX_BITS, SETTING_MODE, UI_DRAW_TIME, WORK_MODE, reset_diagnostics};
use crate::ui;

// スイッチの判定周期。短い押下を取りこぼさないよう、描画より短くする
const SWITCH_POLL_MS: u64 = 100;
// OLED の描画・転送は SWITCH_POLL_MS の DRAW_DIVIDER 回に 1 回（5fps）。
// 描画は await を挟まない CPU 処理（約3ms）で、その間 Core0 の他のタスク（MIDI 送信・RingLED）が待たされるため、頻度を下げる
const DRAW_DIVIDER: u32 = 2;
const OLED_POWER_ON_WAIT_MS: u64 = 100;
const OLED_INIT_TIMEOUT_MS: u64 = 50;
// 1画面(1024バイト)の転送は 400kHz で約25ms かかる。固着したときに UI が止まらないよう余裕を持たせて打ち切る
const OLED_FLUSH_TIMEOUT_MS: u64 = 50;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      UI Task (Core0): スイッチ判定・ページ／モード切替・OLED の描画と I2C0 への転送
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn ui_task(
    mut i2c: I2c<'static, I2C0, i2c::Async>,
    switch1: Input<'static>,
    switch2: Input<'static>,
) {
    use ui::oled_display::{GraphicsDisplay, SETTING_PAGE, next_page, prev_page};

    let mut oled = Oled::new();
    let mut buffer = OledBuffer::new();
    let mut gui = GraphicsDisplay::new();
    let mut counter = 0u32;
    let mut ui_page = 0u8;

    let mut switch1_prev = false;
    let mut switch2_prev = false;

    // OLED初期化
    // 以前はタッチセンサの初期化の後に行っていたが、I2C を分けたことで起動直後に並行して走る。
    // 電源投入直後の OLED が初期化コマンドを受け付けられるよう、少し待ってから始める
    Timer::after_millis(OLED_POWER_ON_WAIT_MS).await;
    let oled_init_result = with_timeout(
        Duration::from_millis(OLED_INIT_TIMEOUT_MS),
        oled.init(&mut i2c),
    )
    .await;
    if !matches!(oled_init_result, Ok(Ok(()))) {
        error::set(error::OLED_INIT);
    }

    // 初期画面表示
    gui.draw_bringup_screen(&mut buffer);
    flush(&oled, &buffer, &mut i2c).await;

    loop {
        // 次のステップまで待機
        Timer::after_millis(SWITCH_POLL_MS).await;
        let mut page_changed = false;

        // スイッチの状態を取得
        let switch_r_state = switch1.is_low();
        let switch_l_state = switch2.is_low();
        let both_pressed = switch_r_state && switch_l_state;
        let enter_setting = |ui_page: &mut u8| {
            // 両方のスイッチが同時に押された場合は、設定画面に直接遷移
            *ui_page = SETTING_PAGE;
            // 設定変更時にエラーコードと診断値をリセットする
            error::clear();
            reset_diagnostics();
            SETTING_MODE.store(true, Ordering::Relaxed);
        };
        if (switch_r_state != switch1_prev) && switch_r_state {
            if both_pressed {
                enter_setting(&mut ui_page);
            } else if ui_page == SETTING_PAGE {
                // 設定画面を抜ける
                ui_page = 0;
                SETTING_MODE.store(false, Ordering::Relaxed);
            } else {
                ui_page = next_page(ui_page);
            }
            gui.change_page(ui_page); // ページ切替をGUIに通知
            page_changed = true;
        }
        if (switch_l_state != switch2_prev) && switch_l_state {
            if both_pressed {
                enter_setting(&mut ui_page);
            } else if ui_page == SETTING_PAGE {
                WORK_MODE.store(
                    (WORK_MODE.load(Ordering::Relaxed) + 1) % 2,
                    Ordering::Relaxed,
                ); // 動作モードを切り替え
                // 受信Note On表示を全キャンセル
                for word in RINGLED_RX_BITS.iter() {
                    word.store(0, Ordering::Relaxed);
                }
            } else {
                ui_page = prev_page(ui_page);
            }
            gui.change_page(ui_page); // ページ切替をGUIに通知
            page_changed = true;
        }
        switch1_prev = switch_r_state;
        switch2_prev = switch_l_state;

        // 描画と転送は DRAW_DIVIDER 回に 1 回。スイッチ操作でページや表示が変わったときは、反応を遅らせないようすぐに描く。
        // counter は SWITCH_POLL_MS 毎に進めるので、点滅などの表示の時間は描画の頻度に関係しない
        if page_changed || counter.is_multiple_of(DRAW_DIVIDER) {
            let draw_start = Instant::now();
            gui.tick(&mut buffer, counter);
            UI_DRAW_TIME.record(draw_start.elapsed().as_micros() as u32);

            // 描画済みバッファを転送
            flush(&oled, &buffer, &mut i2c).await;
        }
        counter = counter.wrapping_add(1);
    }
}

/// バッファを OLED に転送する。失敗・タイムアウトはエラーコードに記録する
async fn flush(oled: &Oled, buffer: &OledBuffer, i2c: &mut I2c<'static, I2C0, i2c::Async>) {
    let result = with_timeout(
        Duration::from_millis(OLED_FLUSH_TIMEOUT_MS),
        oled.flush_buffer(buffer, i2c),
    )
    .await;
    if !matches!(result, Ok(Ok(()))) {
        error::set(error::OLED_FLUSH);
    }
}
