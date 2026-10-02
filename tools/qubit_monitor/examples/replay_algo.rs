//! 記録（.qlog）を touch_algo に通し、キー毎の結果をまとめて表示する（GUI を使わない）
//!
//! 実行: `cd tools/qubit_monitor && cargo run --example replay_algo -- <記録.qlog> [間引き µs（既定 8000、0 で間引かない）]`
//! PC アプリと同じ AlgoRunner を使う。パラメータは既定値（doc/touch_baseline.md §3.11）
use std::path::PathBuf;

use qubit_monitor::algo::AlgoRunner;
use qubit_monitor::playback::Player;
use qubit_monitor::store::Store;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!("使い方: replay_algo <記録.qlog> [間引き µs]");
        std::process::exit(1);
    };
    let decimation_us = args.next().and_then(|s| s.parse().ok()).unwrap_or(8000);

    let mut player = Player::open(&path).expect("記録を開けません");
    let mut store = Store::new();
    while !player.is_finished() {
        player.step(&mut store);
    }
    let mut runner = AlgoRunner::new();
    runner.decimation_us = decimation_us;
    runner.update(&store);

    let n = runner.time_us.len();
    println!(
        "{}: フレーム {}（{:.1} 秒） → アルゴリズムに通したフレーム {}（間引き {} µs）  状態 {:?}",
        path.display(),
        store.time_us.len(),
        player.duration_us() as f64 / 1e6,
        n,
        decimation_us,
        runner.state()
    );
    if n == 0 {
        return;
    }
    // 初期取得の後（最初の 0.5 秒を除く）の範囲で集計する
    let t_start = runner.time_us[0] + 500_000;
    let from = runner.time_us.partition_point(|&t| t < t_start);
    println!(
        "キー  baseline(最初→最後)  filtered の p-p  delta 最小/最大  output 最大  onset 回数"
    );
    for k in 0..runner.baseline.len() {
        let range = from..n;
        let filtered: Vec<u16> = range.clone().map(|i| runner.filtered[k][i]).collect();
        let delta: Vec<i32> = range.clone().map(|i| runner.delta[k][i]).collect();
        let output_max = range
            .clone()
            .map(|i| runner.output[k][i])
            .max()
            .unwrap_or(0);
        let pp = filtered.iter().max().unwrap_or(&0) - filtered.iter().min().unwrap_or(&0);
        let onsets = runner.onsets.iter().filter(|(_, key)| *key == k).count();
        println!(
            "key{k}  {:>5} → {:>5}        {:>3}             {:>4} / {:>4}        {:>4}      {}",
            runner.baseline[k].get(from).copied().unwrap_or(0),
            runner.baseline[k][n - 1],
            pp,
            delta.iter().min().unwrap_or(&0),
            delta.iter().max().unwrap_or(&0),
            output_max,
            onsets
        );
    }
}
