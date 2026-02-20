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
use core::fmt::Write;

use crate::devices::ssd1306::OledBuffer;

pub struct OledDemo {
    step: u8,
    anim_x: u8,
}

#[allow(dead_code)]
pub fn draw_bringup_screen(buffer: &mut OledBuffer) {
    buffer.clear();
    let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let _ = Rectangle::new(Point::new(0, 0), Size::new(128, 64))
        .into_styled(outline)
        .draw(buffer);

    let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let _ = Text::new("OLED OK", Point::new(10, 22), style_big).draw(buffer);
    let _ = Text::new("I2C1 GPIO6/7", Point::new(10, 44), style_small).draw(buffer);
    let _ = Text::new("addr 0x3C/0x3D", Point::new(10, 56), style_small).draw(buffer);
}

impl OledDemo {
    pub fn new() -> Self {
        Self { step: 0, anim_x: 0 }
    }

    /// Executes a single demo step and returns the suggested delay (ms) before the next step.
    pub fn tick(&mut self, buffer: &mut OledBuffer, a: u8, b: u8, c: u8) -> u64 {
        match self.step {
            0 => {
                // loop1(): drawPixel + display + delay(2000)
                buffer.clear();
                //let _ = Pixel(Point::new(10, 10), BinaryColor::On).draw(buffer);
                //self.step += 1;
                //2000
                display_info(buffer, a, b, c);
                100
            }
            1 => {
                demo_lines(buffer);
                self.step += 1;
                800
            }
            2 => {
                demo_rects(buffer);
                self.step += 1;
                800
            }
            3 => {
                demo_filled_rects(buffer);
                self.step += 1;
                800
            }
            4 => {
                demo_circles(buffer);
                self.step += 1;
                800
            }
            5 => {
                demo_filled_circles(buffer);
                self.step += 1;
                800
            }
            6 => {
                demo_round_rects(buffer);
                self.step += 1;
                800
            }
            7 => {
                demo_filled_round_rects(buffer);
                self.step += 1;
                800
            }
            8 => {
                demo_triangles(buffer);
                self.step += 1;
                800
            }
            9 => {
                demo_filled_triangles(buffer);
                self.step += 1;
                800
            }
            10 => {
                demo_text(buffer);
                self.step += 1;
                1200
            }
            11 => {
                demo_styles(buffer);
                self.step += 1;
                900
            }
            12 => {
                // scroll/invert are intentionally omitted.
                demo_bitmap(buffer);
                self.step += 1;
                900
            }
            13 => {
                let done = demo_animate_frame(buffer, self.anim_x);
                if done {
                    self.anim_x = 0;
                    self.step = 0;
                    1000
                } else {
                    self.anim_x = self.anim_x.saturating_add(6);
                    80
                }
            }
            _ => {
                self.step = 0;
                200
            }
        }
    }
}

fn display_info(buffer: &mut OledBuffer, var1: u8, var2: u8, var3: u8) {
    buffer.clear();

    let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let _ = Rectangle::new(Point::new(0, 0), Size::new(128, 64))
        .into_styled(outline)
        .draw(buffer);

    let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    
    let mut text1: String<32> = String::new();
    let _ = write!(text1, "var1: {}", var1);
    let _ = Text::new(&text1, Point::new(6, 16), style_big).draw(buffer);
    
    let mut text2: String<32> = String::new();
    let _ = write!(text2, "var2: {}", var2);
    let _ = Text::new(&text2, Point::new(6, 36), style_big).draw(buffer);
    
    let mut text3: String<32> = String::new();
    let _ = write!(text3, "var3: {}", var3);
    let _ = Text::new(&text3, Point::new(6, 56), style_big).draw(buffer);
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
