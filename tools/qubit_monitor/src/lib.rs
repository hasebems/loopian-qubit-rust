//! Loopian::QUBIT のタッチ信号を受信・表示・記録する PC アプリ（doc/debug_env.md §4）
//!
//! 画面は main.rs / app.rs、それ以外（プロトコル・記録・再生・統計）はこのライブラリに置く。
//! examples/probe.rs からも使う
pub mod csv_export;
pub mod playback;
pub mod protocol;
pub mod qlog;
pub mod serial;
pub mod store;
