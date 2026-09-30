//! 記録ファイル（.qlog）を CSV に書き出す（doc/debug_env.md §4.3）
//!
//! 列: `time_us,seq,valid,key0..keyN-1,event`
//! - フレームの行: seq・valid（読み取り成功フラグの 16 進）・キー毎の生値。event は空
//! - イベントの行（Note・マーカー・INFO・TEXT）: event に説明を入れ、seq・valid・生値は空
//! - 時刻（起動からの µs、一周を伸ばした値）の順に並べる。INFO・TEXT は時刻を持たないので、
//!   直前の行の時刻にする（最初の時刻より前に届いたものは、最初の時刻にする）
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use crate::protocol::{Packet, Parser, TimeExtender};
use crate::qlog;

/// 書き出した行数を返す
pub fn export(qlog_path: &Path, csv_path: &Path) -> io::Result<usize> {
    let (header, bytes) = qlog::read(qlog_path)?;
    let mut packets = Vec::new();
    Parser::new().push(&bytes, &mut packets);

    // (時刻, 元の順番, 行の中身)。時刻を持たない行は None
    let mut rows: Vec<(Option<u64>, usize, Row)> = Vec::new();
    let mut time = TimeExtender::default();
    let mut nkeys = header.nkeys as usize;
    for (order, packet) in packets.into_iter().enumerate() {
        let (t, row) = match packet {
            Packet::Frame(f) => {
                nkeys = nkeys.max(f.raw.len());
                (
                    Some(time.extend(f.time_us)),
                    Row::Frame(f.seq, f.valid, f.raw),
                )
            }
            Packet::Event(e) => (Some(time.extend(e.time_us)), Row::Event(e.describe())),
            Packet::Info(i) => (
                None,
                Row::Event(format!(
                    "info version={} build={} nkeys={} period_us={} dropped={}",
                    i.version, i.build_date, i.nkeys, i.scan_period_us, i.dropped
                )),
            ),
            Packet::Text(s) => (None, Row::Event(format!("text {s}"))),
            Packet::Unknown(_) => continue,
        };
        rows.push((t, order, row));
    }
    let first_t = rows.iter().find_map(|(t, _, _)| *t).unwrap_or(0);
    let mut last_t = first_t;
    let mut rows: Vec<(u64, usize, Row)> = rows
        .into_iter()
        .map(|(t, order, row)| {
            last_t = t.unwrap_or(last_t);
            (last_t, order, row)
        })
        .collect();
    rows.sort_by_key(|(t, order, _)| (*t, *order));

    let mut out = BufWriter::new(File::create(csv_path)?);
    write!(out, "time_us,seq,valid")?;
    for k in 0..nkeys {
        write!(out, ",key{k}")?;
    }
    writeln!(out, ",event")?;
    for (t, _, row) in &rows {
        match row {
            Row::Frame(seq, valid, raw) => {
                write!(out, "{t},{seq},{valid:x}")?;
                for k in 0..nkeys {
                    match raw.get(k) {
                        Some(v) => write!(out, ",{v}")?,
                        None => write!(out, ",")?,
                    }
                }
                writeln!(out, ",")?;
            }
            Row::Event(text) => {
                write!(out, "{t},,")?;
                for _ in 0..nkeys {
                    write!(out, ",")?;
                }
                // CSV の区切りと衝突しないよう、カンマと改行を置き換える
                writeln!(out, ",{}", text.replace([',', '\n', '\r'], " "))?;
            }
        }
    }
    out.flush()?;
    Ok(rows.len())
}

enum Row {
    Frame(u32, u128, Vec<u16>),
    Event(String),
}
