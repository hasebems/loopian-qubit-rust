use crate::constants::*;
use core::fmt::Write;
use embassy_time::Instant;
use embedded_graphics::image::{Image, ImageRaw};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_10X20};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Styled;
use embedded_graphics::primitives::{
    Circle, Line, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle, Triangle,
};
use embedded_graphics::text::Text;
use heapless::String;

use crate::devices::ssd1306::OledBuffer;
use crate::error;
use crate::shared::{
    AD_VALUE0, AD_VALUE1, AD_VALUE2, AD_VALUE3, ANALYSIS_TIME, DEBUG_VALUE, MIDI_TX_MAX_USED,
    MIDI_TX_OVERFLOW, PERIOD_OVERRUN, POINT0, POINT1, POINT2, POINT3, POINT4, POINT5, PRESSURE,
    SCAN_TIME, TOUCH0, TOUCH1, TOUCH2, TOUCH3, UI_DRAW_TIME, WORK_MODE,
};

/// 設定画面のページ番号
pub const SETTING_PAGE: u8 = 4;
/// 通常時に左右スイッチで巡回するページ（5: 診断ページ）
const NORMAL_PAGES: [u8; 5] = [0, 1, 2, 3, 5];

/// 右スイッチで進む次のページ
pub fn next_page(page: u8) -> u8 {
    let idx = NORMAL_PAGES.iter().position(|&p| p == page).unwrap_or(0);
    NORMAL_PAGES[(idx + 1) % NORMAL_PAGES.len()]
}

/// 左スイッチで戻る前のページ
pub fn prev_page(page: u8) -> u8 {
    let idx = NORMAL_PAGES.iter().position(|&p| p == page).unwrap_or(0);
    NORMAL_PAGES[(idx + NORMAL_PAGES.len() - 1) % NORMAL_PAGES.len()]
}

/// 描画の途中で他のタスクに順番を譲り、譲らずに走った区間のうち最長の時間を測る
/// （doc/task_architecture.md §4.4.1）。
/// 描画は await の無い CPU 処理で、その間 Core0 の他のタスク（MIDI 送信・RingLED）が止まるため、
/// テキスト 1 行・図形 1 つ毎に譲って、1 回に止める時間を短くする
pub struct DrawPacer {
    segment_start: Instant,
    longest_us: u32,
}

impl DrawPacer {
    pub fn new() -> Self {
        Self {
            segment_start: Instant::now(),
            longest_us: 0,
        }
    }

    /// ここまでの区間の時間を測り、他のタスクに順番を譲る
    pub async fn yield_now(&mut self) {
        self.end_segment();
        embassy_futures::yield_now().await;
        self.segment_start = Instant::now();
    }

    fn end_segment(&mut self) {
        let us = self.segment_start.elapsed().as_micros() as u32;
        self.longest_us = self.longest_us.max(us);
    }

    /// 描画の終わりに呼ぶ。最後の区間も含めた、最長の区間の時間（us）を返す
    pub fn finish(mut self) -> u32 {
        self.end_segment();
        self.longest_us
    }
}

/// 1 つ描いてから、他のタスクに順番を譲る
async fn draw<D>(buffer: &mut OledBuffer, pacer: &mut DrawPacer, item: &D)
where
    D: Drawable<Color = BinaryColor>,
{
    let _ = item.draw(buffer);
    pacer.yield_now().await;
}

/// 画面の外枠（128x64）
fn outline_frame() -> Styled<Rectangle, PrimitiveStyle<BinaryColor>> {
    Rectangle::new(Point::new(0, 0), Size::new(128, 64))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
}

pub struct GraphicsDisplay {
    page: u8,
    step: u8,
    anim_x: u8,
}

impl GraphicsDisplay {
    pub fn new() -> Self {
        Self {
            page: 0,
            step: 0,
            anim_x: 0,
        }
    }

    pub fn change_page(&mut self, page: u8) {
        self.page = page; // 14 demo pages
    }

    pub async fn draw_bringup_screen(&self, buffer: &mut OledBuffer, pacer: &mut DrawPacer) {
        buffer.clear();
        draw(buffer, pacer, &outline_frame()).await;

        let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
        let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
        draw(
            buffer,
            pacer,
            &Text::new("Loopian::", Point::new(5, 20), style_small),
        )
        .await;
        draw(
            buffer,
            pacer,
            &Text::new("QUBIT", Point::new(64, 20), style_big),
        )
        .await;
        let build_date = concat!("build: ", env!("BUILD_DATE"));
        draw(
            buffer,
            pacer,
            &Text::new(build_date, Point::new(30, 44), style_small),
        )
        .await;
        let version = concat!("       ", env!("BUILD_VERSION"));
        draw(
            buffer,
            pacer,
            &Text::new(version, Point::new(30, 56), style_small),
        )
        .await;
    }

    /// 今のページを描く。テキスト 1 行・図形 1 つ毎に他のタスクに順番を譲る
    pub async fn tick(&mut self, buffer: &mut OledBuffer, counter: u32, pacer: &mut DrawPacer) {
        match self.page {
            0 => self.draw_bringup_screen(buffer, pacer).await,
            1 => display1(buffer, counter, pacer).await,
            2 => display2(buffer, pacer).await,
            3 => display3(buffer, pacer).await,
            4 => display4(buffer, counter, pacer).await,
            5 => display_diag(buffer, pacer).await,
            _ => {
                // デモのページは開発用なので同期のまま描き、描く前に 1 回だけ譲る
                pacer.yield_now().await;
                self.tick_demo(buffer);
            }
        }
    }

    fn tick_demo(&mut self, buffer: &mut OledBuffer) {
        match self.page {
            10 => demo_lines(buffer),
            11 => demo_rects(buffer),
            12 => demo_filled_rects(buffer),
            13 => demo_circles(buffer),
            14 => demo_filled_circles(buffer),
            15 => demo_round_rects(buffer),
            16 => demo_filled_round_rects(buffer),
            17 => demo_triangles(buffer),
            18 => demo_filled_triangles(buffer),
            19 => demo_text(buffer),
            20 => demo_styles(buffer),
            21 =>
            // scroll/invert are intentionally omitted.
            {
                demo_bitmap(buffer)
            }
            22 => {
                let done = demo_animate_frame(buffer, self.anim_x);
                if done {
                    self.anim_x = 0;
                    self.step = 0;
                } else {
                    self.anim_x = self.anim_x.saturating_add(6);
                }
            }
            _ => (),
        }
    }
}

async fn display1(buffer: &mut OledBuffer, counter: u32, pacer: &mut DrawPacer) {
    buffer.clear();
    draw(buffer, pacer, &outline_frame()).await;

    //let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);

    let mut text1: String<32> = String::new();
    let _ = write!(text1, "Cntr: {}", counter);
    draw(
        buffer,
        pacer,
        &Text::new(&text1, Point::new(6, 12), style_small),
    )
    .await;

    for (number, ad_value) in [&AD_VALUE0, &AD_VALUE1, &AD_VALUE2, &AD_VALUE3]
        .iter()
        .enumerate()
    {
        let value = ad_value.load(core::sync::atomic::Ordering::Relaxed);
        draw_bar(buffer, number as i32, value);
        pacer.yield_now().await;
    }

    text1.clear();
    let _ = write!(text1, "Prs:");
    draw(
        buffer,
        pacer,
        &Text::new(&text1, Point::new(6, 24), style_small),
    )
    .await;

    let pressure = PRESSURE.load(core::sync::atomic::Ordering::Relaxed);
    text1.clear();
    let _ = write!(text1, "Calc.Prs: {:>6}", pressure);
    draw(
        buffer,
        pacer,
        &Text::new(&text1, Point::new(6, 40), style_small),
    )
    .await;

    let vib = DEBUG_VALUE.load(core::sync::atomic::Ordering::Relaxed);
    text1.clear();
    let _ = write!(text1, "Vibrato: {}", vib);
    draw(
        buffer,
        pacer,
        &Text::new(&text1, Point::new(6, 52), style_small),
    )
    .await;
}

async fn display2(buffer: &mut OledBuffer, pacer: &mut DrawPacer) {
    buffer.clear();
    draw(buffer, pacer, &outline_frame()).await;

    //let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);

    let points = [&POINT0, &POINT1, &POINT2, &POINT3, &POINT4, &POINT5];
    let mut text1: String<32> = String::new();
    for (i, point) in points.iter().enumerate() {
        let value = point.load(core::sync::atomic::Ordering::Relaxed);
        text1.clear();
        let _ = write!(text1, "Point{}: {}", i, value);
        let y = 10 + 10 * i as i32;
        draw(
            buffer,
            pacer,
            &Text::new(&text1, Point::new(6, y), style_small),
        )
        .await;
    }
}

async fn display3(buffer: &mut OledBuffer, pacer: &mut DrawPacer) {
    buffer.clear();
    draw(buffer, pacer, &outline_frame()).await;

    //let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);

    let touches = [&TOUCH0, &TOUCH1, &TOUCH2, &TOUCH3];
    let mut text1: String<32> = String::new();
    for (i, touch) in touches.iter().enumerate() {
        let value = touch.load(core::sync::atomic::Ordering::Relaxed);
        text1.clear();
        if (0..10000).contains(&value) {
            let _ = write!(text1, "Touch{}: {}", i + 1, value);
        } else {
            let _ = write!(text1, "Touch{}: ---", i + 1);
        }
        let y = 12 + 12 * i as i32;
        draw(
            buffer,
            pacer,
            &Text::new(&text1, Point::new(6, y), style_small),
        )
        .await;
    }
}

/// 診断ページ: 処理時間（us, 最小/平均/最大）と周期超過・MIDI 送信キューの状況
/// 最小・最大と回数は、設定画面に入ったときにリセットされる。
/// Drw は描画 1 回のうち、譲らずに走った最長の区間（1 回に Core0 を止めた時間）
async fn display_diag(buffer: &mut OledBuffer, pacer: &mut DrawPacer) {
    use core::sync::atomic::Ordering;
    buffer.clear();

    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let mut text: String<32> = String::new();

    let _ = write!(text, "us  min/avg/max");
    draw(
        buffer,
        pacer,
        &Text::new(&text, Point::new(0, 8), style_small),
    )
    .await;

    let (min, avg, max) = SCAN_TIME.get();
    text.clear();
    let _ = write!(text, "Scn{:>5}/{:>5}/{:>5}", min, avg, max);
    draw(
        buffer,
        pacer,
        &Text::new(&text, Point::new(0, 18), style_small),
    )
    .await;

    let (min, avg, max) = ANALYSIS_TIME.get();
    text.clear();
    let _ = write!(text, "Anl{:>5}/{:>5}/{:>5}", min, avg, max);
    draw(
        buffer,
        pacer,
        &Text::new(&text, Point::new(0, 28), style_small),
    )
    .await;

    let (min, avg, max) = UI_DRAW_TIME.get();
    text.clear();
    let _ = write!(text, "Drw{:>5}/{:>5}/{:>5}", min, avg, max);
    draw(
        buffer,
        pacer,
        &Text::new(&text, Point::new(0, 38), style_small),
    )
    .await;

    text.clear();
    let _ = write!(
        text,
        "Ovr {} MIDIq {}/{}",
        PERIOD_OVERRUN.load(Ordering::Relaxed),
        MIDI_TX_MAX_USED.load(Ordering::Relaxed),
        MIDI_TX_OVERFLOW.load(Ordering::Relaxed)
    );
    draw(
        buffer,
        pacer,
        &Text::new(&text, Point::new(0, 50), style_small),
    )
    .await;

    text.clear();
    let _ = write!(text, "Err {}", error::get());
    draw(
        buffer,
        pacer,
        &Text::new(&text, Point::new(0, 60), style_small),
    )
    .await;
}

async fn display4(buffer: &mut OledBuffer, counter: u32, pacer: &mut DrawPacer) {
    buffer.clear();
    draw(buffer, pacer, &outline_frame()).await;

    //let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);

    let mut text: String<32> = String::new();
    let _ = write!(text, "Piano");
    draw(
        buffer,
        pacer,
        &Text::new(&text, Point::new(12, 18), style_small),
    )
    .await;

    text.clear();
    let _ = write!(text, "Violin");
    draw(
        buffer,
        pacer,
        &Text::new(&text, Point::new(12, 36), style_small),
    )
    .await;

    text.clear();
    let _ = write!(text, "up/down   quit");
    draw(
        buffer,
        pacer,
        &Text::new(&text, Point::new(20, 56), style_small),
    )
    .await;

    let work_mode = WORK_MODE
        .load(core::sync::atomic::Ordering::Relaxed)
        .try_into()
        .unwrap_or(WorkMode::Piano);
    if counter % 10 < 5 {
        let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
        let y = if work_mode == WorkMode::Piano { 10 } else { 27 };
        let cursor = Rectangle::new(Point::new(8, y), Size::new(100, 14)).into_styled(outline);
        draw(buffer, pacer, &cursor).await;
    }
}

pub fn draw_bar(buffer: &mut OledBuffer, number: i32, value: u32) {
    const BAR_START_X: i32 = 54;
    let start_y: i32 = 24 + number * 2;
    const BAR_WIDTH: u32 = 64;
    const BAR_HEIGHT: u32 = 2;
    const BAR_VALUE_MIN: u32 = 0;
    const BAR_VALUE_MAX: u32 = 4095;

    let fill_style = PrimitiveStyle::with_fill(BinaryColor::On);
    let clamped = value.clamp(BAR_VALUE_MIN, BAR_VALUE_MAX);
    let range = BAR_VALUE_MAX.saturating_sub(BAR_VALUE_MIN);
    let numerator = clamped.saturating_sub(BAR_VALUE_MIN);
    let filled_width = (numerator * BAR_WIDTH).checked_div(range).unwrap_or(0);

    if filled_width > 0 {
        let fill = Rectangle::new(
            Point::new(BAR_START_X, start_y),
            Size::new(filled_width, BAR_HEIGHT),
        );
        let _ = fill.into_styled(fill_style).draw(buffer);
    }
}

fn demo_lines(buffer: &mut OledBuffer) {
    buffer.clear();
    let style = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let w = 128;
    let h = 64;

    // Reduced iterations to avoid I2C timeout
    for x in (0..w).step_by(16) {
        let _ = Line::new(Point::new(0, 0), Point::new(x, h - 1))
            .into_styled(style)
            .draw(buffer);
    }
    for y in (0..h).step_by(16) {
        let _ = Line::new(Point::new(0, 0), Point::new(w - 1, y))
            .into_styled(style)
            .draw(buffer);
    }
}

fn demo_rects(buffer: &mut OledBuffer) {
    buffer.clear();
    let style = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    // Reduced from 8 to 5 iterations
    for i in 0..5 {
        let inset = i * 6;
        let rect = Rectangle::new(
            Point::new(inset, inset),
            Size::new(128 - (inset as u32) * 2, 64 - (inset as u32) * 2),
        );
        let _ = rect.into_styled(style).draw(buffer);
    }
}

fn demo_filled_rects(buffer: &mut OledBuffer) {
    buffer.clear();
    let style = PrimitiveStyle::with_fill(BinaryColor::On);

    // Reduced from 8 to 4 iterations
    for i in 0..4 {
        let inset = i * 10;
        let size = Size::new(128 - (inset as u32) * 2, 64 - (inset as u32) * 2);
        if size.width == 0 || size.height == 0 {
            break;
        }
        let rect = Rectangle::new(Point::new(inset, inset), size);
        let _ = rect.into_styled(style).draw(buffer);
    }
}

fn demo_circles(buffer: &mut OledBuffer) {
    buffer.clear();
    let style = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    // Reduced iterations
    for r in (6..30).step_by(6) {
        let circle = Circle::new(Point::new(64 - r, 32 - r), (r as u32) * 2);
        let _ = circle.into_styled(style).draw(buffer);
    }
}

fn demo_filled_circles(buffer: &mut OledBuffer) {
    buffer.clear();
    let style = PrimitiveStyle::with_fill(BinaryColor::On);

    // Reduced iterations
    for r in (8..28).step_by(8) {
        let circle = Circle::new(Point::new(64 - r, 32 - r), (r as u32) * 2);
        let _ = circle.into_styled(style).draw(buffer);
    }
}

fn demo_round_rects(buffer: &mut OledBuffer) {
    buffer.clear();
    let style = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    // Reduced from 6 to 4
    for i in 0..4 {
        let inset = i * 5;
        let rect = RoundedRectangle::with_equal_corners(
            Rectangle::new(
                Point::new(inset, inset),
                Size::new(128 - (inset as u32) * 2, 64 - (inset as u32) * 2),
            ),
            Size::new(6, 6),
        );
        let _ = rect.into_styled(style).draw(buffer);
    }
}

fn demo_filled_round_rects(buffer: &mut OledBuffer) {
    buffer.clear();
    let style = PrimitiveStyleBuilder::new()
        .fill_color(BinaryColor::On)
        .stroke_color(BinaryColor::Off)
        .stroke_width(0)
        .build();

    // Reduced from 6 to 4
    for i in 0..4 {
        let inset = i * 6;
        let size = Size::new(128 - (inset as u32) * 2, 64 - (inset as u32) * 2);
        if size.width == 0 || size.height == 0 {
            break;
        }
        let rect = RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(inset, inset), size),
            Size::new(8, 8),
        );
        let _ = rect.into_styled(style).draw(buffer);
    }
}

fn demo_triangles(buffer: &mut OledBuffer) {
    buffer.clear();
    let style = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    // Reduced from 6 to 4
    for i in 0..4 {
        let inset = i * 5;
        let tri = Triangle::new(
            Point::new(64, inset),
            Point::new(127 - inset, 63 - inset),
            Point::new(inset, 63 - inset),
        );
        let _ = tri.into_styled(style).draw(buffer);
    }
}

fn demo_filled_triangles(buffer: &mut OledBuffer) {
    buffer.clear();
    let style = PrimitiveStyle::with_fill(BinaryColor::On);

    // Reduced from 5 to 3
    for i in 0..3 {
        let inset = i * 7;
        let tri = Triangle::new(
            Point::new(64, inset),
            Point::new(127 - inset, 63 - inset),
            Point::new(inset, 63 - inset),
        );
        let _ = tri.into_styled(style).draw(buffer);
    }
}

fn demo_text(buffer: &mut OledBuffer) {
    buffer.clear();
    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);

    let _ = Text::new("QUBIT2", Point::new(0, 16), style_big).draw(buffer);
    let _ = Text::new("SSD1306 demo", Point::new(0, 40), style_small).draw(buffer);
    let _ = Text::new("I2C shared bus", Point::new(0, 54), style_small).draw(buffer);
}

fn demo_styles(buffer: &mut OledBuffer) {
    buffer.clear();

    let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let _ = Rectangle::new(Point::new(0, 0), Size::new(128, 64))
        .into_styled(outline)
        .draw(buffer);

    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let _ = Text::new("1) shapes", Point::new(6, 16), style_small).draw(buffer);
    let _ = Text::new("2) text", Point::new(6, 30), style_small).draw(buffer);
    let _ = Text::new("3) bitmap", Point::new(6, 44), style_small).draw(buffer);
}

fn demo_bitmap(buffer: &mut OledBuffer) {
    // 16x16 1bpp "X" bitmap
    #[rustfmt::skip]
    const RAW: [u8; 32] = [
        0b1000_0001, 0b0000_0001,
        0b0100_0010, 0b1000_0010,
        0b0010_0100, 0b0100_0100,
        0b0001_1000, 0b0010_1000,
        0b0001_1000, 0b0010_1000,
        0b0010_0100, 0b0100_0100,
        0b0100_0010, 0b1000_0010,
        0b1000_0001, 0b0000_0001,
        0b1000_0001, 0b0000_0001,
        0b0100_0010, 0b1000_0010,
        0b0010_0100, 0b0100_0100,
        0b0001_1000, 0b0010_1000,
        0b0001_1000, 0b0010_1000,
        0b0010_0100, 0b0100_0100,
        0b0100_0010, 0b1000_0010,
        0b1000_0001, 0b0000_0001,
    ];

    buffer.clear();
    let raw: ImageRaw<BinaryColor> = ImageRaw::new(&RAW, 16);
    let _ = Image::new(&raw, Point::new(56, 24)).draw(buffer);

    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let _ = Text::new("bitmap", Point::new(0, 12), style_small).draw(buffer);
}

fn demo_animate_frame(buffer: &mut OledBuffer, x: u8) -> bool {
    let x = x.min(128 - 10);
    buffer.clear();
    let style = PrimitiveStyle::with_fill(BinaryColor::On);
    let rect = Rectangle::new(Point::new(x as i32, 28), Size::new(10, 10));
    let _ = rect.into_styled(style).draw(buffer);
    x >= (128 - 10)
}
