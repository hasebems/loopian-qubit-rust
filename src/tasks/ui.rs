use embassy_rp::peripherals::PIO1;
use embassy_time::{Duration, Timer, with_timeout};
use portable_atomic::Ordering;

use crate::devices::pcf8574::Pcf8574;
use crate::devices::pio_i2c::PioI2c;
use crate::devices::ssd1306::{Oled, OledBuffer};
use crate::error;
use crate::shared::{SETTING_MODE, UI_BUS_RECOVERY_COUNT, UI_DRAW_TIME, reset_diagnostics};
use crate::ui;

/// OLED と PCF8574 のバス（PIO1 の SM0。doc/hw96_firmware261114.md §3）
pub type UiI2c = PioI2c<'static, PIO1, 0>;

// スイッチの判定周期。短い押下を取りこぼさないよう、描画より短くする
const SWITCH_POLL_MS: u64 = 100;
// OLED の描画・転送は SWITCH_POLL_MS の DRAW_DIVIDER 回に 1 回（5fps）。
// 描画は await を挟まない CPU 処理（約3ms）で、その間 Core0 の他のタスク（MIDI 送信・RingLED）が待たされるため、頻度を下げる
const DRAW_DIVIDER: u32 = 2;
const OLED_POWER_ON_WAIT_MS: u64 = 100;
const OLED_INIT_TIMEOUT_MS: u64 = 50;
// 1画面(1024バイト)の転送は 400kHz で約25ms かかる。固着したときに UI が止まらないよう余裕を持たせて打ち切る
const OLED_FLUSH_TIMEOUT_MS: u64 = 50;
// PCF8574 の 1 回の読み込み・書き込みは 400kHz で約 0.05ms。バスの復帰（約 0.2ms）を含めても収まるように打ち切る
const SWITCH_READ_TIMEOUT_MS: u64 = 5;
// スイッチをつないだ PCF8574 のピン。Low（0）で押下（doc/hw_modify.md §8.2）
const SWITCH_L_BIT: u8 = 6;
const SWITCH_R_BIT: u8 = 7;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      UI Task (Core0): スイッチ判定・ページ／モード切替・OLED の描画と転送（OLED とスイッチは PIO の I2C）
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn ui_task(mut i2c: UiI2c) {
    use ui::oled_display::{DrawPacer, GraphicsDisplay, SETTING_PAGE, next_page, prev_page};

    let mut oled = Oled::new();
    let expander = Pcf8574::new();
    let mut buffer = OledBuffer::new();
    let mut gui = GraphicsDisplay::new();
    let mut counter = 0u32;
    let mut ui_page = 0u8;

    let mut switch_r_prev = false;
    let mut switch_l_prev = false;

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

    // スイッチの PCF8574 を初期化（OLED と同じバス）
    let expander_init_result = with_timeout(
        Duration::from_millis(SWITCH_READ_TIMEOUT_MS),
        expander.init(&mut i2c),
    )
    .await;
    if !matches!(expander_init_result, Ok(Ok(()))) {
        error::set(error::SWITCH_READ);
    }

    // 初期画面表示
    gui.draw_bringup_screen(&mut buffer, 0, &mut DrawPacer::new())
        .await;
    flush(&oled, &buffer, &mut i2c).await;

    loop {
        // 次のステップまで待機
        Timer::after_millis(SWITCH_POLL_MS).await;
        let mut page_changed = false;

        // スイッチの状態を取得。読めなかった回は前回のままにする
        // （「押されていない」にすると、押し続けているスイッチが、読めるようになった回に押し直したように見えるため）
        let (switch_r_state, switch_l_state) = match read_switches(&expander, &mut i2c).await {
            Some(bits) => (
                bits & (1 << SWITCH_R_BIT) == 0,
                bits & (1 << SWITCH_L_BIT) == 0,
            ),
            None => (switch_r_prev, switch_l_prev),
        };
        let both_pressed = switch_r_state && switch_l_state;
        let enter_setting = |ui_page: &mut u8| {
            // 両方のスイッチが同時に押された場合は、設定画面に直接遷移
            *ui_page = SETTING_PAGE;
            // 設定変更時にエラーコードと診断値をリセットする
            error::clear();
            reset_diagnostics();
            SETTING_MODE.store(true, Ordering::Relaxed);
        };
        if (switch_r_state != switch_r_prev) && switch_r_state {
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
        if (switch_l_state != switch_l_prev) && switch_l_state {
            if both_pressed {
                enter_setting(&mut ui_page);
            } else if ui_page == SETTING_PAGE {
                // 設定画面では何もしない（動作モードの切替は 2026-11-13 に無くした）
            } else {
                ui_page = prev_page(ui_page);
            }
            gui.change_page(ui_page); // ページ切替をGUIに通知
            page_changed = true;
        }
        switch_r_prev = switch_r_state;
        switch_l_prev = switch_l_state;

        // 描画と転送は DRAW_DIVIDER 回に 1 回。スイッチ操作でページや表示が変わったときは、反応を遅らせないようすぐに描く。
        // counter は SWITCH_POLL_MS 毎に進めるので、点滅などの表示の時間は描画の頻度に関係しない
        if page_changed || counter.is_multiple_of(DRAW_DIVIDER) {
            // 描画はテキスト 1 行・図形 1 つ毎に他のタスクに順番を譲る。
            // UI_DRAW_TIME には、譲らずに走った最長の区間（1 回に Core0 を止めた時間）を記録する
            let mut pacer = DrawPacer::new();
            gui.tick(&mut buffer, counter, &mut pacer).await;
            UI_DRAW_TIME.record(pacer.finish());

            // 描画済みバッファを転送
            flush(&oled, &buffer, &mut i2c).await;
        }
        UI_BUS_RECOVERY_COUNT.fetch_add(i2c.take_recovery_count(), Ordering::Relaxed);
        counter = counter.wrapping_add(1);
    }
}

/// スイッチの PCF8574 を読む。失敗・タイムアウトはエラーコードに記録して None
async fn read_switches(expander: &Pcf8574, i2c: &mut UiI2c) -> Option<u8> {
    match with_timeout(
        Duration::from_millis(SWITCH_READ_TIMEOUT_MS),
        expander.read(i2c),
    )
    .await
    {
        Ok(Ok(bits)) => Some(bits),
        _ => {
            error::set(error::SWITCH_READ);
            None
        }
    }
}

/// バッファを OLED に転送する。失敗・タイムアウトはエラーコードに記録する
async fn flush(oled: &Oled, buffer: &OledBuffer, i2c: &mut UiI2c) {
    let result = with_timeout(
        Duration::from_millis(OLED_FLUSH_TIMEOUT_MS),
        oled.flush_buffer(buffer, i2c),
    )
    .await;
    if !matches!(result, Ok(Ok(()))) {
        error::set(error::OLED_FLUSH);
    }
}
