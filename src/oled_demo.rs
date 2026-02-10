use embedded_graphics::image::{Image, ImageRaw};
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_6X10};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{
    Circle, Line, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle, Triangle,
};
use embedded_graphics::text::Text;

use crate::devices::ssd1306::Oled;

type DispErr = display_interface::DisplayError;

pub struct OledDemo {
    step: u8,
    anim_x: u8,
}

#[allow(dead_code)]
pub fn draw_bringup_screen<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();
    let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let _ = Rectangle::new(Point::new(0, 0), Size::new(128, 64))
        .into_styled(outline)
        .draw(oled.display());

    let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let _ = Text::new("OLED OK", Point::new(10, 22), style_big).draw(oled.display());
    let _ = Text::new("I2C1 GPIO6/7", Point::new(10, 44), style_small).draw(oled.display());
    let _ = Text::new("addr 0x3C/0x3D", Point::new(10, 56), style_small).draw(oled.display());
    oled.flush()
}

impl OledDemo {
    pub fn new() -> Self {
        Self { step: 0, anim_x: 0 }
    }

    /// Executes a single demo step and returns the suggested delay (ms) before the next step.
    pub fn tick<I2C>(&mut self, oled: &mut Oled<I2C>) -> Result<u64, DispErr>
    where
        I2C: embedded_hal::i2c::I2c,
    {
        match self.step {
            0 => {
                // loop1(): drawPixel + display + delay(2000)
                oled.clear();
                let _ = Pixel(Point::new(10, 10), BinaryColor::On).draw(oled.display());
                oled.flush()?;
                self.step += 1;
                Ok(2000)
            }
            1 => {
                demo_lines(oled)?;
                self.step += 1;
                Ok(800)
            }
            2 => {
                demo_rects(oled)?;
                self.step += 1;
                Ok(800)
            }
            3 => {
                demo_filled_rects(oled)?;
                self.step += 1;
                Ok(800)
            }
            4 => {
                demo_circles(oled)?;
                self.step += 1;
                Ok(800)
            }
            5 => {
                demo_filled_circles(oled)?;
                self.step += 1;
                Ok(800)
            }
            6 => {
                demo_round_rects(oled)?;
                self.step += 1;
                Ok(800)
            }
            7 => {
                demo_filled_round_rects(oled)?;
                self.step += 1;
                Ok(800)
            }
            8 => {
                demo_triangles(oled)?;
                self.step += 1;
                Ok(800)
            }
            9 => {
                demo_filled_triangles(oled)?;
                self.step += 1;
                Ok(800)
            }
            10 => {
                demo_text(oled)?;
                self.step += 1;
                Ok(1200)
            }
            11 => {
                demo_styles(oled)?;
                self.step += 1;
                Ok(900)
            }
            12 => {
                // scroll/invert are intentionally omitted.
                demo_bitmap(oled)?;
                self.step += 1;
                Ok(900)
            }
            13 => {
                let done = demo_animate_frame(oled, self.anim_x)?;
                if done {
                    self.anim_x = 0;
                    self.step = 0;
                    Ok(1000)
                } else {
                    self.anim_x = self.anim_x.saturating_add(6);
                    Ok(80)
                }
            }
            _ => {
                self.step = 0;
                Ok(200)
            }
        }
    }
}

fn demo_lines<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();
    let style = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let w = 128;
    let h = 64;

    // Reduced iterations to avoid I2C timeout
    for x in (0..w).step_by(16) {
        let _ = Line::new(Point::new(0, 0), Point::new(x as i32, (h - 1) as i32))
            .into_styled(style)
            .draw(oled.display());
    }
    for y in (0..h).step_by(16) {
        let _ = Line::new(Point::new(0, 0), Point::new((w - 1) as i32, y as i32))
            .into_styled(style)
            .draw(oled.display());
    }

    oled.flush()
}

fn demo_rects<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();
    let style = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    // Reduced from 8 to 5 iterations
    for i in 0..5 {
        let inset = i * 6;
        let rect = Rectangle::new(
            Point::new(inset, inset),
            Size::new(128 - (inset as u32) * 2, 64 - (inset as u32) * 2),
        );
        let _ = rect.into_styled(style).draw(oled.display());
    }

    oled.flush()
}

fn demo_filled_rects<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();
    let style = PrimitiveStyle::with_fill(BinaryColor::On);

    // Reduced from 8 to 4 iterations
    for i in 0..4 {
        let inset = i * 10;
        let size = Size::new(128 - (inset as u32) * 2, 64 - (inset as u32) * 2);
        if size.width == 0 || size.height == 0 {
            break;
        }
        let rect = Rectangle::new(Point::new(inset, inset), size);
        let _ = rect.into_styled(style).draw(oled.display());
    }

    oled.flush()
}

fn demo_circles<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();
    let style = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    // Reduced iterations
    for r in (6..30).step_by(6) {
        let circle = Circle::new(Point::new(64 - r, 32 - r), (r as u32) * 2);
        let _ = circle.into_styled(style).draw(oled.display());
    }

    oled.flush()
}

fn demo_filled_circles<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();
    let style = PrimitiveStyle::with_fill(BinaryColor::On);

    // Reduced iterations
    for r in (8..28).step_by(8) {
        let circle = Circle::new(Point::new(64 - r, 32 - r), (r as u32) * 2);
        let _ = circle.into_styled(style).draw(oled.display());
    }

    oled.flush()
}

fn demo_round_rects<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();
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
        let _ = rect.into_styled(style).draw(oled.display());
    }

    oled.flush()
}

fn demo_filled_round_rects<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();
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
        let _ = rect.into_styled(style).draw(oled.display());
    }

    oled.flush()
}

fn demo_triangles<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();
    let style = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    // Reduced from 6 to 4
    for i in 0..4 {
        let inset = i * 5;
        let tri = Triangle::new(
            Point::new(64, inset),
            Point::new(127 - inset, 63 - inset),
            Point::new(inset, 63 - inset),
        );
        let _ = tri.into_styled(style).draw(oled.display());
    }

    oled.flush()
}

fn demo_filled_triangles<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();
    let style = PrimitiveStyle::with_fill(BinaryColor::On);

    // Reduced from 5 to 3
    for i in 0..3 {
        let inset = i * 7;
        let tri = Triangle::new(
            Point::new(64, inset),
            Point::new(127 - inset, 63 - inset),
            Point::new(inset, 63 - inset),
        );
        let _ = tri.into_styled(style).draw(oled.display());
    }

    oled.flush()
}

fn demo_text<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();
    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let style_big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);

    let _ = Text::new("QUBIT2", Point::new(0, 16), style_big).draw(oled.display());
    let _ = Text::new("SSD1306 demo", Point::new(0, 40), style_small).draw(oled.display());
    let _ = Text::new("I2C shared bus", Point::new(0, 54), style_small).draw(oled.display());

    oled.flush()
}

fn demo_styles<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    oled.clear();

    let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let _ = Rectangle::new(Point::new(0, 0), Size::new(128, 64))
        .into_styled(outline)
        .draw(oled.display());

    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let _ = Text::new("1) shapes", Point::new(6, 16), style_small).draw(oled.display());
    let _ = Text::new("2) text", Point::new(6, 30), style_small).draw(oled.display());
    let _ = Text::new("3) bitmap", Point::new(6, 44), style_small).draw(oled.display());

    oled.flush()
}

fn demo_bitmap<I2C>(oled: &mut Oled<I2C>) -> Result<(), DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    // 16x16 1bpp "X" bitmap
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

    oled.clear();
    let raw: ImageRaw<BinaryColor> = ImageRaw::new(&RAW, 16);
    let _ = Image::new(&raw, Point::new(56, 24)).draw(oled.display());

    let style_small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let _ = Text::new("bitmap", Point::new(0, 12), style_small).draw(oled.display());

    oled.flush()
}

fn demo_animate_frame<I2C>(oled: &mut Oled<I2C>, x: u8) -> Result<bool, DispErr>
where
    I2C: embedded_hal::i2c::I2c,
{
    let x = x.min(128 - 10);
    oled.clear();
    let style = PrimitiveStyle::with_fill(BinaryColor::On);
    let rect = Rectangle::new(Point::new(x as i32, 28), Size::new(10, 10));
    let _ = rect.into_styled(style).draw(oled.display());
    oled.flush()?;
    Ok(x >= (128 - 10))
}
