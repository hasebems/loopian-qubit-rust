//! Loopian::QUBIT のタッチの計算。I2C や時計には触れない純粋な計算だけで、
//! ファーム（no_std）と PC アプリ（tools/qubit_etude）で共有する。
//!
//! - `electrode`: 電極 1 つずつの信号処理（ノイズ除去・基準値。doc/touch_baseline.md）
//! - `touch`: 複数の電極の output から指 1 本のタッチ（depth・position）を求めて追跡する（doc/touch_analysis.md §3）
//! - `playinfo`: タッチの depth・position から、Note On/Off・ビブラートなどの演奏情報を作る（doc/touch_analysis.md §4）
//!
//! 「タッチ」は指 1 本の単位だけに使い、電極 1 つの単位のものは electrode と呼ぶ（doc/touch_analysis.md §7）
#![no_std]

pub mod electrode;
pub mod playinfo;
pub mod touch;
