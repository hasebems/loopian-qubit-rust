use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_usb::class::midi::Receiver;
use portable_atomic::Ordering;

use crate::constants::*;
use crate::error::ERROR_CODE;
use crate::shared::{RINGLED_RX_BITS, WORK_MODE};

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      MIDI RX Task: USB経由で受信したMIDIイベントの処理
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn midi_rx_task(mut receiver: Receiver<'static, Driver<'static, USB>>) {
    let mut buf = [0; 64];

    let set_rx_led = |note: u8, on: bool| {
        let led = (note as usize).min(NUM_LEDS - 1);
        let word = &RINGLED_RX_BITS[led / 32];
        let bit = 1u32 << (led % 32);
        if on {
            word.fetch_or(bit, Ordering::Relaxed);
        } else {
            word.fetch_and(!bit, Ordering::Relaxed);
        }
    };

    loop {
        match receiver.read_packet(&mut buf).await {
            Ok(n) => {
                let work_mode = WORK_MODE
                    .load(Ordering::Relaxed)
                    .try_into()
                    .unwrap_or(WorkMode::Piano);

                if work_mode != WorkMode::Violin {
                    for packet in buf[0..n].chunks(4) {
                        if packet.len() == 4 {
                            let status = packet[1];
                            let note = packet[2];
                            let velocity = packet[3];

                            // Note On (Channel 0-15)
                            if (status & 0xF0) == 0x90 {
                                if velocity > 0 {
                                    set_rx_led(note, true);
                                } else {
                                    set_rx_led(note, false);
                                }
                            }
                            // Note Off
                            else if (status & 0xF0) == 0x80 {
                                set_rx_led(note, false);
                            }
                        }
                    }
                }
            }
            Err(_e) => {
                // エラーカウント
                ERROR_CODE.store(54, Ordering::Relaxed);
            }
        }
    }
}
