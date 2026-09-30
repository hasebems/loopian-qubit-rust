//  Created by Hasebe Masahiko on 2026/09/30.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! デバッグ用: タッチの生値を USB CDC で PC に送る（doc/debug_env.md §3.5）
use embassy_futures::select::{Either3, select3};
use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_time::{Duration, with_timeout};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, Sender};
use portable_atomic::Ordering;

use crate::constants::SCAN_PERIOD_MS;
use crate::debug_protocol::Packet;
use crate::shared::{DEBUG_DROPPED, DEBUG_FRAMES, DEBUG_STREAMING};

type UsbDriver = Driver<'static, USB>;

pub const CDC_PACKET_SIZE: u16 = 64; // CDC のバルク転送の最大パケット長
// 1 パケットの書き込みがこれを超えたら、PC が読んでいない（切断された）とみなす
const WRITE_TIMEOUT_MS: u64 = 100;
const COMMAND_MAX_LEN: usize = 32; // PC からのコマンド 1 行の最大長

/// PC との接続が切れた（ポートが閉じた・書き込みのタイムアウト・USB の切断）
struct Disconnected;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Debug Stream Task (Core0): DEBUG_FRAMES を PC に送り、PC からのコマンドを受ける
//      PC がポートを開いている（DTR）間だけ動き、start を受けるまではフレームを送らない
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn debug_stream_task(class: CdcAcmClass<'static, UsbDriver>) {
    let (mut sender, mut receiver, control) = class.split_with_control();
    let mut packet = Packet::new();

    loop {
        DEBUG_STREAMING.store(false, Ordering::Relaxed);

        // USB の構成を待ち、PC がポートを開く（DTR）まで待つ。
        // wait_connection() は端点が有効になるまで待つだけで、DTR は待たない
        sender.wait_connection().await;
        while !control.dtr() {
            control.control_changed().await;
        }
        // 前の接続で残ったフレームを捨てる
        while DEBUG_FRAMES.try_receive().is_ok() {}

        // 切断されたら最初に戻り、次の接続は start を受ける前の状態から始める
        let _ = session(&mut sender, &mut receiver, &control, &mut packet).await;
    }
}

/// 1 回の接続の間の処理。接続直後に INFO を送る
async fn session(
    sender: &mut Sender<'static, UsbDriver>,
    receiver: &mut Receiver<'static, UsbDriver>,
    control: &ControlChanged<'static>,
    packet: &mut Packet,
) -> Result<(), Disconnected> {
    let mut rx_buf = [0u8; CDC_PACKET_SIZE as usize];
    let mut line = [0u8; COMMAND_MAX_LEN];
    let mut line_len = 0;
    let mut line_overflow = false;

    write(sender, packet.info(scan_period_us(), dropped())).await?;

    loop {
        let event = select3(
            DEBUG_FRAMES.receive(),
            receiver.read_packet(&mut rx_buf),
            control.control_changed(),
        )
        .await;
        match event {
            Either3::First(frame) => {
                // stop の直後にキューに残っていたフレームは送らない
                if DEBUG_STREAMING.load(Ordering::Relaxed) {
                    write(sender, packet.frame(&frame)).await?;
                }
            }
            Either3::Second(Ok(n)) => {
                // 改行区切りのテキストコマンド（CR / LF のどちらでも区切る）
                for &byte in &rx_buf[..n] {
                    if byte == b'\n' || byte == b'\r' {
                        if line_overflow {
                            write(sender, packet.text(&["error: command too long"])).await?;
                        } else if line_len > 0 {
                            command(sender, packet, &line[..line_len]).await?;
                        }
                        line_len = 0;
                        line_overflow = false;
                    } else if line_len < COMMAND_MAX_LEN {
                        line[line_len] = byte;
                        line_len += 1;
                    } else {
                        line_overflow = true;
                    }
                }
            }
            Either3::Second(Err(_)) => return Err(Disconnected), // USB が切断された
            Either3::Third(()) => {
                if !control.dtr() {
                    return Err(Disconnected); // PC がポートを閉じた
                }
            }
        }
    }
}

/// PC からのコマンドを 1 つ処理する（doc/debug_env.md §5.2）
async fn command(
    sender: &mut Sender<'static, UsbDriver>,
    packet: &mut Packet,
    line: &[u8],
) -> Result<(), Disconnected> {
    let line = core::str::from_utf8(line).unwrap_or("").trim();
    match line {
        "start" => {
            DEBUG_STREAMING.store(true, Ordering::Relaxed);
            write(sender, packet.text(&["ok: start"])).await
        }
        "stop" => {
            DEBUG_STREAMING.store(false, Ordering::Relaxed);
            write(sender, packet.text(&["ok: stop"])).await
        }
        "info" => write(sender, packet.info(scan_period_us(), dropped())).await,
        _ => write(sender, packet.text(&["error: unknown command: ", line])).await,
    }
}

fn scan_period_us() -> u32 {
    (SCAN_PERIOD_MS * 1000) as u32
}

fn dropped() -> u32 {
    DEBUG_DROPPED.load(Ordering::Relaxed)
}

/// パケットを 64 バイトずつ送る。長さが 64 の倍数のときは、区切りとして長さ 0 のパケットを送る
async fn write(sender: &mut Sender<'static, UsbDriver>, data: &[u8]) -> Result<(), Disconnected> {
    for chunk in data.chunks(CDC_PACKET_SIZE as usize) {
        write_packet(sender, chunk).await?;
    }
    if data.len().is_multiple_of(CDC_PACKET_SIZE as usize) {
        write_packet(sender, &[]).await?;
    }
    Ok(())
}

async fn write_packet(
    sender: &mut Sender<'static, UsbDriver>,
    data: &[u8],
) -> Result<(), Disconnected> {
    match with_timeout(
        Duration::from_millis(WRITE_TIMEOUT_MS),
        sender.write_packet(data),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        _ => Err(Disconnected),
    }
}
