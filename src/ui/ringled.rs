use crate::constants::*;
use core::f32::consts::PI;
use libm::sinf;
use smart_leds::RGBW;

pub struct RingLed {
    rxkey_state: [bool; NUM_LEDS], // 受信したNote On/Offの状態を保持
    touchkey_state: [Option<f32>; MAX_TOUCH_POINTS], // 送信したNote On/Offの状態を保持
    counter: u32,                  // 色の変化のためのカウンター
}

impl RingLed {
    pub fn new() -> Self {
        Self {
            rxkey_state: [false; NUM_LEDS],
            touchkey_state: [None; MAX_TOUCH_POINTS],
            counter: 0,
        }
    }

    pub fn set_color(&mut self, data: &mut [RGBW<u8>; NUM_LEDS], location: f32, cmd: u8) {
        let num = (location + 0.5).clamp(0.0, (NUM_LEDS - 1) as f32) as usize; // 安全のために位置をクランプ
        if cmd == RINGLED_CMD_RX_ON {
            self.rxkey_state[num] = true;
        } else if cmd == RINGLED_CMD_RX_OFF {
            self.rxkey_state[num] = false;
        } else if cmd & 0xf0 == RINGLED_CMD_TX_ON {
            let idx = (cmd & 0x0f).clamp(0, MAX_TOUCH_POINTS_U8 - 1) as usize;
            self.touchkey_state[idx] = Some(location);
        } else if cmd & 0xf0 == RINGLED_CMD_TX_OFF {
            let idx = (cmd & 0x0f).clamp(0, MAX_TOUCH_POINTS_U8 - 1) as usize;
            self.touchkey_state[idx] = None;
        }

        let num_leds_f = NUM_LEDS as f32;
        let time_sec = self.counter as f32 * 0.02; // ringled_task is updated every 20ms
        let phase = 0.5 * PI * time_sec; // 0.5pi rad/s

        for (i, led) in data.iter_mut().enumerate().take(NUM_LEDS) {
            let led_angle = (i as f32 / num_leds_f) * 2.0 * PI;
            // 8x finer spatial wave and darker output for NeoPixel brightness perception.
            let wave = (sinf(led_angle * 8.0 - phase) + 1.0) * 0.5;
            let wave_shaped = wave * wave;
            // Keep the wider white range (2..24) without temporal dithering.
            let white = (2.0 + wave_shaped * 22.0).clamp(0.0, 255.0) as u8;

            let mut r = 0u8;
            let mut g = 0u8;
            let mut b = 0u8;

            // touch position: magenta glow around +/- 3 LEDs (circular wrap)
            for touch in self.touchkey_state.iter().flatten() {
                let mut dist = (touch - i as f32).abs();
                dist = dist.min(num_leds_f - dist);
                if dist <= 3.0 {
                    let intensity = 1.0 - dist / 3.0;
                    let magenta = (220.0 * intensity * intensity).clamp(0.0, 255.0) as u8;
                    r = r.saturating_add(magenta);
                    b = b.saturating_add(magenta);
                }
            }

            // rx key: only this LED lights cyan
            if self.rxkey_state[i] {
                g = g.saturating_add(120);
                b = b.saturating_add(180);
            }

            *led = RGBW {
                r,
                g,
                b,
                a: smart_leds::White(white),
            };
        }
        self.counter = self.counter.wrapping_add(1);
    }
}
