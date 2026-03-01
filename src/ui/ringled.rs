use crate::constants::*;
use crate::devices::ws2812::wheel;
use smart_leds::RGBW;

pub struct RingLed {
    rxkey_state: [bool; NUM_LEDS], // 受信したNote On/Offの状態を保持
    txkey_state: [Option<f32>; MAX_TOUCH_POINTS], // 送信したNote On/Offの状態を保持
    counter: u32,                  // 色の変化のためのカウンター
}

impl RingLed {
    pub fn new() -> Self {
        Self {
            rxkey_state: [false; NUM_LEDS],
            txkey_state: [None; MAX_TOUCH_POINTS],
            counter: 0,
        }
    }

    pub fn set_color(&mut self, data: &mut [RGBW<u8>; NUM_LEDS], location: f32, cmd: u8) {
        let num = location.clamp(0.0, (NUM_LEDS - 1) as f32) as usize; // 安全のために位置をクランプ
        if cmd == RINGLED_CMD_RX_ON {
            self.rxkey_state[num] = true;
        } else if cmd == RINGLED_CMD_RX_OFF {
            self.rxkey_state[num] = false;
        } else if cmd & 0xf0 == RINGLED_CMD_TX_ON {
            self.txkey_state[(cmd & 0x0f).clamp(0, (MAX_TOUCH_POINTS - 1) as u8) as usize] =
                Some(location);
        } else if cmd & 0xf0 == RINGLED_CMD_TX_OFF {
            self.txkey_state[(cmd & 0x0f).clamp(0, (MAX_TOUCH_POINTS - 1) as u8) as usize] = None;
        }
        for (i, led) in data.iter_mut().enumerate().take(NUM_LEDS) {
            let color = wheel(self.counter.wrapping_add(i as u32 * 16) as u8);
            // Convert RGB8 to RGBW (White is controlled by MIDI)
            let txlvl = self
                .txkey_state
                .iter()
                .map(|&tx| {
                    tx.map(|tx| {
                        let close = (tx - i as f32).abs();
                        if close > 2.0 {
                            0
                        } else {
                            (128.0 / close).clamp(0.0, 255.0) as u8
                        }
                    })
                    .unwrap_or(0)
                })
                .sum::<u8>();
            *led = RGBW {
                r: color.r / 4, // RGBを少し暗くして白とバランスを取る
                g: color.g / 4,
                b: color.b / 4,
                a: smart_leds::White(
                    if self.rxkey_state[i] { 255 } else { 0 } + txlvl, // 受信状態と送信状態を合算),
                ),
            };
        }
        self.counter = self.counter.wrapping_add(1);
    }
}
