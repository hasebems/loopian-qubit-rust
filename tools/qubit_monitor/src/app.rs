//! 画面（doc/debug_env.md §4.2）
//!
//! 上: 接続・記録・開始/停止・マーカー・周期、再生の操作、受信の状態
//! 左: 表示するキー・系列・時間幅
//! 中央: 時系列グラフ（イベントを縦線で重ねる）。上段は raw と filtered・baseline、下段は delta・output
//! 右: アルゴリズム（touch_algo）の設定と、統計（表示している時間の範囲で計算する）
//! 下: 全キーの今の値のバー表示
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use egui_plot::{
    Bar, BarChart, HLine, Legend, Line, LineStyle, MarkerShape, Plot, PlotPoints, Points, VLine,
};
use touch_algo::{Params, State};

use qubit_monitor::algo::{self, AlgoRunner};
use qubit_monitor::csv_export;
use qubit_monitor::playback::{self, Player};
use qubit_monitor::protocol::{
    EVENT_MARKER, EVENT_NOTE_MOVED, EVENT_NOTE_OFF, EVENT_NOTE_ON, Packet, Parser,
};
use qubit_monitor::qlog;
use qubit_monitor::serial::{self, Command, Link, Notice, PortEntry};
use qubit_monitor::store::{self, Store};

/// ファームの period コマンドで選べる周期（µs）。ファームの SCAN_PERIODS_US と同じ
const PERIODS_US: [u32; 4] = [2_000, 5_000, 10_000, 20_000];
/// 1 本の折れ線に描く点の上限の目安。超えたら区間毎の最小・最大に間引く
const MAX_PLOT_POINTS: usize = 4000;
const REPAINT_INTERVAL: Duration = Duration::from_millis(33);

const KEY_COLORS: [Color32; 8] = [
    Color32::from_rgb(0x4e, 0x79, 0xa7),
    Color32::from_rgb(0xf2, 0x8e, 0x2b),
    Color32::from_rgb(0x59, 0xa1, 0x4f),
    Color32::from_rgb(0xe1, 0x57, 0x59),
    Color32::from_rgb(0x76, 0xb7, 0xb2),
    Color32::from_rgb(0xed, 0xc9, 0x48),
    Color32::from_rgb(0xb0, 0x7a, 0xa1),
    Color32::from_rgb(0x9c, 0x75, 0x5f),
];

fn key_color(key: usize) -> Color32 {
    KEY_COLORS[key % KEY_COLORS.len()]
}

pub struct MonitorApp {
    ports: Vec<PortEntry>,
    selected_port: Option<String>,
    link: Option<Link>,
    parser: Parser,
    store: Store,
    player: Option<Player>,

    // 操作の状態
    streaming_wanted: bool, // 開始を押した（ファームが INFO を送り直したら start を送り直す）
    recording: Option<PathBuf>,
    marker_count: u32,
    rate: RateMeter,
    message: String, // 最後の操作の結果など

    // 表示
    show_keys: Vec<bool>,
    show_raw: bool,
    show_corrected: bool,
    show_events: bool,
    span_s: f64,
    paused: bool,
    view_us: Option<(u64, u64)>, // 前回描いたグラフの時間の範囲（一時停止中の統計・描画に使う）
    stats_key: usize,            // 値の更新間隔を調べるキー
    bars_minus_min: bool,        // バー表示で、表示範囲の最小値を引く

    // アルゴリズム（touch_algo、doc/debug_env.md §4.6）
    algo: AlgoRunner,
    algo_enabled: bool,
    show_filtered: bool,
    show_baseline: bool,
    show_delta: bool,
    show_output: bool,
    bars_output: bool, // バー表示に output を出す
}

impl MonitorApp {
    pub fn new() -> Self {
        let ports = serial::list_ports();
        let selected_port = ports.first().map(|p| p.name.clone());
        Self {
            ports,
            selected_port,
            link: None,
            parser: Parser::new(),
            store: Store::new(),
            player: None,
            streaming_wanted: false,
            recording: None,
            marker_count: 0,
            rate: RateMeter::default(),
            message: String::new(),
            show_keys: Vec::new(),
            show_raw: true,
            show_corrected: false,
            show_events: true,
            span_s: 5.0,
            paused: false,
            view_us: None,
            stats_key: 0,
            bars_minus_min: true,
            algo: AlgoRunner::new(),
            algo_enabled: true,
            show_filtered: true,
            show_baseline: true,
            show_delta: true,
            show_output: true,
            bars_output: false,
        }
    }

    /// データを捨てて、受信・再生の状態を最初に戻す
    fn reset_data(&mut self) {
        self.parser = Parser::new();
        self.store = Store::new();
        self.view_us = None;
        self.paused = false;
        self.algo.clear();
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let Some(port) = self.selected_port.clone() else {
            return;
        };
        self.player = None;
        self.reset_data();
        self.streaming_wanted = false;
        self.marker_count = 0;
        match Link::open(&port, ctx.clone()) {
            Ok(link) => {
                self.link = Some(link);
                self.message = format!("{port} に接続しました");
            }
            Err(e) => self.message = format!("{port} を開けません: {e}"),
        }
    }

    fn disconnect(&mut self) {
        // Link を捨てると受信スレッドが記録を閉じ、ポートを閉じる（DTR が落ちてファームは切断とみなす）
        self.link = None;
        self.recording = None;
        self.streaming_wanted = false;
        self.message = "切断しました".into();
    }

    /// 受信スレッドからの通知を処理し、受信したパケットを Store に入れる
    fn poll(&mut self) {
        let Some(link) = &self.link else {
            return;
        };
        let notices: Vec<Notice> = link.rx.try_iter().collect();
        let mut packets = Vec::new();
        let mut closed = None;
        for notice in notices {
            match notice {
                Notice::Data(bytes) => self.parser.push(&bytes, &mut packets),
                Notice::RecordStarted(path) => {
                    self.message = format!("記録を始めました: {}", path.display());
                    self.recording = Some(path);
                }
                Notice::RecordStopped { path, bytes } => {
                    self.message = format!("記録を止めました: {} ({bytes} バイト)", path.display());
                    self.recording = None;
                }
                Notice::RecordError(e) => {
                    self.message = format!("記録のエラー: {e}");
                    self.recording = None;
                }
                Notice::Closed(reason) => closed = Some(reason),
            }
        }
        for packet in packets {
            match &packet {
                Packet::Frame(_) => self.rate.count(),
                // ファームは切断・接続し直したとき（書き込みのタイムアウトなど）に INFO を送り、
                // start を受ける前の状態に戻る。開始中なら start を送り直す（doc/debug_env.md §6.1）
                Packet::Info(_) if self.streaming_wanted => {
                    if let Some(link) = &self.link {
                        link.send_line("start");
                    }
                }
                _ => {}
            }
            self.store.ingest(packet);
        }
        if let Some(reason) = closed {
            self.link = None;
            self.recording = None;
            self.streaming_wanted = false;
            self.message = match reason {
                Some(e) => format!("切断されました: {e}"),
                None => "切断しました".into(),
            };
        }
    }

    fn toggle_record(&mut self) {
        let Some(link) = &self.link else {
            return;
        };
        if self.recording.is_some() {
            link.send(Command::StopRecord);
            return;
        }
        let name = format!(
            "qubit_{}.qlog",
            chrono::Local::now().format("%Y%m%d_%H%M%S")
        );
        let Some(path) = rfd::FileDialog::new()
            .add_filter("qlog", &["qlog"])
            .set_file_name(name)
            .save_file()
        else {
            return;
        };
        let header = qlog::Header::now(self.store.info.as_ref());
        link.send(Command::StartRecord(path, header));
        // 記録の中にも INFO（周期など）が入るように、記録を始めた直後に要求する
        link.send_line("info");
    }

    fn open_playback(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("qlog", &["qlog"])
            .pick_file()
        else {
            return;
        };
        match Player::open(&path) {
            Ok(mut player) => {
                // 再生中は実機からの受信を止める（データが混ざらないように）
                if self.link.is_some() {
                    self.disconnect();
                }
                self.reset_data();
                player.set_playing(true);
                self.message = format!(
                    "{} を再生します（記録 {}、{:.1} 秒）",
                    path.display(),
                    player.header.recorded_at_text(),
                    player.duration_us() as f64 / 1e6
                );
                self.player = Some(player);
            }
            Err(e) => self.message = format!("{} を開けません: {e}", path.display()),
        }
    }

    fn export_csv(&mut self) {
        let mut dialog = rfd::FileDialog::new().add_filter("qlog", &["qlog"]);
        if let Some(player) = &self.player {
            dialog = dialog.set_file_name(
                player
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
            );
            if let Some(dir) = player.path.parent() {
                dialog = dialog.set_directory(dir);
            }
        }
        let Some(qlog_path) = dialog.pick_file() else {
            return;
        };
        let Some(csv_path) = rfd::FileDialog::new()
            .add_filter("csv", &["csv"])
            .set_file_name(
                qlog_path
                    .with_extension("csv")
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
            )
            .save_file()
        else {
            return;
        };
        self.message = match csv_export::export(&qlog_path, &csv_path) {
            Ok(rows) => format!("CSV に書き出しました: {} ({rows} 行)", csv_path.display()),
            Err(e) => format!("CSV の書き出しに失敗しました: {e}"),
        };
    }

    // ------------------------------------------------------------------
    //  上: 操作と状態
    // ------------------------------------------------------------------
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let connected = self.link.is_some();
            ui.add_enabled_ui(!connected, |ui| {
                let selected = self
                    .ports
                    .iter()
                    .find(|p| Some(&p.name) == self.selected_port.as_ref())
                    .map(|p| p.label.clone())
                    .unwrap_or_else(|| "（ポートなし）".into());
                egui::ComboBox::from_id_salt("port")
                    .width(280.0)
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for p in &self.ports {
                            ui.selectable_value(
                                &mut self.selected_port,
                                Some(p.name.clone()),
                                &p.label,
                            );
                        }
                    });
                if ui.button("更新").clicked() {
                    self.ports = serial::list_ports();
                    if self
                        .selected_port
                        .as_ref()
                        .is_none_or(|s| !self.ports.iter().any(|p| &p.name == s))
                    {
                        self.selected_port = self.ports.first().map(|p| p.name.clone());
                    }
                }
            });
            if connected {
                if ui.button("切断").clicked() {
                    self.disconnect();
                }
            } else if ui
                .add_enabled(self.selected_port.is_some(), egui::Button::new("接続"))
                .clicked()
            {
                self.connect(ui.ctx());
            }
            ui.separator();

            ui.add_enabled_ui(connected, |ui| {
                let record_label = if self.recording.is_some() {
                    RichText::new("■ 記録停止").color(Color32::RED)
                } else {
                    RichText::new("● 記録")
                };
                if ui.button(record_label).clicked() {
                    self.toggle_record();
                }
                let start_label = if self.streaming_wanted {
                    "■ 停止"
                } else {
                    "▶ 開始"
                };
                if ui.button(start_label).clicked()
                    && let Some(link) = &self.link
                {
                    self.streaming_wanted = !self.streaming_wanted;
                    link.send_line(if self.streaming_wanted {
                        "start"
                    } else {
                        "stop"
                    });
                }
                if ui.button("マーカー").clicked()
                    && let Some(link) = &self.link
                {
                    self.marker_count += 1;
                    link.send_line(&format!("mark {}", self.marker_count));
                }
                ui.label("周期:");
                let current = self.store.info.as_ref().map(|i| i.scan_period_us);
                let text = current.map_or("?".into(), |us| format!("{}ms", us as f64 / 1000.0));
                egui::ComboBox::from_id_salt("period")
                    .width(60.0)
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        for us in PERIODS_US {
                            if ui
                                .selectable_label(current == Some(us), format!("{}ms", us / 1000))
                                .clicked()
                                && let Some(link) = &self.link
                            {
                                // 応答の INFO で表示が新しい周期に変わる
                                link.send_line(&format!("period {us}"));
                            }
                        }
                    });
            });
            ui.separator();

            if ui.button("記録を開く").clicked() {
                self.open_playback();
            }
            if ui.button("CSV 書き出し").clicked() {
                self.export_csv();
            }
        });

        if self.player.is_some() {
            self.playback_bar(ui);
        }

        // 受信の状態
        ui.horizontal_wrapped(|ui| {
            let s = &self.store;
            ui.label(format!("受信: {:.0} fr/s", self.rate.per_second()));
            ui.separator();
            ui.label(format!("取りこぼし: {}", s.lost_frames));
            ui.separator();
            let crc = self.parser.crc_errors + self.parser.malformed;
            let crc = crc + self.player.as_ref().map_or(0, |p| p.crc_errors);
            ui.label(format!("CRC エラー: {crc}"));
            ui.separator();
            ui.label(format!("ずれ補正: {}", s.hi_lo_corrections));
            ui.separator();
            if let Some(info) = &s.info {
                ui.label(format!("キュー溢れ(ファーム): {}", info.dropped));
                ui.separator();
                ui.label(format!(
                    "ファーム {} ({})  {} キー  周期 {}µs",
                    info.version, info.build_date, info.nkeys, info.scan_period_us
                ));
                ui.separator();
            }
            if let Some(path) = &self.recording {
                ui.label(
                    RichText::new(format!("● 記録中: {}", path.display())).color(Color32::RED),
                );
                ui.separator();
            }
            ui.label(&self.message);
        });
    }

    fn playback_bar(&mut self, ui: &mut egui::Ui) {
        let Some(player) = &mut self.player else {
            return;
        };
        let mut rewind = false;
        let mut close = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("再生").strong());
            let label = if player.playing {
                "⏸ 一時停止"
            } else {
                "▶ 再生"
            };
            if ui.button(label).clicked() {
                let playing = !player.playing;
                player.set_playing(playing);
            }
            if ui.button("コマ送り").clicked() {
                player.step(&mut self.store);
            }
            if ui.button("最初から").clicked() {
                rewind = true;
            }
            ui.label("速さ:");
            egui::ComboBox::from_id_salt("speed")
                .width(60.0)
                .selected_text(format!("×{}", player.speed))
                .show_ui(ui, |ui| {
                    for speed in playback::SPEEDS {
                        if ui
                            .selectable_label(player.speed == speed, format!("×{speed}"))
                            .clicked()
                        {
                            player.set_speed(speed);
                        }
                    }
                });
            ui.label(format!(
                "{:.1} / {:.1} 秒",
                player.position_us() as f64 / 1e6,
                player.duration_us() as f64 / 1e6
            ));
            ui.separator();
            let h = &player.header;
            ui.label(format!(
                "{}  記録 {}  ファーム {} ({})  {} キー",
                player
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                h.recorded_at_text(),
                h.version,
                h.build_date,
                h.nkeys
            ));
            if ui.button("閉じる").clicked() {
                close = true;
            }
        });
        if close {
            self.player = None;
        } else if rewind && let Some(player) = &mut self.player {
            player.rewind();
            self.parser = Parser::new();
            self.store = Store::new();
            self.view_us = None;
            // 校正のスイッチの記録は残し、同じ時刻で切り替える
            self.algo.invalidate();
        }
    }

    // ------------------------------------------------------------------
    //  左: 表示の設定
    // ------------------------------------------------------------------
    fn view_panel(&mut self, ui: &mut egui::Ui) {
        let nkeys = self.store.nkeys;
        if self.show_keys.len() != nkeys {
            // 既定は最初の 6 キーを表示する
            self.show_keys = (0..nkeys).map(|k| k < 6).collect();
            self.stats_key = self.stats_key.min(nkeys.saturating_sub(1));
        }
        ui.heading("表示");
        ui.label("キー");
        ui.horizontal(|ui| {
            if ui.small_button("全部").clicked() {
                self.show_keys.iter_mut().for_each(|s| *s = true);
            }
            if ui.small_button("なし").clicked() {
                self.show_keys.iter_mut().for_each(|s| *s = false);
            }
        });
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                egui::Grid::new("keys").show(ui, |ui| {
                    for k in 0..nkeys {
                        ui.checkbox(
                            &mut self.show_keys[k],
                            RichText::new(format!("{k}")).color(key_color(k)),
                        );
                        if k % 4 == 3 {
                            ui.end_row();
                        }
                    }
                });
            });
        ui.separator();
        ui.label("系列");
        ui.checkbox(&mut self.show_raw, "raw（生値）");
        ui.checkbox(&mut self.show_corrected, "補正後（hi/lo ずれ）");
        ui.checkbox(&mut self.show_events, "イベント");
        ui.add_enabled_ui(self.algo_enabled, |ui| {
            ui.checkbox(&mut self.show_filtered, "filtered（ノイズ除去後）");
            ui.checkbox(&mut self.show_baseline, "baseline（基準値）");
            ui.label("下段");
            ui.checkbox(&mut self.show_delta, "delta（filtered − baseline）");
            ui.checkbox(&mut self.show_output, "output（delta − noise_floor）");
        });
        ui.separator();
        ui.label("表示する時間幅 [秒]");
        ui.add(egui::Slider::new(&mut self.span_s, 1.0..=30.0).logarithmic(true));
        let label = if self.paused {
            "▶ 表示を再開"
        } else {
            "⏸ 表示を一時停止"
        };
        if ui.button(label).clicked() {
            self.paused = !self.paused;
        }
        if self.paused {
            ui.label("一時停止中も受信と記録は続けます。グラフはドラッグ・拡大できます");
        }
        ui.separator();
        ui.checkbox(&mut self.bars_minus_min, "バー表示で表示範囲の最小値を引く");
        ui.add_enabled_ui(self.algo_enabled, |ui| {
            ui.checkbox(&mut self.bars_output, "バー表示に output を出す");
        });
    }

    /// 今描く時間の範囲（µs）
    fn window_us(&self) -> Option<(u64, u64)> {
        let latest = self.store.latest_time_us()?;
        if self.paused
            && let Some(view) = self.view_us
        {
            return Some(view);
        }
        let span = (self.span_s * 1e6) as u64;
        Some((latest.saturating_sub(span), latest))
    }

    // ------------------------------------------------------------------
    //  中央: 時系列グラフ
    // ------------------------------------------------------------------
    fn plot(&mut self, ui: &mut egui::Ui) {
        let Some((t0, t1)) = self.window_us() else {
            ui.centered_and_justified(|ui| {
                ui.label("データがありません。接続して「▶ 開始」を押すか、記録を開いてください");
            });
            return;
        };
        let shown = |k: usize| self.show_keys.get(k).copied().unwrap_or(false);
        let algo_on = self.algo_enabled && self.algo.filtered.len() == self.store.nkeys;
        let algo_range = self.algo.range(t0, t1);

        // 上段: raw・補正後・filtered・baseline
        let range = self.store.range(t0, t1);
        let mut lines = Vec::new();
        for k in (0..self.store.nkeys).filter(|&k| shown(k)) {
            let store = &self.store;
            let valid = |i: usize| store.is_valid(i, k);
            if self.show_raw {
                let raw = &store.raw[k];
                let points = decimate(&store.time_us, |i| raw[i] as f64, valid, range.clone());
                lines.push(
                    Line::new(format!("key{k}"), PlotPoints::new(points)).color(key_color(k)),
                );
            }
            if self.show_corrected {
                let corrected = &store.corrected[k];
                let points = decimate(
                    &store.time_us,
                    |i| corrected[i] as f64,
                    valid,
                    range.clone(),
                );
                lines.push(
                    Line::new(format!("key{k} 補正後"), PlotPoints::new(points))
                        .color(key_color(k))
                        .style(LineStyle::dashed_loose()),
                );
            }
            if algo_on && self.show_filtered {
                let filtered = &self.algo.filtered[k];
                let points = decimate(
                    &self.algo.time_us,
                    |i| filtered[i] as f64,
                    |_| true,
                    algo_range.clone(),
                );
                lines.push(
                    Line::new(format!("key{k} filtered"), PlotPoints::new(points))
                        .color(key_color(k))
                        .width(2.0),
                );
            }
            if algo_on && self.show_baseline {
                let baseline = &self.algo.baseline[k];
                let points = decimate(
                    &self.algo.time_us,
                    |i| baseline[i] as f64,
                    |_| true,
                    algo_range.clone(),
                );
                lines.push(
                    Line::new(format!("key{k} baseline"), PlotPoints::new(points))
                        .color(key_color(k))
                        .style(LineStyle::dotted_dense())
                        .width(2.0),
                );
            }
        }
        let vlines = self.event_lines(t0, t1);

        // 下段: delta・output と、しきい値・onset
        let lower = algo_on && (self.show_delta || self.show_output);
        let mut lower_lines = Vec::new();
        let mut onset_points = Vec::new();
        if lower {
            for k in (0..self.store.nkeys).filter(|&k| shown(k)) {
                if self.show_delta {
                    let delta = &self.algo.delta[k];
                    let points = decimate(
                        &self.algo.time_us,
                        |i| delta[i] as f64,
                        |_| true,
                        algo_range.clone(),
                    );
                    lower_lines.push(
                        Line::new(format!("key{k} delta"), PlotPoints::new(points))
                            .color(key_color(k)),
                    );
                }
                if self.show_output {
                    let output = &self.algo.output[k];
                    let points = decimate(
                        &self.algo.time_us,
                        |i| output[i] as f64,
                        |_| true,
                        algo_range.clone(),
                    );
                    lower_lines.push(
                        Line::new(format!("key{k} output"), PlotPoints::new(points))
                            .color(key_color(k))
                            .style(LineStyle::dashed_loose()),
                    );
                }
                // onset を記録した時刻に、そのときの delta の位置で印を付ける
                let marks: Vec<[f64; 2]> = self
                    .algo
                    .onsets
                    .iter()
                    .filter(|(t, key)| *key == k && (t0..=t1).contains(t))
                    .filter_map(|(t, _)| {
                        let i = self.algo.time_us.partition_point(|&x| x < *t);
                        (i < self.algo.time_us.len())
                            .then(|| [*t as f64 / 1e6, self.algo.delta[k][i] as f64])
                    })
                    .collect();
                if !marks.is_empty() {
                    onset_points.push(
                        Points::new(format!("key{k} onset"), PlotPoints::new(marks))
                            .color(key_color(k))
                            .shape(MarkerShape::Diamond)
                            .filled(true)
                            .radius(5.0),
                    );
                }
            }
        }

        let paused = self.paused;
        let upper_height = if lower {
            ui.available_height() * 0.55
        } else {
            ui.available_height()
        };
        let response = Plot::new("timeseries")
            .legend(Legend::default())
            .height(upper_height)
            .link_axis("time_axis", [true, false])
            .link_cursor("time_axis", [true, false])
            .x_axis_label("時刻 [秒]（ファームの起動から）")
            .allow_drag(paused)
            .allow_zoom(paused)
            .allow_scroll(paused)
            .show(ui, |plot_ui| {
                if !paused {
                    plot_ui.set_plot_bounds_x(t0 as f64 / 1e6..=t1 as f64 / 1e6);
                    plot_ui.set_auto_bounds([false, true]);
                }
                for line in lines {
                    plot_ui.line(line);
                }
                for vline in vlines.clone() {
                    plot_ui.vline(vline);
                }
            });
        let bounds = response.transform.bounds();
        let (x0, x1) = (bounds.min()[0].max(0.0), bounds.max()[0].max(0.0));
        self.view_us = Some(((x0 * 1e6) as u64, (x1 * 1e6) as u64));

        if lower {
            let params = self.algo.params;
            let onset_default = params.noise_floor_default as f64 + params.onset_margin as f64;
            Plot::new("algo")
                .legend(Legend::default())
                .link_axis("time_axis", [true, false])
                .link_cursor("time_axis", [true, false])
                .x_axis_label("時刻 [秒]")
                .allow_drag(paused)
                .allow_zoom(paused)
                .allow_scroll(paused)
                .show(ui, |plot_ui| {
                    if !paused {
                        plot_ui.set_plot_bounds_x(t0 as f64 / 1e6..=t1 as f64 / 1e6);
                        plot_ui.set_auto_bounds([false, true]);
                    }
                    let gray = Color32::GRAY;
                    plot_ui.hline(
                        HLine::new("QUIET_THRESHOLD", params.quiet_threshold as f64).color(gray),
                    );
                    plot_ui.hline(
                        HLine::new("ACTIVE_THRESHOLD", params.active_threshold as f64)
                            .color(gray)
                            .style(LineStyle::dashed_loose()),
                    );
                    plot_ui.hline(
                        HLine::new("onset しきい値（noise_floor 既定値）", onset_default)
                            .color(gray)
                            .style(LineStyle::dotted_dense()),
                    );
                    for line in lower_lines {
                        plot_ui.line(line);
                    }
                    for points in onset_points {
                        plot_ui.points(points);
                    }
                    for vline in vlines {
                        plot_ui.vline(vline);
                    }
                });
        }
    }

    /// 表示範囲のイベントの縦線
    fn event_lines(&self, t0: u64, t1: u64) -> Vec<VLine> {
        if !self.show_events {
            return Vec::new();
        }
        self.store
            .events
            .iter()
            .filter(|(t, _)| (t0..=t1).contains(t))
            .map(|(t, event)| {
                let (name, color) = match event.kind {
                    EVENT_NOTE_ON => ("Note On", Color32::from_rgb(0x2c, 0xa0, 0x2c)),
                    EVENT_NOTE_OFF => ("Note Off", Color32::from_rgb(0xd6, 0x27, 0x28)),
                    EVENT_NOTE_MOVED => ("Note Moved", Color32::from_rgb(0xff, 0x7f, 0x0e)),
                    EVENT_MARKER => ("マーカー", Color32::from_rgb(0x1f, 0x77, 0xb4)),
                    _ => ("その他", Color32::GRAY),
                };
                let width = if event.kind == EVENT_MARKER { 2.5 } else { 1.0 };
                VLine::new(name, *t as f64 / 1e6).color(color).width(width)
            })
            .collect()
    }

    // ------------------------------------------------------------------
    //  右: アルゴリズム（touch_algo）の設定
    // ------------------------------------------------------------------
    fn algo_panel(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(RichText::new("アルゴリズム（touch_algo）").heading())
            .default_open(true)
            .show(ui, |ui| {
                if ui
                    .checkbox(&mut self.algo_enabled, "アルゴリズムを通す")
                    .changed()
                {
                    self.algo.invalidate();
                }
                if !self.algo_enabled {
                    return;
                }
                ui.horizontal(|ui| {
                    ui.label("間引き:");
                    let label = |us: u32| {
                        if us == 0 {
                            "間引かない".to_string()
                        } else {
                            format!("{}ms", us / 1000)
                        }
                    };
                    egui::ComboBox::from_id_salt("decimation")
                        .width(90.0)
                        .selected_text(label(self.algo.decimation_us))
                        .show_ui(ui, |ui| {
                            for us in algo::DECIMATIONS_US {
                                if ui
                                    .selectable_label(self.algo.decimation_us == us, label(us))
                                    .clicked()
                                    && self.algo.decimation_us != us
                                {
                                    self.algo.decimation_us = us;
                                    self.algo.invalidate();
                                }
                            }
                        });
                });
                ui.horizontal(|ui| {
                    let mut on = self.algo.calibrating;
                    if ui.toggle_value(&mut on, "設定画面（校正）").changed() {
                        self.algo.set_calibrating(on, &self.store);
                    }
                    let state = match self.algo.state() {
                        Some(State::Acquiring) => "初期取得中",
                        Some(State::Running) => "通常",
                        Some(State::Calibrating) => "校正中",
                        None => "-",
                    };
                    ui.label(format!("状態: {state}"));
                });
                ui.label("校正は PC のアルゴリズムだけのもの（実機の設定画面とは連動しない）");

                // キー毎の最後の値
                if let Some(last) = self.algo.time_us.len().checked_sub(1) {
                    egui::Grid::new("algo_keys").striped(true).show(ui, |ui| {
                        for h in ["キー", "baseline", "delta", "output", "noise"] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        for k in 0..self.algo.baseline.len() {
                            ui.label(RichText::new(format!("{k}")).color(key_color(k)));
                            ui.label(format!("{}", self.algo.baseline[k][last]));
                            ui.label(format!("{}", self.algo.delta[k][last]));
                            ui.label(format!("{}", self.algo.output[k][last]));
                            ui.label(format!("{}", self.algo.noise_floor[k]));
                            ui.end_row();
                        }
                    });
                }

                ui.collapsing("パラメータ（doc/touch_baseline.md §3.11）", |ui| {
                    let mut p = self.algo.params;
                    egui::Grid::new("params").show(ui, |ui| {
                        param(
                            ui,
                            "SMOOTH_SAMPLES",
                            &mut p.smooth_samples,
                            1..=touch_algo::MAX_SMOOTH_SAMPLES as u8,
                        );
                        param(
                            ui,
                            "BASELINE_UPDATE_INTERVAL_MS",
                            &mut p.baseline_update_interval_ms,
                            1..=1000,
                        );
                        param(ui, "ACQUIRE_MS", &mut p.acquire_ms, 0..=5000);
                        param(ui, "NEIGHBOR_RANGE", &mut p.neighbor_range, 0..=8);
                        param(ui, "QUIET_THRESHOLD", &mut p.quiet_threshold, 0..=200);
                        param(ui, "ACTIVE_THRESHOLD", &mut p.active_threshold, 0..=500);
                        param(ui, "ONSET_MARGIN", &mut p.onset_margin, 0..=100);
                        param(ui, "QUIET_HOLD_MS", &mut p.quiet_hold_ms, 0..=10_000);
                        param(ui, "RISE_SHIFT", &mut p.rise_shift, 0..=16);
                        param(ui, "RISE_MAX_STEP_Q", &mut p.rise_max_step_q, 0..=4096);
                        param(ui, "FALL_SHIFT", &mut p.fall_shift, 0..=16);
                        param(
                            ui,
                            "NEG_RECAL_THRESHOLD",
                            &mut p.neg_recal_threshold,
                            0..=200,
                        );
                        param(ui, "NEG_RECAL_MS", &mut p.neg_recal_ms, 0..=5000);
                        param(ui, "STUCK_TIME_MS", &mut p.stuck_time_ms, 0..=300_000);
                        param(ui, "STUCK_VARIATION", &mut p.stuck_variation, 0..=200);
                        param(ui, "CALIB_SHIFT", &mut p.calib_shift, 0..=16);
                        param(ui, "CALIB_SETTLE_MS", &mut p.calib_settle_ms, 0..=10_000);
                        param(
                            ui,
                            "NOISE_FLOOR_DEFAULT",
                            &mut p.noise_floor_default,
                            0..=200,
                        );
                        param(ui, "NOISE_FLOOR_MAX", &mut p.noise_floor_max, 0..=500);
                        param(ui, "HI_LO_JUMP", &mut p.hi_lo_jump, 0..=1000);
                    });
                    if ui.button("既定値に戻す").clicked() {
                        p = Params::DEFAULT;
                    }
                    if p != self.algo.params {
                        // 変えたら、読み込んでいるデータの先頭から通し直す
                        self.algo.params = p;
                        self.algo.invalidate();
                    }
                });
            });
    }

    // ------------------------------------------------------------------
    //  右: 統計（表示している時間の範囲）
    // ------------------------------------------------------------------
    fn stats_panel(&mut self, ui: &mut egui::Ui) {
        self.algo_panel(ui);
        ui.separator();
        ui.heading("統計");
        let Some((t0, t1)) = self.window_us() else {
            return;
        };
        let stats = self
            .store
            .stats(t0, t1, self.show_corrected && !self.show_raw);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.label(format!(
                "範囲 {:.2} 秒  {} フレーム  欠け {}",
                (t1 - t0) as f64 / 1e6,
                stats.frames,
                stats.lost_frames
            ));
            ui.label("ノイズを見るときは、触れていない区間を表示してください");
            egui::Grid::new("key_stats").striped(true).show(ui, |ui| {
                for h in ["キー", "平均", "σ", "p-p", "ずれ"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for (k, ks) in stats.keys.iter().enumerate() {
                    ui.label(RichText::new(format!("{k}")).color(key_color(k)));
                    ui.label(format!("{:.1}", ks.mean));
                    ui.label(format!("{:.2}", ks.std));
                    ui.label(format!("{}", ks.peak_to_peak));
                    ui.label(format!("{}", ks.hi_lo_jumps));
                    ui.end_row();
                }
            });
            ui.separator();

            let iv = stats.interval_us;
            ui.label(RichText::new("サンプル間隔").strong());
            ui.label(format!(
                "最小 {:.2} / 平均 {:.3} / 最大 {:.2} ms",
                iv.min as f64 / 1000.0,
                iv.mean / 1000.0,
                iv.max as f64 / 1000.0
            ));
            histogram_plot(
                ui,
                "interval_hist",
                &stats.interval_hist,
                store::INTERVAL_BIN_US,
            );
            ui.separator();

            ui.horizontal(|ui| {
                ui.label(RichText::new("値の更新間隔").strong());
                egui::ComboBox::from_id_salt("stats_key")
                    .width(60.0)
                    .selected_text(format!("key{}", self.stats_key))
                    .show_ui(ui, |ui| {
                        for k in 0..self.store.nkeys {
                            ui.selectable_value(&mut self.stats_key, k, format!("key{k}"));
                        }
                    });
            });
            if self.stats_key < self.store.nkeys {
                let changes = self.store.change_intervals(t0, t1, self.stats_key);
                let summary = store::Summary::of(&changes);
                ui.label("生値が変わるまでの時間（チップが値を更新する周期を見る）");
                ui.label(format!(
                    "{} 回  最小 {:.2} / 平均 {:.2} / 最大 {:.2} ms",
                    summary.count,
                    summary.min as f64 / 1000.0,
                    summary.mean / 1000.0,
                    summary.max as f64 / 1000.0
                ));
                const CHANGE_BIN_US: u64 = 1000;
                histogram_plot(
                    ui,
                    "change_hist",
                    &store::histogram(&changes, CHANGE_BIN_US),
                    CHANGE_BIN_US,
                );
            }
            ui.separator();

            ui.label(RichText::new("イベント（表示範囲、新しい順）").strong());
            let events: Vec<_> = self
                .store
                .events
                .iter()
                .rev()
                .filter(|(t, _)| (t0..=t1).contains(t))
                .take(30)
                .collect();
            for (t, event) in events {
                ui.label(format!("{:.3}s  {}", *t as f64 / 1e6, event.describe()));
            }
            ui.separator();
            ui.label(RichText::new("ファームからのメッセージ").strong());
            for text in self.store.texts.iter().rev().take(10) {
                ui.label(text);
            }
        });
    }

    // ------------------------------------------------------------------
    //  下: 全キーの今の値
    // ------------------------------------------------------------------
    fn bars(&mut self, ui: &mut egui::Ui) {
        let store = &self.store;
        let Some(last) = store.time_us.len().checked_sub(1) else {
            return;
        };
        let range = self
            .window_us()
            .map(|(t0, t1)| store.range(t0, t1))
            .unwrap_or(last..last + 1);
        if self.algo_enabled && self.bars_output && self.algo.output.len() == store.nkeys {
            // output（アルゴリズムの結果）の、表示範囲の最後の値
            let bars: Vec<Bar> = match self.window_us() {
                Some((t0, t1)) => {
                    let r = self.algo.range(t0, t1);
                    (0..store.nkeys)
                        .map(|k| {
                            let v = r.end.checked_sub(1).map_or(0, |i| self.algo.output[k][i]);
                            Bar::new(k as f64, v as f64)
                                .fill(key_color(k))
                                .name(format!("key{k}"))
                        })
                        .collect()
                }
                None => Vec::new(),
            };
            bar_plot(ui, "output", bars);
            return;
        }
        let series = if self.show_corrected && !self.show_raw {
            &store.corrected
        } else {
            &store.raw
        };
        let bars: Vec<Bar> = (0..store.nkeys)
            .map(|k| {
                // 表示範囲の最後の値（一時停止中は範囲の終わり）
                let i = range.end.saturating_sub(1).min(last);
                let mut v = series[k][i] as f64;
                if self.bars_minus_min {
                    let min = range.clone().map(|i| series[k][i]).min().unwrap_or(0);
                    v -= min as f64;
                }
                Bar::new(k as f64, v)
                    .fill(key_color(k))
                    .name(format!("key{k}"))
            })
            .collect();
        bar_plot(ui, "今の値", bars);
    }
}

impl eframe::App for MonitorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        if let Some(player) = &mut self.player {
            player.update(&mut self.store);
        }
        if self.algo_enabled {
            self.algo.update(&self.store);
        }

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::left("view")
            .default_size(190.0)
            .show(ui, |ui| self.view_panel(ui));
        egui::Panel::right("stats")
            .default_size(330.0)
            .show(ui, |ui| self.stats_panel(ui));
        egui::Panel::bottom("bars").show(ui, |ui| self.bars(ui));
        egui::CentralPanel::default().show(ui, |ui| self.plot(ui));

        let busy = self.link.is_some() || self.player.as_ref().is_some_and(|p| p.playing);
        if busy {
            ui.ctx().request_repaint_after(REPAINT_INTERVAL);
        }
    }
}

/// 表示する点を作る。valid でないサンプルは除く。多すぎるときは区間毎の最小・最大に間引く
fn decimate(
    times: &VecDeque<u64>,
    value: impl Fn(usize) -> f64,
    valid: impl Fn(usize) -> bool,
    range: std::ops::Range<usize>,
) -> Vec<[f64; 2]> {
    let point = |i: usize| [times[i] as f64 / 1e6, value(i)];
    if range.len() <= MAX_PLOT_POINTS * 2 {
        return range.filter(|&i| valid(i)).map(point).collect();
    }
    let bucket = range.len().div_ceil(MAX_PLOT_POINTS);
    let mut out = Vec::with_capacity(MAX_PLOT_POINTS * 2);
    let mut start = range.start;
    while start < range.end {
        let end = (start + bucket).min(range.end);
        let mut min: Option<(usize, f64)> = None;
        let mut max: Option<(usize, f64)> = None;
        for i in (start..end).filter(|&i| valid(i)) {
            let v = value(i);
            if min.is_none_or(|(_, m)| v < m) {
                min = Some((i, v));
            }
            if max.is_none_or(|(_, m)| v > m) {
                max = Some((i, v));
            }
        }
        if let (Some((a, _)), Some((b, _))) = (min, max) {
            // 時刻の順に並べる
            let (a, b) = if a <= b { (a, b) } else { (b, a) };
            out.push(point(a));
            if b != a {
                out.push(point(b));
            }
        }
        start = end;
    }
    out
}

/// パラメータの欄の 1 行
fn param<N: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    name: &str,
    value: &mut N,
    range: std::ops::RangeInclusive<N>,
) {
    ui.label(name);
    ui.add(egui::DragValue::new(value).range(range));
    ui.end_row();
}

fn bar_plot(ui: &mut egui::Ui, name: &str, bars: Vec<Bar>) {
    Plot::new("bars")
        .height(140.0)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .include_y(0.0)
        .x_axis_label("キー")
        .show(ui, |plot_ui| {
            plot_ui.bar_chart(BarChart::new(name, bars).width(0.8))
        });
}

fn histogram_plot(ui: &mut egui::Ui, id: &str, hist: &[(u64, u32)], bin_us: u64) {
    let bin_ms = bin_us as f64 / 1000.0;
    let bars: Vec<Bar> = hist
        .iter()
        .map(|&(lo, n)| Bar::new(lo as f64 / 1000.0 + bin_ms / 2.0, n as f64).width(bin_ms * 0.9))
        .collect();
    Plot::new(id)
        .height(110.0)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .include_y(0.0)
        .x_axis_label("ms")
        .show(ui, |plot_ui| plot_ui.bar_chart(BarChart::new("回数", bars)));
}

/// 受信したフレームの数 / 秒
#[derive(Default)]
struct RateMeter {
    times: VecDeque<Instant>,
}

impl RateMeter {
    fn count(&mut self) {
        let now = Instant::now();
        self.times.push_back(now);
        while self
            .times
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(1))
        {
            self.times.pop_front();
        }
    }

    fn per_second(&self) -> f64 {
        let now = Instant::now();
        self.times
            .iter()
            .filter(|t| now.duration_since(**t) <= Duration::from_secs(1))
            .count() as f64
    }
}
