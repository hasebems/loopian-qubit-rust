//! This build script copies the `memory.x` file from the crate root into
//! a directory where the linker can always find it at build time.
//! For many projects this is optional, as the linker always searches the
//! project root directory -- wherever `Cargo.toml` is. However, if you
//! are using a workspace or have a more complicated build setup, this
//! build script becomes required. Additionally, by requesting that
//! Cargo re-run the build script whenever `memory.x` is changed,
//! updating `memory.x` ensures a rebuild of the application with the
//! new memory settings.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    // Emit build date and time as environment variables
    let now = chrono::Local::now();
    println!("cargo:rustc-env=BUILD_DATE={}", now.format("%y-%m-%d"));
    println!(
        "cargo:rustc-env=BUILD_VERSION=v{}",
        env!("CARGO_PKG_VERSION")
    );

    // Put the linker script somewhere the linker can find it
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    println!("cargo:rustc-link-search={}", out.display());

    // The file `memory.x` is loaded by cortex-m-rt's `link.x` script, which
    // is what we pass to the linker below
    let memory_x = include_bytes!("memory.x");
    let mut f = File::create(out.join("memory.x")).unwrap();
    f.write_all(memory_x).unwrap();

    // リンカの引数。以前は .cargo/config.toml の rustflags で渡していたが、新しい cargo（nightly）で
    // rustflags がリンカに届かず、link.x のシンボルが未定義になって CI が失敗した。
    // build.rs からなら cargo の版や成果物の置き方に関係なく渡せる（doc/build.md）
    // - --nmagic: セクションのページ境界への整列をやめる（フラッシュの節約）
    // - -Tlink.x: cortex-m-rt のリンカスクリプト（memory.x を読み込む）
    // - -Map=output.map: リンクの結果のマップを、リポジトリ直下に書き出す（git 管理外）
    for arg in ["--nmagic", "-Tlink.x", "-Map=output.map"] {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
}
