use embassy_rp::gpio::Input;
use embassy_time::Timer;
use portable_atomic::Ordering;

use crate::error::ERROR_CODE;
use crate::shared::{
    BUFFER_FROM_DISPLAY, BUFFER_TO_DISPLAY, RINGLED_RX_BITS, WORK_MODE, WORK_MODE_DISPLAY,
};
use crate::ui;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Core1 OLED UI Task: OLEDディスプレイの更新
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn core1_oled_ui_task(switch1: Input<'static>, switch2: Input<'static>) {
    use ui::oled_display::GraphicsDisplay;

    let mut gui = GraphicsDisplay::new();
    let mut counter = 0u32;
    let mut ui_page = 0u8;

    let mut switch1_prev = false;
    let mut switch2_prev = false;

    // 初期画面表示
    let mut buffer = BUFFER_FROM_DISPLAY.receive().await;
    gui.draw_bringup_screen(&mut buffer);
    BUFFER_TO_DISPLAY.send(buffer).await;

    loop {
        // 次のステップまで待機(10fps想定)
        Timer::after_millis(100).await;

        // 空バッファを受信
        buffer = BUFFER_FROM_DISPLAY.receive().await;

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

        // 描画済みバッファを送信
        BUFFER_TO_DISPLAY.send(buffer).await;
    }
}
