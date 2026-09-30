//! qubit_monitor: Loopian::QUBIT のタッチ信号を受信・表示・記録する PC アプリ（doc/debug_env.md §4）
//!
//! 実行: `cd tools/qubit_monitor && cargo run --release`
//! （リポジトリ直下から --manifest-path で指定すると、組み込み向けのターゲットが効いて失敗する）
mod app;

use std::sync::Arc;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 860.0])
            .with_title("qubit_monitor"),
        ..Default::default()
    };
    eframe::run_native(
        "qubit_monitor",
        options,
        Box::new(|cc| {
            setup_japanese_font(&cc.egui_ctx);
            Ok(Box::new(app::MonitorApp::new()))
        }),
    )
}

/// egui の既定のフォントには日本語が無いので、OS のフォントを後ろに足す（見つからなければ何もしない）
fn setup_japanese_font(ctx: &egui::Context) {
    const CANDIDATES: [&str; 3] = [
        "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", // macOS
        "/System/Library/Fonts/Hiragino Sans GB.ttc",      // macOS
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", // Linux (Debian 系)
    ];
    let Some(bytes) = CANDIDATES.iter().find_map(|path| std::fs::read(path).ok()) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "japanese".into(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("japanese".into());
    }
    ctx.set_fonts(fonts);
}
