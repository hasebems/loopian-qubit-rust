//! ファームとの通信を確かめる（GUI を使わない）。PC アプリと同じ受信スレッド・Parser・Store を使う
//!
//! 実行: `cd tools/qubit_monitor && cargo run --example probe [ポート]`
//! ポートを省くと、見つかった QUBIT を使う。
//! INFO → start → FRAME → period → mark → 不正なコマンド → stop の順に確かめ、結果を表示する
use std::time::{Duration, Instant};

use qubit_monitor::playback::Player;
use qubit_monitor::protocol::{EVENT_MARKER, Packet, Parser};
use qubit_monitor::serial::{self, Command, Link, Notice};
use qubit_monitor::store::Store;
use qubit_monitor::{csv_export, qlog};

struct Probe {
    link: Link,
    parser: Parser,
    store: Store,
    log: Vec<Packet>, // FRAME 以外に受け取ったもの
}

impl Probe {
    /// duration の間受信し、パケットを Store に入れる
    fn run(&mut self, duration: Duration) {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            if let Ok(notice) = self.link.rx.recv_timeout(Duration::from_millis(20)) {
                match notice {
                    Notice::Data(bytes) => {
                        let mut packets = Vec::new();
                        self.parser.push(&bytes, &mut packets);
                        for p in packets {
                            if !matches!(p, Packet::Frame(_)) {
                                println!("  受信: {p:?}");
                                self.log.push(p.clone());
                            }
                            self.store.ingest(p);
                        }
                    }
                    Notice::Closed(reason) => panic!("切断されました: {reason:?}"),
                    Notice::RecordStarted(path) => println!("  記録開始: {}", path.display()),
                    Notice::RecordStopped { path, bytes } => {
                        println!("  記録終了: {} ({bytes} バイト)", path.display())
                    }
                    Notice::RecordError(e) => panic!("記録のエラー: {e}"),
                }
            }
        }
    }

    fn send(&self, line: &str) {
        println!("> {line}");
        self.link.send_line(line);
    }

    /// 直前の measure 以降のフレームの数と間隔
    fn summarize(&self, label: &str, from_frame: usize) {
        let n = self.store.time_us.len();
        let frames = n.saturating_sub(from_frame);
        if frames < 2 {
            println!("[{label}] フレーム {frames}");
            return;
        }
        let (t0, t1) = (self.store.time_us[from_frame], self.store.time_us[n - 1]);
        let stats = self.store.stats(t0, t1, false);
        let iv = stats.interval_us;
        println!(
            "[{label}] フレーム {frames}  欠け {}  間隔 最小 {:.2} / 平均 {:.3} / 最大 {:.2} ms",
            stats.lost_frames,
            iv.min as f64 / 1000.0,
            iv.mean / 1000.0,
            iv.max as f64 / 1000.0
        );
        let hist: Vec<String> = stats
            .interval_hist
            .iter()
            .map(|(lo, n)| format!("{:.2}ms:{n}", *lo as f64 / 1000.0))
            .collect();
        println!("    間隔の分布: {}", hist.join("  "));
        for (k, ks) in stats.keys.iter().enumerate() {
            let changes =
                qubit_monitor::store::Summary::of(&self.store.change_intervals(t0, t1, k));
            println!(
                "    key{k}: 平均 {:.1} σ {:.2} p-p {} ずれ {}  値の更新間隔 平均 {:.2} ms ({} 回)",
                ks.mean,
                ks.std,
                ks.peak_to_peak,
                ks.hi_lo_jumps,
                changes.mean / 1000.0,
                changes.count
            );
        }
    }
}

fn main() {
    let port = std::env::args().nth(1).or_else(|| {
        serial::list_ports()
            .into_iter()
            .find(|p| p.is_qubit)
            .map(|p| p.name)
    });
    let Some(port) = port else {
        eprintln!("QUBIT のシリアルポートが見つかりません");
        std::process::exit(1);
    };
    println!("ポート: {port}");
    let link = Link::open(&port, eframe::egui::Context::default()).expect("ポートを開けません");
    let mut p = Probe {
        link,
        parser: Parser::new(),
        store: Store::new(),
        log: Vec::new(),
    };

    println!("-- 接続直後（INFO が来るはず）");
    p.run(Duration::from_millis(500));

    // 以降を記録し、最後に再生・CSV 書き出しして受信の結果と比べる
    let qlog_path = std::env::temp_dir().join("qubit_probe.qlog");
    p.link.send(Command::StartRecord(
        qlog_path.clone(),
        qlog::Header::now(p.store.info.as_ref()),
    ));
    let frames_before_record = p.store.frames_total;

    println!("-- start");
    p.send("start");
    p.run(Duration::from_millis(200));
    let from = p.store.time_us.len();
    p.run(Duration::from_secs(2));
    p.summarize("既定の周期", from);

    println!("-- period 5000");
    p.send("period 5000");
    p.run(Duration::from_millis(300));
    let from = p.store.time_us.len();
    p.run(Duration::from_secs(1));
    p.summarize("5ms", from);

    println!("-- period 20000");
    p.send("period 20000");
    p.run(Duration::from_millis(300));
    let from = p.store.time_us.len();
    p.run(Duration::from_secs(1));
    p.summarize("20ms", from);

    println!("-- period 2000 に戻す");
    p.send("period 2000");
    p.run(Duration::from_millis(300));

    println!("-- mark 7");
    p.send("mark 7");
    p.run(Duration::from_millis(200));
    let marker = p
        .store
        .events
        .iter()
        .any(|(_, e)| e.kind == EVENT_MARKER && e.marker_number() == 7);
    println!("  マーカー 7 を受信: {marker}");

    println!("-- 不正なコマンド");
    p.send("period 3000");
    p.send("hello");
    p.send("start now");
    p.run(Duration::from_millis(300));

    println!("-- stop");
    p.send("stop");
    p.run(Duration::from_millis(300));
    let before = p.store.frames_total;
    p.run(Duration::from_millis(500));
    println!(
        "  stop の後に届いたフレーム: {}",
        p.store.frames_total - before
    );

    println!("-- info");
    p.send("info");
    p.run(Duration::from_millis(300));

    println!("-- 記録を止めて、再生・CSV と比べる");
    p.link.send(Command::StopRecord);
    p.run(Duration::from_millis(200));
    let recorded_frames = p.store.frames_total - frames_before_record;
    let mut player = Player::open(&qlog_path).expect("記録を開けません");
    let mut replay = Store::new();
    while !player.is_finished() {
        player.step(&mut replay);
    }
    println!(
        "  受信したフレーム {recorded_frames} / 再生したフレーム {}  再生の CRC エラー {}  ヘッダ {:?}",
        replay.frames_total, player.crc_errors, player.header
    );
    let csv_path = qlog_path.with_extension("csv");
    let rows = csv_export::export(&qlog_path, &csv_path).expect("CSV を書き出せません");
    let text = std::fs::read_to_string(&csv_path).unwrap();
    println!("  CSV {rows} 行: {}", csv_path.display());
    for line in text.lines().take(3) {
        println!("    {line}");
    }
    for line in text
        .lines()
        .filter(|l| l.ends_with("marker 7") || l.contains(",info "))
        .take(3)
    {
        println!("    {line}");
    }

    println!(
        "== 合計: フレーム {}  欠け {}  CRC エラー {}  長さ違い {}  読み飛ばし {} バイト  ずれ補正 {}",
        p.store.frames_total,
        p.store.lost_frames,
        p.parser.crc_errors,
        p.parser.malformed,
        p.parser.skipped_bytes,
        p.store.hi_lo_corrections
    );
    println!("   FRAME 以外に受け取ったパケット: {}", p.log.len());
}
