use embassy_rp::gpio::Input;
use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::I2C0;
use embassy_time::{Duration, Timer, with_timeout};
use portable_atomic::Ordering;

use crate::devices::ssd1306::{Oled, OledBuffer};
use crate::error::ERROR_CODE;
use crate::shared::{RINGLED_RX_BITS, WORK_MODE, WORK_MODE_DISPLAY};
use crate::ui;

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
    use ui::oled_display::GraphicsDisplay;

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
        ERROR_CODE.store(51, Ordering::Relaxed);
    }

    // 初期画面表示
    gui.draw_bringup_screen(&mut buffer);
    flush(&oled, &buffer, &mut i2c).await;

    loop {
        // 次のステップまで待機(10fps想定)
        Timer::after_millis(100).await;

        // スイッチの状態を取得
        let switch_r_state = switch1.is_low();
        let switch_l_state = switch2.is_low();
        if (switch_r_state != switch1_prev) && switch_r_state {
            if switch_l_state {
                // 両方のスイッチが同時に押された場合は、設定画面に直接遷移
                ui_page = 4;
                // 設定変更時にエラーコードをリセットする
                ERROR_CODE.store(0, Ordering::Relaxed);
                WORK_MODE_DISPLAY.store(true, Ordering::Relaxed);
            } else if ui_page == 3 || ui_page == 4 {
                ui_page = 0;
                WORK_MODE_DISPLAY.store(false, Ordering::Relaxed);
            } else {
                ui_page += 1;
            }
            gui.change_page(ui_page); // ページ切替をGUIに通知
        }
        if (switch_l_state != switch2_prev) && switch_l_state {
            if switch_r_state {
                // 両方のスイッチが同時に押された場合は、設定画面に直接遷移
                ui_page = 4;
                // 設定変更時にエラーコードをリセットする
                ERROR_CODE.store(0, Ordering::Relaxed);
                WORK_MODE_DISPLAY.store(true, Ordering::Relaxed);
            } else if ui_page == 4 {
                WORK_MODE.store(
                    (WORK_MODE.load(Ordering::Relaxed) + 1) % 2,
                    Ordering::Relaxed,
                ); // 動作モードを切り替え
                // 受信Note On表示を全キャンセル
                for word in RINGLED_RX_BITS.iter() {
                    word.store(0, Ordering::Relaxed);
                }
            } else if ui_page == 0 {
                ui_page = 3;
            } else {
                ui_page -= 1;
            }
            gui.change_page(ui_page); // ページ切替をGUIに通知
        }
        switch1_prev = switch_r_state;
        switch2_prev = switch_l_state;

        // 描画
        gui.tick(&mut buffer, counter);
        counter = counter.wrapping_add(1);

        // 描画済みバッファを転送
        flush(&oled, &buffer, &mut i2c).await;
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
        ERROR_CODE.store(52, Ordering::Relaxed);
    }
}
