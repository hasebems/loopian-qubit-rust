//! シリアルポート（USB CDC）との通信。受信は別スレッドで行い、UI にはバイト列を渡す
//!
//! 記録（.qlog への書き込み）も受信スレッドで行う。UI の描画が遅れても、受信したバイト列を
//! 欠けなくそのまま保存するため
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::qlog;

/// QUBIT の USB の VID / PID（ファームの main.rs）
pub const QUBIT_VID: u16 = 0x1209;
pub const QUBIT_PID: u16 = 0x3691;

const READ_TIMEOUT: Duration = Duration::from_millis(20);

pub struct PortEntry {
    pub name: String,
    pub label: String,
    pub is_qubit: bool,
}

/// 使えるシリアルポートの一覧。QUBIT を先頭にする。
/// macOS では同じポートが /dev/tty.* と /dev/cu.* の 2 つ見えるので、cu.* だけにする
pub fn list_ports() -> Vec<PortEntry> {
    let mut ports: Vec<PortEntry> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !(cfg!(target_os = "macos") && p.port_name.starts_with("/dev/tty.")))
        .map(|p| {
            let (label, is_qubit) = match &p.port_type {
                serialport::SerialPortType::UsbPort(usb) => {
                    let is_qubit = usb.vid == QUBIT_VID && usb.pid == QUBIT_PID;
                    let product = usb.product.clone().unwrap_or_default();
                    (format!("{product} ({})", p.port_name), is_qubit)
                }
                _ => (p.port_name.clone(), false),
            };
            PortEntry {
                name: p.port_name,
                label,
                is_qubit,
            }
        })
        .collect();
    ports.sort_by_key(|p| !p.is_qubit);
    ports
}

/// UI → 受信スレッド
pub enum Command {
    /// ファームへ送る（改行を含めて渡す）
    Write(Vec<u8>),
    StartRecord(PathBuf, qlog::Header),
    StopRecord,
    Close,
}

/// 受信スレッド → UI
pub enum Notice {
    Data(Vec<u8>),
    RecordStarted(PathBuf),
    RecordStopped {
        path: PathBuf,
        bytes: u64,
    },
    RecordError(String),
    /// 切断した（ポートのエラーなど）。これ以降は何も来ない
    Closed(Option<String>),
}

pub struct Link {
    pub port_name: String,
    tx: Sender<Command>,
    pub rx: Receiver<Notice>,
}

impl Link {
    /// ポートを開き、受信スレッドを起こす。ポートを開くと DTR が立ち、ファームは接続とみなす
    pub fn open(port_name: &str, ctx: eframe::egui::Context) -> io::Result<Self> {
        let mut port = serialport::new(port_name, 115_200)
            .timeout(READ_TIMEOUT)
            .open()?;
        port.write_data_terminal_ready(true)?;
        let (tx, cmd_rx) = mpsc::channel();
        let (notice_tx, rx) = mpsc::channel();
        thread::spawn(move || run(port, cmd_rx, notice_tx, ctx));
        let link = Self {
            port_name: port_name.to_string(),
            tx,
            rx,
        };
        // ファームは DTR が立つとすぐ INFO を送るが、macOS の serialport は open の最後に
        // 受信バッファを捨てる（tcflush）ので、その INFO は失われることがある。改めて要求する
        link.send_line("info");
        Ok(link)
    }

    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    /// ファームへテキストコマンドを送る（doc/debug_env.md §5.2）
    pub fn send_line(&self, line: &str) {
        self.send(Command::Write(format!("{line}\n").into_bytes()));
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Close);
    }
}

fn run(
    mut port: Box<dyn serialport::SerialPort>,
    commands: Receiver<Command>,
    notices: Sender<Notice>,
    ctx: eframe::egui::Context,
) {
    let mut record: Option<(PathBuf, qlog::Writer)> = None;
    let mut buf = vec![0u8; 16 * 1024];
    let notify = |n: Notice| {
        let _ = notices.send(n);
        ctx.request_repaint();
    };
    let stop_record = |record: &mut Option<(PathBuf, qlog::Writer)>| {
        if let Some((path, writer)) = record.take() {
            match writer.finish() {
                Ok(bytes) => notify(Notice::RecordStopped { path, bytes }),
                Err(e) => notify(Notice::RecordError(e.to_string())),
            }
        }
    };

    let reason = loop {
        // UI からの指示
        let mut close = false;
        loop {
            match commands.try_recv() {
                Ok(Command::Write(bytes)) => {
                    if let Err(e) = port.write_all(&bytes) {
                        stop_record(&mut record);
                        notify(Notice::Closed(Some(e.to_string())));
                        return;
                    }
                }
                Ok(Command::StartRecord(path, header)) => {
                    stop_record(&mut record);
                    match qlog::Writer::create(&path, &header) {
                        Ok(writer) => {
                            notify(Notice::RecordStarted(path.clone()));
                            record = Some((path, writer));
                        }
                        Err(e) => notify(Notice::RecordError(e.to_string())),
                    }
                }
                Ok(Command::StopRecord) => stop_record(&mut record),
                Ok(Command::Close) | Err(TryRecvError::Disconnected) => {
                    close = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if close {
            break None;
        }

        // 受信
        match port.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                if let Some((_, writer)) = record.as_mut()
                    && let Err(e) = writer.write(&buf[..n])
                {
                    notify(Notice::RecordError(e.to_string()));
                    record = None;
                }
                notify(Notice::Data(buf[..n].to_vec()));
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => break Some(e.to_string()),
        }
    };
    stop_record(&mut record);
    // ポートを閉じると DTR が落ち、ファームは切断とみなす
    drop(port);
    notify(Notice::Closed(reason));
}
