# タッチ信号のデバッグ環境 設計書

## 1. 目的と前提

### 1.1 目的

`doc/touch_baseline.md` の方針（基準値・ノイズ除去・立ち上がり開始時刻）は、一度には決めない。実際に触りながら、変位の時系列を見て少しずつ決めていく。そのために次のことができる環境を作る。

1. タッチセンサの生値を、欠けなく時刻付きで PC に送る
2. PC 上でリアルタイムに波形を見る
3. 記録したデータを保存し、あとから同じデータで何度でも再生・比較する
4. 基準値やフィルタのアルゴリズムを PC 上で試し、パラメータを動かしながら結果を見る
5. 決まったアルゴリズムをファームに移し、PC 側と同じ結果になることを確かめる

### 1.2 前提

- 実験は **PCA9544 を使わず、AT42QT1070 1 個（6 キー）を I2C に直結** した構成で行う（既存の `no_pca9544` feature）
- 表示は PC 側で行い、実験中は PC を常に接続する。OLED は `doc/task_architecture.md` で Core0 の I2C0 に移るので、使ってもタッチのスキャンには影響しない
- 作業は `main` ではなく `debug_env` ブランチ（`task_architecture` から分岐）で進める
- **`doc/task_architecture.md` のコア・タスク構成の改修が済んでいることを前提とする**。Core1 の `touch_task` が一定周期でスキャン → 解析を行い、ノートイベントを `MIDI_TX` で Core0 に送っている状態から始める
- 後で 96 キー（16 倍）に戻しても、プロトコル・ファーム・PC アプリのどれにも作り直しが要らないようにする（§7）。ただし **当面は 96 キーの構成では使わない**（2026-09-30 決定）
- **チップの基準値（Reference Data）は読まない**（`doc/touch_baseline.md` §2 の方針 1）。PC に送るのは Key Signal の生値だけで、基準値の欄はプロトコルに持たせない
    - ただし、今の `read_touch` にはチップの基準値を 12 スキャンに 1 回読み直す処理（`set_reference()`）が残っている。これを削除するのは段階 3・4（`touch_algo` への置き換え）のときで、段階 1・2 では手を入れない
    - そのため段階 1・2 では、12 スキャンに 1 回（2ms 周期なら 24ms 毎）スキャンの I2C の読み取りが 1 回増え、そのスキャンだけ間隔がずれる。FRAME の `time_us` で PC 側から見分けられる

## 2. 全体構成

```
┌──────────── XIAO RP2350 ────────────┐          ┌──────────── PC ────────────┐
│ Core1: touch_task                   │          │ qubit_monitor（PC アプリ）   │
│   AT42QT1070 を周期的に読む         │          │   ・シリアル受信／記録       │
│   → DebugFrame を DEBUG_FRAMES へ   │          │   ・リアルタイム波形表示     │
│                                     │   USB    │   ・記録ファイルの再生       │
│ Core0: debug_stream_task            │ ──CDC──► │   ・PC 側アルゴリズムの試行  │
│   DEBUG_FRAMES / DEBUG_EVENTS を    │ ◄─コマンド│     （パラメータを GUI で操作）│
│   バイナリにして CDC へ送る         │          │                              │
│ Core0: usb_task（MIDI + CDC 複合）  │ ──MIDI─► │ DAW / Loopian::App（従来通り）│
└─────────────────────────────────────┘          └──────────────────────────────┘
```

- USB を **MIDI と CDC-ACM（仮想シリアル）の複合デバイス** にする。MIDI の演奏データはこれまでどおり流しつつ、デバッグ用のデータは別の経路で送る
- 全体は `debug_stream` feature で有効にする。本番用のファームの USB 構成は今のまま

### 2.1 USB CDC を選んだ理由

| 方式 | 採否 | 理由 |
|---|---|---|
| **USB CDC-ACM（MIDI と複合）** | 採用 | 追加のハードウェアが不要。帯域に余裕がある（§5.4）。PC からは普通のシリアルポートに見える |
| USB MIDI の SysEx | 不採用 | 7bit に詰め直す必要がある。OS や DAW の MIDI スタックを通るので、取りこぼしや他のアプリへの干渉が心配 |
| UART | 保留 | USB-シリアル変換器と配線が必要。USB が不調なときの予備として、裏面の D11/D12（UART1）を空けておく（`doc/hw_modify.md`） |
| defmt + RTT | 不採用 | デバッグプローブと SWD の配線が必要で、今の picotool での書き込みとは流れが変わる |

## 3. ファームウェア側

### 3.1 feature

```toml
[features]
debug_stream = []   # USB CDC でタッチの生値を PC に送る
```

- 実験時は `cargo run --release --features no_pca9544,debug_stream`
- `debug_stream` は `no_pca9544` とは独立させる。後で 96 キー構成でも使えるようにするため

### 3.2 USB の複合化（`main.rs`）

- `Config` で IAD を使う複合デバイスにする
    - `config.composite_with_iads = true`
    - `config.device_class = 0xEF`、`device_sub_class = 0x02`、`device_protocol = 0x01`
- `MidiClass::new(...)` に加えて `CdcAcmClass::new(&mut builder, state, 64)` を作る。`State` は `make_static!` で確保する
- ディスクリプタが大きくなるので、`config_descriptor` などのバッファを 256 → 512 バイトに増やす
- Mac では `/dev/cu.usbmodem*` として見える
- 注意: 同じ VID/PID のままディスクリプタが変わる。OS がデバイス情報をキャッシュしていて認識がおかしいときは、MIDI スタジオ（Audio MIDI 設定）から削除して挿し直す

### 3.3 スキャン（Core1、`read_touch.rs` と `touch_task`）

- スキャンループは `touch_task` の `Ticker` で周期実行している（`task_architecture.md` §4.1）。改修の時点の周期は `constants.rs` の `SCAN_PERIOD_MS`（const、10ms）だったが、デバッグ環境では PC からのコマンドで変えられるようにした（段階 2）
    - 周期の既定値は `constants.rs` の `SCAN_PERIOD_US`（µs の const）。`debug_stream` のときは 2000、無いときは 10000
    - 実行中の周期は `shared::scan_period_us()` で読み、`shared::set_scan_period_us()` で変える（値は `shared.rs` の非公開の Atomic `SCAN_PERIOD_SETTING_US` に持つ）。`debug_stream` が無いときは `scan_period_us()` は const を返すだけ
    - 周期を変えたときは `Ticker` を作り直し、解析の間引き（`analysis_divider(周期)`）も計算し直す
    - **選べる周期は 2, 5, 10, 20ms の 4 つ**（2026-09-30 決定）。それ以外の値を指定されたら、周期は変えずにエラーを返す
    - 20ms は 10ms の約数ではないので、解析の周期を 10ms にできない。**20ms のときは毎スキャン（20ms 毎）解析する**（2026-09-30 決定）。`analysis_divider()` は `max(10ms / 周期, 1)` を返す
        - `QubitTouch` の時間の定数は 10ms 毎の呼び出しを前提にしているので、20ms のときは時間の計算（離したと判断するまでの時間、ビブラート、ベロシティ）が 2 倍にずれる。承知の上で使う（ノートは出るが、振る舞いは通常と同じではない）
    - 既定は **2ms**。チップの更新（8ms 周期）より速く読むことで、チップが実際にいつ値を更新しているか、読み取り中に hi/lo がずれる頻度はどのくらいか、を観察できる。しばらくは 2ms で使い、最終的な周期は後で決める
    - 最終構成（96 キー）では 1 周に 8ms 前後かかるため、2ms では読めない。最終的なアルゴリズムは 8ms 周期のデータで評価する（PC 側で間引いて再現できる。§4.4）。当面は 96 キーでは使わないので（§1.2）、`test_mode` かどうかで既定を変えることはしない
- 読み取った値は **補正前の生値** のまま送る。hi/lo ずれ補正（`raw -= 256`）も含め、補正はすべて PC 側で試す
    - `read_touch` の読み取りの直後、hi/lo ずれ補正をかける前の値を別に取っておき、それを `DebugFrame` に入れる。解析に渡す経路（補正あり）は変えない
    - 読み取りに失敗したキーは `valid` のビットを 0 にし、`raw` には前回の生値をそのまま入れる
    - `seq` は捨てたフレームも含めて数える。PC 側は `seq` の欠けで取りこぼしを知る
- 1 回のスキャン毎に `DebugFrame` を作り、`DEBUG_FRAMES` に `try_send` する。キューがいっぱいなら捨て、捨てた数を `DEBUG_DROPPED`（Atomic）に数える
- 通常の処理（`QubitTouch` での解析、ノートイベントの送信）は今までどおり行う。演奏しながら記録できるようにするため
- スキャン周期を 2ms・5ms にしても、解析は 10ms 毎（`analysis_divider()` フレームに 1 回）に行うので、`QubitTouch` の振る舞いは変わらない（`task_architecture.md` §4.1）。20ms のときだけは変わる（上記）
    - `QubitTouch` に渡すのは、解析する回のスキャンで読んだ値だけ。ほかの回（2ms なら 5 回のうち 4 回）の値は PC に送るだけで、解析には使わない（平均などはとらない）

```rust
pub struct DebugFrame {
    pub seq: u32,                            // スキャン番号
    pub time_us: u32,                        // スキャン開始時刻（Core1 起動からの µs。約 71 分で一周）
    pub valid: u128,                         // 読み取り成功フラグ（1 bit / キー。96 キーまで）
    pub raw: [u16; constants::TOTAL_QT_KEYS],
}
```

`DEBUG_FRAMES: Channel<CriticalSectionRawMutex, DebugFrame, 16>`。6 キーなら 1 フレーム 30 バイト程度、96 キーでも 210 バイト程度。

### 3.4 イベント

波形のどの時点で何が起きたかを照らし合わせられるよう、次のイベントも同じストリームに入れる。

| イベント | 発生元 | 内容 |
|---|---|---|
| Note On / Off | `touch_task`（Core1）の MIDI コールバック | ノート番号、ベロシティ、位置 |
| マーカー | PC からのコマンド | 実験者が付ける目印（「ここからゆっくり触る」など） |

- **イベントは Note On / Off とマーカーだけにする**（2026-09-30 決定）。スイッチ、動作モード・設定画面、エラーなどは今は入れず、必要になったときに加える

`DEBUG_EVENTS: Channel<CriticalSectionRawMutex, DebugEvent, 16>` に `try_send` する。時刻は各タスクで `Instant::now()` から取る（両コアで同じタイマーを使っているので、フレームの時刻と比べられる）。

- EVENT は PC がポートを開いている間だけキューに入れて送る。`stop` の後も送り続ける
- キューがあふれた分は、FRAME と同じ `DEBUG_DROPPED` に数える
- マーカーは `debug_stream_task` がコマンドを受けたときに、キューを通さずにその場で送る

### 3.5 送信タスク（Core0、`debug_stream_task`）

```
loop {
    cdc.wait_connection().await;        // PC がポートを開くまで待つ（DTR）
    while 接続中 {
        select(DEBUG_FRAMES の受信, DEBUG_EVENTS の受信, CDC からのコマンド受信)
        → パケットに組み立てて、64 バイトずつ write_packet（with_timeout で包む）
    }
}
```

- PC がポートを開いていない間は送らない。キューがあふれた分は捨てるだけなので、演奏には影響しない
- **注意（embassy-usb の API を確認した結果）**: `CdcAcmClass::wait_connection()` は、ホストがデバイスを構成して端点を有効にするまで待つだけで、DTR（PC がポートを開いたこと）は待たない。PC がポートを開いたかどうかは `split_with_control()` で得られる `ControlChanged` の `control_changed()`（変化を待つ）と `dtr()` で見る。DTR が落ちたら（ポートを閉じたら）切断とみなす
- `write_packet` がタイムアウトしたら切断されたとみなし、`wait_connection` に戻る
- パケットの長さがちょうど 64 の倍数になったときは、長さ 0 のパケットを送る（USB バルク転送で、受信側に区切りを知らせるため）
- `start` コマンドを受けるまではフレームを送らない（§5.2）。切断したら、次に接続したときは `start` を受ける前の状態（送らない）に戻る
- `DEBUG_FRAMES` があふれたときは `DEBUG_DROPPED` を数えるだけで、エラーコードは記録しない
- spawn に失敗したら、既存の約束事どおりエラーコードに記録する（25 を割り当てる。`task_architecture.md` §4.7 で予約済み）

### 3.6 OLED

- コア・タスク構成の改修後は、OLED は Core0 の I2C0 にあり、タッチのスキャンとはバスもコアも別になる。そのため `debug_stream` のときに OLED を止める必要は無い
- スイッチによる設定画面の出入り（`SETTING_MODE`）とモード切替は、実験中もそのまま使う

## 4. PC アプリ（qubit_monitor）

### 4.1 技術選定

**Rust + egui（eframe + egui_plot）+ serialport クレート** を推奨する。

最大の理由は、**アルゴリズムのコードをファームと PC で共有できる** こと。

- `touch_baseline.md` §4.2 の `TouchSignal` は、I2C に触れない純粋な計算として設計している
- これを `no_std` の小さなクレート（`touch_algo`）に切り出し、ファームと PC アプリの両方から使う
- PC で調整したアルゴリズムが、そのままファームで動く。移植の手間も、移植時のずれも無い
- 固定小数点・飽和演算・整数の桁あふれなど、ファーム特有の振る舞いも PC 上で同じように再現できる

egui はフレーム毎に全体を描き直す方式（immediate mode）で、数千〜数万点の折れ線ならリアルタイムで十分に描ける。

| 候補 | 評価 |
|---|---|
| **Rust + egui** | 推奨。アルゴリズムを共有できる。単一の実行ファイルになる |
| Python + pyqtgraph | リアルタイム描画は速く、試行錯誤もしやすい。ただしアルゴリズムを Python と Rust で二重に書くことになる。記録ファイルを numpy で解析する補助ツールとしては併用してよい |
| ブラウザ + Web Serial API | インストール不要。ただし Chrome 系に限られ、アルゴリズムの共有には WASM 化が必要 |

### 4.2 画面構成

```
┌────────────────────────────────────────────────────────────────────┐
│ [ポート ▼] [接続] [● 記録] [▶ 開始/■ 停止] [マーカー] 周期:[2ms ▼]   │
│ 受信: 498 fr/s  取りこぼし: 0  ずれ補正: 3  キュー溢れ(ファーム): 0 │
├──────────────────────────────────────────────┬─────────────────────┤
│ 時系列グラフ（上段）: 選んだキーの raw / filtered / baseline        │ パラメータ          │
│   ─ key0 ─ key1 ─ key2 ...   │ Note On/Off を縦線で表示              │  SMOOTH_SAMPLES [2] │
│                                                                      │  RISE_SHIFT     [8] │
│ 時系列グラフ（下段）: delta / output、しきい値の横線                │  QUIET_THRESHOLD[8] │
│                                                                      │  ...                │
├──────────────────────────────────────────────┤  [既定値に戻す]      │
│ キーのバー表示: 全キーの現在の output（96 キーでは円形またはヒートマップ）│  [ファームへ送る]※  │
├──────────────────────────────────────────────┴─────────────────────┤
│ 統計: キー毎のノイズ（標準偏差・p-p）、サンプル間隔のヒストグラム      │
└────────────────────────────────────────────────────────────────────┘
```

※ 「ファームへ送る」は、アルゴリズムをファームに移した後の段階で使う（§6 の段階 4）

- **時系列グラフ**
    - 表示する時間幅（1〜30 秒）を変えられる
    - 一時停止・スクロール・拡大ができる。一時停止しても受信と記録は続ける
    - 表示するキーと、表示する系列（raw / 補正後 / filtered / baseline / delta / output / onset）を選べる
    - イベント（Note On/Off、スイッチ、マーカー）を縦線で重ねて表示する
- **キーのバー表示**: 全キーの今の値を一目で見る。96 キーのときは円形に並べた表示にする
- **統計**: キー毎のノイズ（無操作区間の標準偏差・p-p）、サンプル間隔のばらつき、スキャン番号の欠け（取りこぼし）、hi/lo ずれの回数
- **パラメータ**: `touch_algo` の調整値を GUI で動かすと、表示中のデータにすぐ反映される

### 4.3 記録と再生

- **記録**: 受信したバイト列を、そのままファイル（`.qlog`）に保存する。ファイルの先頭にヘッダ（記録日時、ファームのバージョン、キー数、スキャン周期）を付ける
    - ヘッダは固定長 64 バイト（2026-09-30、PC アプリの実装時に決定）。`"QLOG"`、形式の版（u16、1）、ヘッダ長（u16、64）、記録開始の日時（i64、Unix 時刻の ms）、ファームのバージョン（8 バイト）、ビルド日（16 バイト）、キー数（u8）、予約 3 バイト、スキャン周期（u32、µs）、予約 16 バイト。すべてリトルエンディアン。読むときはヘッダ長だけ読み飛ばすので、後でヘッダを延ばせる。詳細は `tools/qubit_monitor/src/qlog.rs` の先頭のコメント
    - ヘッダの値は、記録を始めたときに受け取っている INFO から取る。記録を始めた直後に `info` を送り、記録の中にも INFO が入るようにする（周期を変えたときの INFO も記録に入る）
    - 記録ファイルの置き場所は決めず、記録を始めるときに保存先を選ぶ（§8）
    - 受信したバイト列をそのまま保存するので、後でプロトコルの解釈を直しても読み直せる
- **再生**: `.qlog` を開くと、実機からの受信と同じ経路でデータを流す。速さは等倍・早送り・コマ送りを選べる
    - 同じ記録に、パラメータを変えたアルゴリズムを何度でもかけて比べられる
    - 「ゆっくり触る」「素早く叩く」「手をかざす」などのシナリオごとに記録を残し、`touch_baseline.md` §6 の確認項目の試験データにする
- **CSV 書き出し**: 時刻・キー毎の raw・イベントを CSV にする。Python や表計算ソフトでの解析用

### 4.4 PC 側のアルゴリズム実行

- 受信した（または再生した）フレームを、`touch_algo` の `TouchSignal::process()` に 1 フレームずつ通し、その結果を表示する
- **間引き**: 2ms 周期のデータを 8ms / 10ms 周期に間引いてからアルゴリズムに通せるようにする。最終構成（96 キー）のサンプル周期で、アルゴリズムがどう振る舞うかを評価するため
- **比較表示**: パラメータの組を 2 つ持ち、同じデータに通した結果を重ねて表示する（A/B 比較）

### 4.5 置き場所とビルド

```
loopian_qubit/
├── Cargo.toml              # ファーム（依存に touch_algo を path で追加）
├── crates/
│   └── touch_algo/         # no_std。TouchSignal などの純粋な計算
└── tools/
    └── qubit_monitor/      # PC アプリ（依存に touch_algo を path で追加）
        └── .cargo/config.toml
```

- リポジトリ直下の `.cargo/config.toml` が `build.target = "thumbv8m.main-none-eabihf"` を指定しており、その下のディレクトリにも効いてしまう。そのため `tools/qubit_monitor/.cargo/config.toml` に `[build] target = "host-tuple"` を書いて上書きする（cargo 1.93 で動作を確認済み。`host-tuple` はビルドするマシン自身のターゲットを表す）
- cargo の設定は実行したディレクトリから探されるので、PC アプリは `cd tools/qubit_monitor && cargo run` で実行する。リポジトリ直下から `--manifest-path` で指定すると、組み込み向けのターゲットが効いて失敗する
- `tools/qubit_monitor/Cargo.toml` には空の `[workspace]` を書き、直下のファームとは独立したプロジェクトにする。`Cargo.lock` と `target/` は `tools/qubit_monitor/` の下に別にできる
- rust-analyzer（VSCode）は `.vscode/settings.json` で `rust-analyzer.cargo.target` を thumbv8m に固定している。PC アプリ側も解析させるには `rust-analyzer.linkedProjects` などの設定が要る。今は設定しておらず、PC アプリのコードは VSCode で正しく解析されない（§6.1）。必要になったら設定を考える
- 直下での `cargo fmt` / `cargo clippy` は PC アプリに触れない。PC アプリと `touch_algo` のチェックは、今は CI に入れていない（PC アプリの確認は手元で `cargo clippy`・`cargo test` を行う）
- `touch_algo` はファームのビルド（thumbv8m）と PC アプリのビルド（ホスト）の両方で通るようにする。`embassy_time::Instant` には依存せず、時刻は `u32` の µs で受け取る
- ワークスペースにはしない（ターゲットが違うため）。それぞれが `touch_algo` を path 依存で使う
- `flake.nix` は変えていない（macOS では追加のものは要らない）。Linux で PC アプリをビルドするときは、`pkg-config`・`libudev` と X11 / Wayland のライブラリを足す

## 5. 通信プロトコル

### 5.1 ファーム → PC（バイナリ）

すべてリトルエンディアン。

```
+------+------+------+---------+---------------+-------+
| 0xA5 | 0x5A | type | len(u16)| payload (len) | crc8  |
+------+------+------+---------+---------------+-------+
```

- 先頭の 2 バイトで同期を取る。PC は受信の途中から読み始めても、ここを探して合わせられる
- crc8 は `type`・`len`・`payload` にかける（CRC-8/ATM、多項式 0x07）。一致しないパケットは捨てて数える

| type | 名前 | payload |
|---|---|---|
| 0x01 | FRAME | `seq: u32`, `time_us: u32`, `nkeys: u8`, `valid: [u8; ceil(nkeys/8)]`, `raw: [u16; nkeys]` |
| 0x02 | PROC | （段階 4 以降）`seq: u32`, `nkeys: u8`, キー毎の `filtered: u16`, `baseline: u16`, `output: u16`, `onset_ms: u32` |
| 0x10 | EVENT | `time_us: u32`, `kind: u8`, `data: [u8; 4]`（kind 毎の意味は下の表） |
| 0x20 | INFO | `version: [u8; 8]`, `build_date: [u8; 16]`, `nkeys: u8`, `scan_period_us: u32`, `dropped: u32` |
| 0x7F | TEXT | UTF-8 の文字列（ファームからの任意のメッセージ） |

- `nkeys` を毎フレーム持たせるので、6 キーでも 96 キーでも同じ形式で扱える
- INFO は接続直後と `info` コマンドへの応答で送る。`dropped` はファーム側のキュー溢れの累計
- `period` コマンドが成功したときも、応答として INFO（新しい周期を含む）を送る

EVENT の `kind` と `data`（2026-09-30 決定。使わないバイトは 0）:

| kind | イベント | data |
|---|---|---|
| 0x01 | Note On | `[0]` note, `[1]` velocity, `[2..4]` 位置 `u16`（キー位置 ×100。`TOUCH0-3` と同じ単位） |
| 0x02 | Note Off | 同上 |
| 0x03 | Note Moved（MIDI では Note Off として送るもの） | 同上 |
| 0x30 | マーカー | `u32` の番号（`mark` コマンドの引数） |

- FRAME と EVENT は別のキューから送るので、ストリームの中では時刻の順に並ぶとは限らない。PC 側は `time_us` で並べて扱う
- INFO の `version` には `BUILD_VERSION`（例 `v0.3.0`）、`build_date` には `BUILD_DATE`（`yy-mm-dd`）を入れ、余りは 0 で埋める（`build.rs` が埋め込む値）

### 5.2 PC → ファーム（テキスト）

改行区切りのテキストコマンド。シリアルターミナルから手で打っても試せる（ただし実験では使わず、PC アプリから送る。§6）。

| コマンド | 動作 |
|---|---|
| `start` | FRAME の送信を始める |
| `stop` | FRAME の送信を止める（EVENT は送り続ける） |
| `info` | INFO を返す |
| `period <µs>` | スキャン周期を変える。2000, 5000, 10000, 20000 のどれか（§3.3）。それ以外は周期を変えずにエラーを返す |
| `mark <番号>` | マーカーイベントを発生させる |
| `set <名前> <値>` | （段階 4 以降）ファーム側のアルゴリズムのパラメータを変える |

### 5.3 時刻

- 時刻は起動からの µs（`u32`）。約 71 分で一周するので、PC 側で一周を検出して 64 bit に伸ばす
- イベントの時刻も同じ基準にする（`embassy_time::Instant` は両コアで共通）

### 5.4 帯域

| 構成 | 1 フレーム | 周期 | 帯域 |
|---|---|---|---|
| 6 キー | 約 28 バイト | 2ms | 約 14KB/s |
| 96 キー | 約 218 バイト | 8ms | 約 27KB/s |
| 96 キー＋PROC | 約 1.2KB | 8ms | 約 150KB/s |

USB フルスピードの CDC で実用上 500KB/s 以上は出せるので、どの構成でも余裕がある。

## 6. 進め方

各段階の終わりに `cargo build`（`--features no_pca9544,debug_stream` とそれ以外の組み合わせ）と clippy が通ることを確認し、段階ごとにコミットする。

| 段階 | 内容 | 目的 |
|---|---|---|
| 1 | ファーム: CDC 複合化、FRAME・INFO 送信、`start`/`stop`/`info`<br>PC: 最小限の受信とリアルタイムのグラフ表示、記録 | 生値の時系列が見える状態を最短で作る |
| 2 | ファーム: EVENT、`period`、`mark`<br>PC: イベント表示、再生、統計、CSV 書き出し | チップの更新周期、ノイズの大きさ、hi/lo ずれの頻度を調べる。シナリオごとの記録を集める |
| 3 | `touch_algo` を作り、PC アプリで動かす。パラメータの GUI、間引き、A/B 比較 | `touch_baseline.md` の方針を PC 上で試して決める |
| 4 | ファームも `touch_algo` を使うようにする。PROC 送信、`set` コマンド | ファームの結果が PC 側と一致することを確かめ、実機で演奏して確認する |
| 5 | 96 キー構成（PCA9544 あり）で段階 2〜4 を繰り返す | 16 倍の構成で、周期・帯域・アルゴリズムが問題ないことを確かめる |

段階 1 は、**ファームを先に作ってコミットし、その後で PC アプリに進む**（2026-09-30 決定）。段階 2 もファームを先に作り、PC アプリは段階 1・2 の分をまとめて作る（同日決定）。シリアルターミナルは使わないので、ファームの実機での確認は PC アプリができてから行う。ファームだけの時点では、ビルドと clippy が通ることまでを確かめる。

PC アプリの `.qlog` のヘッダの形式（§4.3）と、PC アプリ・`touch_algo` のチェックを CI に入れるか（§4.5）は、PC アプリを実装するときに決めて、この設計書に書き足す。

段階 1 で観察したいこと:

- チップの値が本当に 8ms 毎に変わるか（2ms で読むと、同じ値が約 4 回続くはず）
- 無操作時のノイズの大きさと、その周波数的な特徴（ゆっくりしたうねりか、単発のスパイクか）
- hi/lo ずれ（値が 256 前後飛ぶ）がどのくらいの頻度で起きるか
- 触れたとき・離したときの立ち上がり・立ち下がりにかかる時間

### 6.1 実装の記録

各段階を実装したときに、設計から補ったこと・途中の状態を記録する。

**段階 1（ファーム）**（2026-09-30）

- ファイル
    - `src/tasks/debug_stream.rs`: `debug_stream_task`（Core0）
    - `src/debug_protocol.rs`: パケットの組み立て（`Packet`）と CRC-8/ATM
    - `src/shared.rs`: `DebugFrame`・`DEBUG_FRAMES`・`DEBUG_STREAMING`・`DEBUG_DROPPED`
    - いずれも `debug_stream` feature のときだけコンパイルする。feature が無いときの USB の構成・ディスクリプタのバッファ（256 バイト）・スキャン周期（10ms）は今までどおり
- **スキャン周期**: 段階 1 では `period` コマンドが無いので、`SCAN_PERIOD_MS` を const のまま `debug_stream` のときだけ 2 にした。`SCAN_PERIOD_US`（Atomic）への置き換えと `Ticker` の作り直しは、`period` コマンドと合わせて段階 2 で行う
- **`DEBUG_STREAMING`**（設計に追加）: `debug_stream_task` が `start` を受けたら true、`stop`・切断で false にする。`touch_task` は true の間だけ `DEBUG_FRAMES` に入れる。PC が読んでいない間にキューがあふれて `DEBUG_DROPPED` が増え続けないようにするため。`DEBUG_DROPPED` は本当の取りこぼしだけを数える
- **生値**: `ReadTouch` に `debug_raw`（補正前の生値）と `debug_valid` を持たせ、`debug_raw()` で読む。`valid` はスキャンの始めに 0 にし、読めたキーのビットを立てる
- **`seq`** は `touch_task` のスキャン番号（`frame`）をそのまま使う。**`time_us`** はスキャン開始の `Instant` の µs。embassy-time の時刻は起動（`embassy_rp::init`）からで、Core1 の起動からではない（§3.3 の構造体のコメントより正確には「起動から」）
- **USB**: CDC のパケット長は 64。MIDI・CDC とも `builder.function()` で IAD が付く。インターフェースは MIDI 2 + CDC 2 = 4 個で、embassy-usb の既定の上限（4）に収まる。USB がリセットされると DTR は false に戻る
- **コマンドの応答**（設計に追加）: `start` / `stop` には TEXT で `ok: start` / `ok: stop` を返す。知らないコマンドには `error: unknown command: <コマンド>`、32 バイトを超える行には `error: command too long` を返す。行の区切りは CR・LF のどちらでもよい（空行は無視する）
- **TEXT** の payload は 64 バイトまでで、超えた分は切り捨てる
- **切断の判定**: DTR が落ちた・`read_packet` がエラー（USB の切断）・`write_packet` が 100ms でタイムアウト、のいずれか。切断したら最初に戻って DTR を待つ。書き込みのタイムアウトで DTR が立ったままのときは、すぐに接続し直して INFO を送り、`start` を受ける前の状態から始める。PC アプリは INFO を受けたら `start` を送り直せばよい
- **接続の始め**に、前の接続で `DEBUG_FRAMES` に残ったフレームを捨てる
- エラーコード 25 を `error::SPAWN_DEBUG_STREAM` として定義した（`debug_stream` のときだけ）

**段階 2（ファーム）**（2026-09-30）

- **スキャン周期**
    - `constants.rs` の `SCAN_PERIOD_MS`（ms）を `SCAN_PERIOD_US`（µs、既定値の const）にした。`debug_stream` では 2000、無いときは 10000
    - 実行中の周期は `shared.rs` の `SCAN_PERIOD_SETTING_US`（Atomic、非公開）に持ち、`scan_period_us()` で読み、`set_scan_period_us()` で変える。§3.3 で `SCAN_PERIOD_US`（Atomic）と書いた名前は、既定値の const に使った。`debug_stream` が無いときは `scan_period_us()` は const を返すだけ
    - 選べる周期は `SCAN_PERIODS_US`。`ANALYSIS_DIVIDER`（const）は `analysis_divider(周期)`（const fn）にした。10ms を周期で割り、0 になる（20ms）ときは 1
    - `touch_task` は `ticker.next()` の直後に周期を確かめ、変わっていたら `Ticker` を作り直す。その周期のスキャンはそのまま行い、次の周期から新しい間隔になる。`seq`（`frame`）は続けて数える
- **EVENT**
    - `DebugEvent`・`DEBUG_EVENTS`（容量 16）を `shared.rs` に置いた
    - `tasks::debug_stream::queue_event()` で入れる。PC がポートを開いている間だけ入れるため、`DEBUG_CONNECTED`（Atomic）を追加した。`debug_stream_task` が DTR を確かめた後に true、切断で false にする。接続の始めに、前の接続で残ったイベントを捨てる
    - Note のイベントは `touch_task` の MIDI コールバックで、チャンネルを付ける前の status（`RINGLED_CMD_TX_ON` / `OFF` / `MOVED`）から kind を決める。位置は `(location × 100) as u16`
    - `debug_stream_task` は `select4` で FRAME・EVENT・コマンドの受信・DTR の変化を待つ
- **コマンド**
    - `period <µs>`: 成功したら INFO を返す。選べない値のときは TEXT で `error: period must be 2000, 5000, 10000 or 20000` を返す（数値でないときは下の `unknown command`）
    - `mark <番号>`: 番号は `u32`。EVENT（kind 0x30）をその場で送り、TEXT の応答は返さない
    - 引数が要るコマンドに引数が無い・数値でない、引数の要らないコマンドに引数がある、ときは `error: unknown command: <行>` を返す

**PC アプリ（段階 1・2）**（2026-09-30）

- **置き場所と構成**: `tools/qubit_monitor/`（§4.5 のとおり、空の `[workspace]` と `target = "host-tuple"`）。画面以外（`protocol`・`qlog`・`store`・`serial`・`playback`・`csv_export`）はライブラリ（`src/lib.rs`）に置き、画面は `src/main.rs`・`src/app.rs`。`touch_algo` は段階 3 で作る
- **egui の版**: 最新の egui 0.36 は rustc 1.95 を要するため、ファームと同じ stable 1.93 でビルドできる eframe 0.35 / egui_plot 0.36 にした（`Cargo.toml` の `rust-version = "1.93"`）。ツールチェインを上げるときに版も上げてよい
- **日本語の表示**: egui の既定のフォントには日本語が無いので、起動時に OS のフォント（macOS はヒラギノ角ゴシック）を読み込んで足す。見つからなければ日本語は表示されない
- **受信**: シリアルポートは別スレッドで読み、UI にはバイト列を渡す。記録（`.qlog` への書き込み）もそのスレッドで行い、描画が遅れても受信したバイト列を欠けなく保存する
- **接続直後の INFO**（実機で確認して対処）: macOS の serialport クレートは、ポートを開く処理の最後に受信バッファを捨てる（`tcflush`）。ファームは DTR が立つとすぐ INFO を送るので、その INFO が捨てられて届かなかった。PC アプリはポートを開いた直後に `info` を送って INFO を受け取り直す。ファームは変えない
- **start の送り直し**: 開始中に INFO を受け取ったら `start` を送り直す。ファームが書き込みのタイムアウトで接続し直したとき（§6.1 段階 1）に、フレームの送信を再開させるため。`period` の応答の INFO でも送るが、ファームは `ok: start` を返すだけで害は無い
- **表示**
    - 時系列グラフの系列は、段階 1・2 では raw と「補正後」（hi/lo ずれ補正。ファームの `read_touch` と同じ規則 `raw > 前回 + 200 なら raw - 256` を PC 側でかけた値）。filtered・baseline・delta・output・onset と下段のグラフ、パラメータの欄は段階 3 で加える
    - 点が多いときは、区間毎の最小・最大に間引いて描く（1 本あたり 4000 区間まで）。読み取りに失敗したサンプルは描かない
    - 一時停止中はグラフをドラッグ・拡大でき、統計もその範囲で計算する
    - 全キーの今の値はバー表示にした（表示範囲の最小値を引く選択あり）。96 キーの円形表示は段階 5 で考える
    - 保持するフレームは最大 100 万（2ms 周期で約 33 分）で、超えたら古いものから捨てる
- **統計**（表示している時間の範囲で計算する）: キー毎の平均・標準偏差・p-p・hi/lo ずれの回数、サンプル間隔（最小・平均・最大とヒストグラム）、seq の欠け、選んだキーの「生値が変わるまでの時間」（チップが値を更新する周期を見るため）、表示範囲のイベントの一覧、ファームからの TEXT
- **再生**: `.qlog` を読み込み、受信と同じ `Parser` と `Store::ingest` に、記録の時刻に合わせて流す。速さは ×0.25〜×64、一時停止・コマ送り（次のフレームまで）・最初から。再生を始めると実機との接続は切る（データが混ざらないように）
- **CSV**: `.qlog` を選んで書き出す（表示中のデータではなく、ファイル全体）。列は `time_us,seq,valid,key0..,event`。フレームの行は生値、イベントの行（Note・マーカー・INFO・TEXT）は `event` 列に説明を入れる。時刻の順に並べ、時刻を持たない INFO・TEXT は直前の行の時刻にする
- **確認用のプログラム** `examples/probe.rs`（`cargo run --example probe`）: GUI を使わず、PC アプリと同じライブラリでファームとの通信を確かめる。INFO → start → FRAME → period（5・20ms）→ mark → 不正なコマンド → stop を順に試し、その間の記録を再生・CSV 書き出しして、受信したフレーム数と一致するかを見る
- **ファームとの確認の結果**（段階 2 のファーム、6 キー直結、センサーに触れない状態）
    - start / stop / info / period / mark と、不正なコマンドへのエラーは設計どおり。CRC エラー・長さ違い・読み飛ばし・seq の欠けは 0。記録→再生のフレーム数は受信と一致
    - 周期 5ms・20ms では、サンプル間隔は 5.00 / 20.00ms（±0.03ms）
    - **周期 2ms では、約 1/12 の間隔が 4.25ms 前後になる**（ほかは 1.75〜2.25ms、平均 2.19ms、約 456 フレーム/秒）。12 スキャンに 1 回のチップの基準値の読み直し（`set_reference()`、§1.2）でそのスキャンが 2ms を超え、周期超過として `ticker.reset()` されるため。`PERIOD_OVERRUN` もそのたびに増える。基準値の読み直しを削除する段階 3・4 で解消する見込み
    - 触れていないときのノイズは、σ 0.4〜1.1、p-p 1〜5（6 キー）。hi/lo ずれは観測されなかった
- **ファームへの影響の確認**: リポジトリ直下の `cargo build`・`cargo clippy --all-features -- --deny=warnings`・`cargo fmt --check` は、`tools/` を加えても変わらず通る（直下の `cargo fmt` は `tools/` に触れない）。`tools/qubit_monitor/target/` は直下の `.gitignore` の `target/` で無視される
- **まだしていないこと**
    - GUI の操作の確認（ボタン・ダイアログ・グラフの表示）は、実機をつないで行う
    - Note のイベントは、センサーに触れないと出ないので、GUI での実験のときに確かめる
    - CI には入れていない。VSCode の rust-analyzer は `.vscode/settings.json` でターゲットを thumbv8m に固定しているので、PC アプリのコードは正しく解析されない（§4.5）。必要になったら設定を考える
    - `flake.nix` は変えていない。Linux でビルドするには、serialport のために `pkg-config` と `libudev`、eframe のために X11 / Wayland のライブラリが要る

## 7. 96 キーに戻すときのための配慮

- プロトコルは `nkeys` を持ち、`valid` も可変長。ファームのキー数が変わっても、PC アプリは変更なしで動く
- `DebugFrame` は `constants::TOTAL_QT_KEYS` で配列の大きさを決める
- PC アプリの表示は、キーを選んで表示する方式と、全キーのバー／円形表示を持つ。96 本の折れ線を同時に描くことは前提にしない
- 最終構成ではスキャン周期が 8ms 前後になる。アルゴリズムは PC 側で 8ms に間引いたデータでも評価しておく（§4.4）
- `touch_algo` の近傍計算（リングの折り返し）は、6 キーでも 96 キーでも同じコードで動くようにする。6 キーの実験では両端のキーの近傍が折り返して反対側になり、物理的な配置とは合わないので、そこは注意して評価する

## 8. 未決事項

- スキャンの既定周期を 2ms にしたときの Core1 の負荷。実測では、通常のスキャンは 2ms に収まるが、12 スキャンに 1 回のチップの基準値の読み直しで 2ms を超え、そのたびに周期超過（間隔が約 4.25ms に延びる）になる（§6.1）。基準値の読み直しを削除する段階 3・4 で解消する見込み
- 記録ファイルの置き場所と、リポジトリで管理するか（試験データとして一部を残すか）
- Python の補助ツール（記録ファイルの解析）を用意するか
