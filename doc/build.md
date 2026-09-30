## フィーチャーフラグ

| フィーチャー | 効果 |
|---|---|
| なし（デフォルト） | PCA9544: 4ch×4台（96 キー）、ADC: 3ch。本番用 |
| `test_mode` | PCA9544: 1ch×1台（6 キー） |
| `no_pca9544` | PCA9544 無し（AT42QT1070 1個を I2C に直結、6 キー）。`test_mode` を含む |
| `debug_stream` | デバッグ環境。USB を MIDI + CDC（仮想シリアル）にし、タッチの生値を PC に送る。スキャン周期の既定が 2ms になる（`doc/debug_env.md`） |

- 組み合わせるときはカンマで区切る: `--features no_pca9544,debug_stream`
- `debug_stream` は `no_pca9544` を含まない。6 キー直結で使うときは両方を指定する
- 以前あった `adc_ch4`（ADC 4ch）は、今の `Cargo.toml` には無い

## ファームのビルド・書き込み

```sh
cargo build                                         # dev ビルド
cargo run --release                                 # 書き込み・実行（BOOTSEL で USB 接続）
cargo run --release --features no_pca9544,debug_stream   # デバッグ環境の実験用（6 キー直結）
```

git push の前に（CI と同じ条件）:

```sh
cargo clippy --all-features -- --deny=warnings
cargo fmt --check
```

## qubit_monitor（PC アプリ）

タッチの生値を受信・表示・記録する。ファームは `debug_stream` 付きで書き込んでおく。

```sh
cd tools/qubit_monitor
cargo run --release              # GUI を起動
cargo run --example probe        # GUI 無しで、ファームとの通信を一通り確かめる
cargo test                       # PC アプリのテスト
```

- **必ず `tools/qubit_monitor` に移動してから実行する**。リポジトリ直下から `--manifest-path` で指定すると、直下の `.cargo/config.toml` の組み込み向けターゲットが効いて失敗する
- 使い方: ポートを選んで［接続］→［▶ 開始］でグラフが動く。［● 記録］で `.qlog` に保存、［記録を開く］で再生、［CSV 書き出し］で `.qlog` を CSV にする
- 周期（2 / 5 / 10 / 20ms）はツールバーから変えられる。［マーカー］で波形に目印を付けられる
- Mac では QUBIT は `/dev/cu.usbmodem*` として見える。認識がおかしいときは、Audio MIDI 設定から QUBIT を削除して挿し直す
