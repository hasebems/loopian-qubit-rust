use crate::WORK_MODE_DISPLAY;
use crate::constants::*;
use core::f32::consts::PI;
use libm::sinf;
use portable_atomic::Ordering;
use smart_leds::RGBW;

pub struct RingLed {
    counter: u32, // 色の変化のためのカウンター
}

impl RingLed {
    fn source_index_for_led(led_index: usize) -> usize {
        (led_index + TOUCH_INDEX_SHIFT) % NUM_LEDS
    }

    pub fn new() -> Self {
        Self { counter: 0 }
    }

    pub fn render(
        &mut self,
        data: &mut [RGBW<u8>; NUM_LEDS],
        touchkey_state: &[Option<f32>; MAX_TOUCH_POINTS],
        rxkey_bits: u32,
    ) {
        let num_leds_f = NUM_LEDS as f32;
        let time_sec = self.counter as f32 * 0.02; // ringled_task is updated every 20ms
        let phase = 0.5 * PI * time_sec; // 0.5pi rad/s
        let color_phase = 0.2 * PI * time_sec; // 10s color cycle for background tint
        let background_tint_r = ((sinf(color_phase) + 1.0) * 0.5).clamp(0.0, 1.0);
        let background_tint_g =
            ((sinf(color_phase + (2.0 * PI / 3.0)) + 1.0) * 0.5).clamp(0.0, 1.0);
        let background_tint_b =
            ((sinf(color_phase + (4.0 * PI / 3.0)) + 1.0) * 0.5).clamp(0.0, 1.0);
        let work_mode_display = WORK_MODE_DISPLAY.load(Ordering::Relaxed);
        let setting_blink_on = (self.counter / 25).is_multiple_of(2);

        for (i, led) in data.iter_mut().enumerate().take(NUM_LEDS) {
            let source_index = Self::source_index_for_led(i);
            let source_pos = source_index as f32;
            let led_angle = (i as f32 / num_leds_f) * 2.0 * PI;

            // 8x finer spatial wave and darker output for NeoPixel brightness perception.
            let wave = (sinf(led_angle * 8.0 - phase) + 1.0) * 0.5;
            let wave_shaped = wave * wave;

            // Make the dark part clearly off instead of keeping a constant floor brightness.
            let background_level = ((wave_shaped - 0.18) / 0.82).clamp(0.0, 1.0);
            let white = (background_level * 20.0).clamp(0.0, 255.0) as u8;
            let mut white_out = 0;

            let mut r = 0u8;
            let mut g = 0u8;
            let mut b = 0u8;

            if !work_mode_display {
                // touch position: magenta glow around +/- 3 LEDs (circular wrap)
                for touch in touchkey_state.iter().flatten() {
                    let mut dist = (*touch - source_pos).abs();
                    dist = dist.min(num_leds_f - dist);
                    if dist <= 3.0 {
                        let intensity = 1.0 - dist / 3.0;
                        let magenta = (220.0_f32 * intensity * intensity).clamp(0.0, 255.0) as u8;
                        r = r.saturating_add(magenta);
                        b = b.saturating_add(magenta);
                    }
                }

                // rx key: only this LED lights cyan
                if ((rxkey_bits >> source_index) & 1) != 0 {
                    g = g.saturating_add(120);
                    b = b.saturating_add(180);
                }

                if white > 0 && r == 0 && g == 0 && b == 0 {
                    let tint_strength = background_level * 14.0;
                    r = (background_tint_r * tint_strength).clamp(0.0, 255.0) as u8;
                    g = (background_tint_g * tint_strength).clamp(0.0, 255.0) as u8;
                    b = (background_tint_b * tint_strength).clamp(0.0, 255.0) as u8;
                }
            } else {
                white_out = if setting_blink_on && i % 6 == 0 {
                    24
                } else {
                    0
                };
            }

            *led = RGBW {
                r,
                g,
                b,
                a: smart_leds::White(white_out),
            };
        }
        self.counter = self.counter.wrapping_add(1);
    }
}
