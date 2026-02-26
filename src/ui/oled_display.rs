use core::fmt::Write;
use embedded_graphics::image::{Image, ImageRaw};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_10X20};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{
    Circle, Line, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle, Triangle,
};
use embedded_graphics::text::Text;
use heapless::String;

use crate::devices::ssd1306::OledBuffer;
use crate::{
    ELAPSED_TIME, ERROR_CODE, POINT0, POINT1, POINT2, POINT3, TOUCH0, TOUCH1, TOUCH2, TOUCH3,
};

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

    pub fn draw_bringup_screen(&self, buffer: &mut OledBuffer) {
        buffer.clear();
        let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
        let _ = Rectangle::new(Point::new(0, 0), Size::new(128, 64))
            .into_styled(outline)
            .draw(buffer);

        let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
        let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
        let _ = Text::new("Loopian::", Point::new(5, 20), style_small).draw(buffer);
        let _ = Text::new("QUBIT", Point::new(64, 20), style_big).draw(buffer);
        let _ = Text::new(
            concat!("build: ", env!("BUILD_DATE")),
            Point::new(30, 44),
            style_small,
        )
        .draw(buffer);
        let _ = Text::new(
            concat!("       ", env!("BUILD_TIME")),
            Point::new(30, 56),
            style_small,
        )
        .draw(buffer);
    }

    /// Executes a single demo step and returns the suggested delay (ms) before the next step.
    pub fn tick(&mut self, buffer: &mut OledBuffer, counter: u32) {
        match self.page {
            0 => self.draw_bringup_screen(buffer),
            1 => display1(buffer, counter),
            2 => display2(buffer),
            3 => display3(buffer),
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

fn display1(buffer: &mut OledBuffer, counter: u32) {
    buffer.clear();

    let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let _ = Rectangle::new(Point::new(0, 0), Size::new(128, 64))
        .into_styled(outline)
        .draw(buffer);

    let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);

    let mut text1: String<32> = String::new();
    let _ = write!(text1, "Cntr: {}", counter);
    let _ = Text::new(&text1, Point::new(6, 16), style_big).draw(buffer);

    let er = ERROR_CODE.load(core::sync::atomic::Ordering::Relaxed);
    text1.clear();
    let _ = write!(text1, "ErCd: {}", er);
    let _ = Text::new(&text1, Point::new(6, 32), style_big).draw(buffer);

    let elapsed_time = ELAPSED_TIME.load(core::sync::atomic::Ordering::Relaxed);
    text1.clear();
    let _ = write!(text1, "Time: {}", elapsed_time);
    let _ = Text::new(&text1, Point::new(6, 48), style_big).draw(buffer);
}

fn display2(buffer: &mut OledBuffer) {
    buffer.clear();

    let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let _ = Rectangle::new(Point::new(0, 0), Size::new(128, 64))
        .into_styled(outline)
        .draw(buffer);

    //let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);

    let mut text1: String<32> = String::new();
    let p0 = POINT0.load(core::sync::atomic::Ordering::Relaxed);
    let _ = write!(text1, "Point64: {}", p0);
    let _ = Text::new(&text1, Point::new(6, 12), style_small).draw(buffer);

    let p1 = POINT1.load(core::sync::atomic::Ordering::Relaxed);
    text1.clear();
    let _ = write!(text1, "Point65: {}", p1);
    let _ = Text::new(&text1, Point::new(6, 24), style_small).draw(buffer);

    let p2 = POINT2.load(core::sync::atomic::Ordering::Relaxed);
    text1.clear();
    let _ = write!(text1, "Point66: {}", p2);
    let _ = Text::new(&text1, Point::new(6, 36), style_small).draw(buffer);

    let p3 = POINT3.load(core::sync::atomic::Ordering::Relaxed);
    text1.clear();
    let _ = write!(text1, "Point67: {}", p3);
    let _ = Text::new(&text1, Point::new(6, 48), style_small).draw(buffer);
}

fn display3(buffer: &mut OledBuffer) {
    buffer.clear();

    let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let _ = Rectangle::new(Point::new(0, 0), Size::new(128, 64))
        .into_styled(outline)
        .draw(buffer);

    //let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);

    let mut text1: String<32> = String::new();
    let p0 = TOUCH0.load(core::sync::atomic::Ordering::Relaxed);
    if (0..10000).contains(&p0) {
        let _ = write!(text1, "Touch1: {}", p0);
    } else {
        let _ = write!(text1, "Touch1: ---");
    }
    let _ = Text::new(&text1, Point::new(6, 12), style_small).draw(buffer);

    text1.clear();
    let p1 = TOUCH1.load(core::sync::atomic::Ordering::Relaxed);
    if (0..10000).contains(&p1) {
        let _ = write!(text1, "Touch2: {}", p1);
    } else {
        let _ = write!(text1, "Touch2: ---");
    }
    let _ = Text::new(&text1, Point::new(6, 24), style_small).draw(buffer);

    text1.clear();
    let p2 = TOUCH2.load(core::sync::atomic::Ordering::Relaxed);
    if (0..10000).contains(&p2) {
        let _ = write!(text1, "Touch3: {}", p2);
    } else {
        let _ = write!(text1, "Touch3: ---");
    }
    let _ = Text::new(&text1, Point::new(6, 36), style_small).draw(buffer);

    text1.clear();
    let p3 = TOUCH3.load(core::sync::atomic::Ordering::Relaxed);
    if (0..10000).contains(&p3) {
        let _ = write!(text1, "Touch4: {}", p3);
    } else {
        let _ = write!(text1, "Touch4: ---");
    }
    let _ = Text::new(&text1, Point::new(6, 48), style_small).draw(buffer);
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
