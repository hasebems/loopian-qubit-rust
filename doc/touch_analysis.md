# タッチ解析（複数の電極から 1 つのタッチを求め、演奏情報を取り出す） 設計書

2026-11-08 書き始め。

## 1. 背景と目的

### 1.1 本書の範囲

タッチの処理は、次の 2 段に分かれる。

| 段 | 扱うもの | 実装 | 設計書 |
|---|---|---|---|
| 信号処理 | 電極 1 つずつ。ノイズ除去・基準値・noise_floor を扱い、電極毎の output を出す | `crates/touch_algo` の `electrode.rs`（`ElectrodeSignal`） | `doc/touch_baseline.md` |
| 解析（本書） | 複数の電極の output から 1 つのタッチを求め、そこから演奏情報（ノート・ベロシティ・ビブラートなど）を取り出す | `crates/touch_algo` の `touch.rs`（`QubitTouch`）。2026-11-08 に `src/touch/qtouch.rs` から移した（§7） | 本書 |

入口は `doc/touch_baseline.md` §3.9 の受け渡し（`ElectrodeSignal::process()` の output を `QubitTouch::set_value()` で渡す）である。

### 1.2 目標

1. **（第一の目標）1 本の指のタッチを、1 つのタッチとして求める。そのタッチは depth と position の 2 つの情報を持つ**（§3）
2. 求めたタッチから演奏情報を取り出す（ノート・ベロシティ・ビブラートなど。§4、後で検討する）
3. 複数の指（最大 `MAX_TOUCH_POINTS` 点）への拡張（後で検討する）

## 2. 用語

| 用語 | 意味 |
|---|---|
| 電極 | タッチセンサの 1 つの電極（electrode）。AT42QT1070 の 1 チャンネルに当たる。`touch_algo` の output が電極毎に 1 つある。データシート・`doc/touch_baseline.md`・コードの多くでは key（キー）と呼んでいるが、本書では電極と呼ぶ |
| output | `touch_algo` が電極毎に出す値。`delta − noise_floor`（0 未満は 0）。未タッチで 0、触れると正（`doc/touch_baseline.md` §2） |
| タッチ | 1 本の指が触れていることを表すもの。隣り合う複数の電極の output から 1 つ求める。**タッチは指 1 本の単位だけに使い、電極 1 つの反応はタッチと呼ばない**（「電極が反応する」と言う）（2026-11-08 決定） |
| depth | タッチの深さ。指がどれだけ強く・広く触れているかを表す量（§3.2） |
| position | タッチの位置。電極番号を単位とする実数（電極間の位置も表す）（§3.3） |

## 3. 第一の目標: 1 本の指のタッチ

### 3.1 考え方

1 本の指は、電極の幅より広いので、隣り合う複数の電極（2〜3 個程度）にまたがって触れる。電極毎の output を個別に見るのではなく、まとめて **1 つのタッチ** とし、そのタッチを次の 2 つの量で表す。

- **depth**: どれだけ触れているか（§3.2）
- **position**: どこに触れているか（§3.3）

演奏情報（§4）は、この 2 つの量とその時間変化から取り出す。電極毎の output は、ここから先の処理には直接使わない。

### 3.2 depth（2026-11-08 決定）

**1 本の指とみなした電極の output を、すべて足したもの。**

```
depth = Σ output[k]    （k は 1 本の指とみなした電極）
```

- 指の押しつけの強さと、触れている面積の両方を含む量になる
- 現状の強度（intensity、§5）と同じ考え方。違いは足す電極の範囲で、現状は山の前後 3 電極（計 7 電極）と決め打ちにしている。どの電極を 1 本の指とみなすかは §3.4 で決める

### 3.3 position（2026-11-08 決定）

**1 本の指とみなした電極の output の重心。** 現状（§5）と同じ。

```
position = Σ (k × output[k]) / depth
```

- 単位は電極番号で、電極の間の位置も実数で表す
- 足す電極は depth と同じ（§3.4）

### 3.4 1 本の指とみなす電極の範囲

（検討中）depth・position のどちらも、この範囲で決まる。現状の `QubitTouch`（§5.3 の 1〜3）を元にし、§6.1 の問題をここで扱う（2026-11-08 方針）。

### 3.5 タッチの有無の判定

（検討中）

## 4. 演奏情報の取り出し

（第一の目標が決まってから検討する）

## 5. 現状の実装（`QubitTouch`）の仕様

2026-11-08 時点の `src/touch/qtouch.rs`（同じ日に、処理を変えずに `crates/touch_algo/src/touch.rs` へ移した。§7.5）。Arduino 版から移植したもので、`doc/touch_baseline.md` §4.6 で閾値などを 1/4 にした以外は変えていない。§3.4（1 本の指とみなす電極の範囲）は、これを元に決める。

### 5.1 呼び出し

`touch_task`（Core1）が、解析の周期（10ms 毎。`analysis_divider()`）に次を行う。

1. 全電極について `set_value(電極番号, touch_algo の output)`
2. `seek_and_update_touch_point(work_mode)`
3. `lighten_leds()`（今は何もしないコールバック）

Note On/Off と CC は、`QubitTouch::new()` に渡したコールバックで `MIDI_TX` に入れる。コールバックは `status` の上位 4 ビットにチャンネルを足すだけで、`note`・`velocity` はそのまま送る。

### 5.2 データ構造

| 構造 | 内容 |
|---|---|
| `Pad`（電極数ぶん） | 電極の output。移動平均の構造が残っているが、`MAX_MOVING_AVERAGE` = 1 なので値そのもの |
| 候補（`(f32, f32, i16)` × `MAX_TOUCH_POINTS`） | その周期で見つかった (山の電極番号, position, depth)。周期毎に作り直す |
| `TouchPoint`（`MAX_TOUCH_POINTS` = 4 個） | 周期をまたいで追跡するタッチ。position・depth（`intensity`）・今のノート・継続時間・位置の履歴（128 サンプル）を持つ |

### 5.3 処理の流れ（`seek_and_update_touch_point`）

1. **山を探す（`scan_pads`）**: 電極 0 から順に、隣との差の符号を見る。左隣より大きく、右隣より大きい（どちらも `>`）電極を山とし、その output が `TOUCH_THRESHOLD`（8）より大きければ候補にする。電極番号は `MAX_PADS` で巡回させる（最後の電極と最初の電極を隣とみなす）。候補が `MAX_TOUCH_POINTS`（4）個になったら、そこで探すのをやめる
2. **depth と position を求める（`decide_touch_point`）**: 山を中心に、前後 `FINGER_RANGE`（3）電極、計 7 電極で
    - depth = 7 電極の output の合計（§3.2）
    - position = 7 電極の重心（§3.3）。電極番号は巡回させずに `山 − 3 〜 山 + 3` のまま掛ける
    - 単発のノイズを捨てる: 山の電極が depth の 70% 以上（`SINGLE_PAD_SPIKE_RATIO_PERMILLE`）で、かつ周りの最大が山の 1/4 以下（`SINGLE_PAD_SPIKE_NEIGHBOR_DIV`）なら、候補を無効にする
3. **近い候補をまとめる（`merge_close_candidates`）**: position の差が `CANDIDATE_MERGE_RANGE`（1.6 電極）以内の候補は、depth の大きい方 1 つだけを残す（position・depth は計算し直さない）
4. **前の周期のタッチと結び付ける（`collate_touch_point`）**: 候補を順に、まだこの周期で更新していない TouchPoint のうち position の最も近いものと比べ、`CLOSE_RANGE`（3 電極）以内なら同じ指が動いたとして更新する。無ければ空いている TouchPoint で新しいタッチを始め、Note On を送る
    - 更新したとき、position がノートの境目を `HISTERESIS`（0.7 電極）越えたら、新しいノートの Note On と古いノートの Note Off（`RINGLED_CMD_TX_MOVED`）を送る
5. **離れたタッチを消す（`erase_touch_point`）**: この周期に更新されなかった TouchPoint は、`RELEASE_WAITING_TIME`（5 周期 = 50ms）続けて更新が無ければ離れたとみなし、Note Off を送る。待っている間は position を保ち、候補が `CLOSE_RANGE` 以内に戻れば同じタッチとして続く
6. **継続時間とビブラート（`update_durations_and_vibrato`）**: どれかが触れていれば `on_time` を更新し `ANY_TOUCH` を立てる。Violin モードでは、position の履歴からビブラート（1.5〜8Hz の往復）を検出し、CC1・CC19 を送る

### 5.4 ノートとベロシティ（§4 で扱う。参考）

- ノート: Piano は position を四捨五入した電極番号 + `PIANO_OFFSET`。Violin は position / 2 を四捨五入 + `VIOLIN_OFFSET`（2 電極で 1 ノート）
- ベロシティ: Piano は Note On の周期の depth から `(100 + depth / 4) / 2`、depth > 63 で 127（2026-11-08 に半分にして 127 で頭打ちにした。§6.3 の 1）。Violin は直前のノート・継続時間・離していた時間から決め、depth は使わない

### 5.5 定数

| 定数 | 値 | 意味 |
|---|---|---|
| `TOUCH_THRESHOLD` | 8 | 山の output がこれより大きければ候補にする |
| `FINGER_RANGE` | 3 | depth・position を求める片側の電極数（計 7 電極） |
| `SINGLE_PAD_SPIKE_RATIO_PERMILLE` | 70 | 山の電極が depth のこの % 以上なら単発のノイズの候補（名前は ‰ だが % として使っている） |
| `SINGLE_PAD_SPIKE_NEIGHBOR_DIV` | 4 | 周りの最大が山の 1/4 以下なら単発のノイズの候補 |
| `CANDIDATE_MERGE_RANGE` | 1.6 | 同じ周期の候補をまとめる距離（電極） |
| `CLOSE_RANGE` | 3.0 | 前の周期のタッチと同じ指とみなす距離（電極 / 10ms） |
| `HISTERESIS` | 0.7 | ノートを切り替える position のヒステリシス（電極） |
| `RELEASE_WAITING_TIME` | 5 | 離れたとみなすまでの周期数（50ms） |
| `MAX_TOUCH_POINTS` | 4 | 候補・TouchPoint の最大数（`constants.rs`） |

## 6. 現状の問題点

コードを読んで見つけたもの（2026-11-08）。実機・記録ではまだ確かめていない。「確実」はコードから必ず起きると言えるもの、「おそれ」は条件によって起きるもの。

### 6.1 1 本の指の範囲・depth・position（第一の目標。§3.4 で扱う）

1. **平らな山を見落とす（確実）**: 山の判定が両側とも `>` なので、隣り合う 2 電極が同じ値の山（例: output が 3, 10, 10, 2）は候補にならない。指がちょうど 2 電極の間にあるときに起きやすい。その周期はタッチが無いことになり、50ms 以内に戻れば `RELEASE_WAITING_TIME` で隠れるが、depth・position が更新されず、続けば Note Off になる
2. **窓が 7 電極に決め打ち**: 指が実際にまたがる電極数と関係なく、山の前後 3 電極を足す
    - 端の 3 電極にノイズ（noise_floor を超えたぶん）が乗ると、depth に加わり、position が引っぱられる
    - 4〜6 電極離れた 2 本の指は、窓が重なり、互いの output を取り込む。depth が大きくなり、position が互いに寄る（第三の目標で効く）
3. **リングのつなぎ目で position が巡回しない（確実）**: 窓の電極の値は巡回させて読むが、重心には電極番号を巡回させずに掛ける。山が電極 0 の近くなら position は負になりうる（例: −0.4）、最後の電極の近くなら `MAX_PADS − 1` を超えうる。さらに
    - ノートの計算で 0〜`MAX_PADS − 1` に切り詰めるので、つなぎ目の近くでは position の変化がノートに出ない
    - 指がつなぎ目を越えると、position が約 `MAX_PADS` 飛ぶので、`CLOSE_RANGE` に入らず、別のタッチになる（Note Off と Note On が出る）
4. **リングでない構成でも巡回させている（確実）**: etude（12 電極）・no_pca9544（6 電極）では両端は隣り合っていないが、最後の電極と最初の電極を隣として扱う。電極 0 の近くに触れると、電極 9〜11 の output（ノイズ）が窓に入り、position を負の側へ引っぱる
5. **noise_floor の大きい電極（おそれ）**: output は `delta − noise_floor` なので、noise_floor の大きい電極（etude の key5 は 13、他は 1〜2）は、同じように触れても output が小さい
    - position がその電極から離れる向きにずれ、depth も小さくなる
    - その分、山がへこんで、1 本の指で山が 2 つできることがある（key4 と key6）。3 でまとめられるが、残るのは片方の候補の position で、まとめた結果の重心ではない
    - 弱く触れたとき、周りが 0 になり、単発のノイズとして捨てられることがある
6. **候補の数の上限（おそれ）**: 山の探索は候補が 4 個になったところで打ち切る。単発のノイズ・窓の端の小さな山・後でまとめられる候補も 1 個に数えるので、電極番号の大きい側にある本当の指が候補にならないことがある
7. **候補をまとめた後の値（確実）**: 3 で depth の大きい候補だけを残すので、position・depth はまとめた範囲で計算し直したものではない。まとめる判定も最初の候補からの距離だけで見る
8. **電極による感度の違い（おそれ）**: noise_floor の他に、電極の大きさ・配線の長さ・チップ毎の違いで、同じ触れ方でも delta の大きさが電極で違う可能性がある。position の偏りになる。96 電極の基板で確かめる
9. **depth の型（おそれ）**: depth を `i16` で足しているので、output が 7 電極平均で 4681 を超えると桁あふれする。dev ビルドでは overflow-checks で panic（エラー 55）になる。今の output（数十〜数百）では起きない

### 6.2 タッチの有無・追跡（§3.5 で扱う）

1. **触れ始めの判定が山の値だけ**: 山の電極の output が 8 を超えた最初の周期で、すぐに Note On を出す。depth は見ない。ヒステリシスも無いので、しきい値の近くでは、6.1 の 1 などで途切れたときに、離れたかどうかが `RELEASE_WAITING_TIME` 頼みになる
2. **離れてから 50ms は同じタッチ**: 離れても 50ms は TouchPoint が残るので、その間に 3 電極以内を叩き直すと、新しいタッチにならない
    - 同じ電極を叩き直すと、Note On が出ない（連打が抜ける）
    - 近くの電極を叩くと、移動として扱われ、Note Off・Note On（MOVED）になる
3. **結び付けが候補の順番による**: 候補を電極番号の順に、その時点で最も近い TouchPoint に結び付ける（貪欲法）。2 本の指が近づいたとき、入れ替わることがある（第三の目標で効く）
4. **TouchPoint が空いていないとき**: 4 個とも使っている（離れるのを待っているものを含む）と、新しい候補は何も言わずに捨てられる

### 6.3 演奏情報（§4 で扱う）

1. ~~**Piano のベロシティが MIDI の範囲を超える（確実）**~~: `default_note_on_velocity()` は 100〜115 か 255 を返し、コールバックはそれをそのまま Note On のデータバイトにしていた。MIDI のデータバイトは 0〜127 なので、128 以上は不正な値になる。Arduino 版から引き継いだもの
    - 2026-11-08 修正: 今までの値を半分にして、127 で頭打ちにした（50〜57、depth > 63 で 127）。ハードウェアが Arduino 版と違うので、互換は取らない。Note On は depth が立ち上がり始めた最初の周期で出る（§6.3 の 2）ので、depth はまだ小さい。記録 `qubit_20261027_211117.qlog` を PC で通すと、ベロシティは 51〜54 だった。触れ方で変わるようにするのは §4 で検討する
2. **ベロシティを触れ始めの 1 周期で決める**: Note On は depth が 8 を超えた最初の周期で出るので、depth はほとんど立ち上がっていない。ベロシティは触れ方によらず、ほぼ一定（100 付近）になる
3. **位置の履歴を新しいタッチで消さない（確実）**: `location_history` は TouchPoint を使い回しても初期化しない。新しいタッチから 1.28 秒（128 サンプル）は、前のタッチの position と混ざり、ビブラートを誤って検出することがある（Violin モード）
4. **ビブラート検出の周期**: `TOUCH_SAMPLE_PERIOD_SEC` を 10ms に決め打ちしている。ただし履歴に入るのは更新した周期だけで、離れるのを待っている間は入らない

### 6.4 コードの整理（動作には関わらない）

- `Pad` の移動平均の構造（`MAX_MOVING_AVERAGE` = 1）が残っている（`doc/touch_baseline.md` §4.6）
- `touch_count` は 1 と 3 で上書きされるだけで、使われていない（`_get_touch_count` だけ）
- `SINGLE_PAD_SPIKE_RATIO_PERMILLE` は ‰ ではなく % として使っている
- `scan_pads` の `i - 1 + MAX_PADS` は `i` = 0 のときに `u16` の桁あふれになる式だが、`i` = 0 では候補にならない（`diff_before` が 0）ので通らない
- `proper_pad()` は読むだけでも `&mut` を取る

## 7. 実装の場所（2026-11-08 方針。手順 1 は同日に実装）

解析も `touch_algo` と同じく、ファームと PC アプリ（qubit_etude）で同じコードを使う。PC で記録を通して確かめ、直したものがそのまま実機の動作になるようにする。

### 7.1 クレートとファイル

新しいクレートは作らず、`crates/touch_algo` に入れて、ファイルを分ける。

| ファイル | 内容 | 扱う単位 |
|---|---|---|
| `src/lib.rs` | モジュールの宣言だけ | |
| `src/electrode.rs` | 今の `lib.rs` の中身（ノイズ除去・基準値・noise_floor。`doc/touch_baseline.md`） | 電極 1 つずつ |
| `src/touch.rs` | `src/touch/qtouch.rs` から移すもの（本書） | 指 1 本 = タッチ |

1 つのクレートにする理由:
- 使うところが同じ（ファームの `touch_task` と qubit_etude）で、必ず「信号処理 → 解析」の順につないで使う。片方だけ使う場面が無い
- どちらも no_std・依存無し・ホストでテストする。クレートを分けると `Cargo.toml`・path 依存・テストの手順が 2 つずつになる
- noise_floor の大きい電極の扱い（§6.1 の 5）のように、信号処理の出し方と解析の受け取り方を一緒に変えることが多い

### 7.2 名前の付け方（2026-11-08 決定）

電極 1 つの処理も、指 1 本の処理も「touch」と呼んでいて紛らわしいので、次のように分ける。

- 電極 1 つの単位のもの: **electrode**
- 指 1 本の単位のもの: **touch**（§2）

これに沿って、名前を次のように変えた（2026-11-08 実装）。

| 今の名前 | 新しい名前 | 備考 |
|---|---|---|
| `touch_algo::TouchSignal` | `touch_algo::electrode::ElectrodeSignal` | 電極の信号処理なのに Touch と付いている |
| `touch_algo::KeyState` | `touch_algo::electrode::ElectrodeState` | |
| `touch_algo::Params` / `State` / `MAX_SMOOTH_SAMPLES` | `touch_algo::electrode::` の下に移す（名前はそのまま） | 解析のパラメータは `touch_algo::touch::` に別に置く |
| `QubitTouch`・`TouchPoint` | `touch_algo::touch::` に移す（名前はそのまま） | |
| `Pad` | 無くす | 電極の output をそのまま配列で受け取れば足りる（§6.4） |

- 古い名前を残す再公開（`pub use`）はしない。使っているのはファームの `src/tasks/touch.rs` と qubit_etude の `algo.rs`・`app.rs` の 3 か所だけなので、そちらを直す
- ファームの定数（`TOTAL_QT_KEYS` など）と、`src/touch/`（`read_touch.rs` など）の名前は今回は変えない。AT42QT1070 の用語（key）に合わせたもので、変えると範囲が広がるため（§9）

### 7.3 移すときに外すファームへの依存

`qtouch.rs` はほとんどが計算で、ファームに頼っているのは次の 4 つだけ。

1. **共有状態への書き込み**: `TOUCH0-3`・`ANY_TOUCH`・`DEBUG_VALUE` を中で書いている。外して、タッチの状態（position・depth・触れているか）を読み出せるようにし、Atomic へ書くのは `touch_task` で行う
2. **電極数**: `TOTAL_QT_KEYS`（feature で 6 / 12 / 96 に変わる）を定数で使っている。PC では記録によって変わるので、`TouchSignal<K>` と同じく外から与える。リングかどうか（§6.1 の 4）も一緒に渡す
3. **定数と型**: `MAX_TOUCH_POINTS`・`PIANO_OFFSET`・`VIOLIN_OFFSET`・`WorkMode`・`RINGLED_CMD_*`・`MIDI_CC`。`touch_algo` に移し、ファームの `constants.rs` はそれを使う
4. **MIDI の出力**: 今のコールバック `Fn(status, note, velocity, location)` はそのまま使える。PC ではイベントを溜めて表示する

### 7.4 手順

1. **動作を変えずに移す**: §6 のバグもこの段階では直さない
2. **同じに動くことを確かめる**: 記録（`.qlog`）にはファームが出した Note On/Off/Moved の EVENT も入っている。記録を PC で `electrode` → `touch` に通し、出てきたイベントが記録のイベントと一致するかを比べる
    - ファームは解析を 10ms 毎に呼ぶ（`analysis_divider()`）。PC でも記録したときのスキャン周期から同じフレームを選んで渡さないと一致しない
3. **qubit_etude に表示を足す**: タッチの position・depth と Note のイベント。まず `replay_algo` の出力に足し、GUI はその後
4. **PC でバグを直す**: §6 のバグを直し、記録で直す前と後を比べてから、ファームに入れる

### 7.5 手順 1 の実装（2026-11-08）

処理は変えずに移した。§6 のバグもそのまま残っている（ベロシティ（§6.3 の 1）だけは、実機で試す前に直した）。移すときに変えたのは次の点。

- **電極数**: `QubitTouch<F, P>` の `P`（`[u16; N]` や `Vec<u16>`）で渡す。`QubitTouch::new(コールバック, 0 で埋めた P)`。電極数を使っていた計算（`new_location_piano` / `_violin` の切り詰め、`collate_touch_point` の初期値）には、`P` の長さを渡す。電極数が 0 のときは何もしない（PC 向け）
- **`Pad` を無くした**: `MAX_MOVING_AVERAGE` = 1 では値そのものなので、`P` の値を直接使う
- **共有状態**: `TOUCH0-3`・`ANY_TOUCH`・`DEBUG_VALUE` は `touch_task` の `publish_touch_state()` が解析の後に書く。読み出しに `touch_point(id)`（`is_touched()`・`get_location()`・`get_intensity()`）・`is_any_touched()`・`vibrato()` を足した
    - `TOUCH0-3` は、これまで変化のあったタッチポイントだけを解析の途中で書いていた。今は毎回 4 つとも書く。書く値（触れていれば position ×100、触れていなければ 10000）は同じ
- **定数と型**: `MAX_TOUCH_POINTS`・`WorkMode`・`KEYBD_LO`・`PIANO_OFFSET`・`VIOLIN_OFFSET` を `touch_algo::touch` に移した。ファームの `constants.rs` は `MAX_TOUCH_POINTS` と `WorkMode` を再公開する
- **コールバックの status**: `STATUS_NOTE_ON`（0x90）・`STATUS_NOTE_OFF`（0x80）・`STATUS_NOTE_MOVED`（0xa0）・`STATUS_CC`（0xb0）を `touch_algo::touch` に置いた。ファームの `RINGLED_CMD_TX_*`・`MIDI_NOTE_ON`・`MIDI_NOTE_OFF`・`MIDI_CC` は使うところが無くなったので消した
- **公開の範囲**: 使われていなかった `_deb_val`・`_debug`・`get_touch_point`（`&mut`）・`proper_pad` を消し、`_get_value` → `value`、`_get_touch_count` → `touch_count`、`_touch_point` → `touch_point` にした
- **テスト**: ホストのテストを 2 つ足した（1 本の指の Note On/Off、指を動かしたときの Note On と MOVED）

残り: 手順 2（記録の EVENT との照合）・4（バグの修正）。実機での確認もまだ

### 7.6 qubit_etude での表示（2026-11-08、手順 3）

- `algo.rs`（`AlgoRunner`）: 信号処理の後、同じフレームで output を `QubitTouch` に渡して解析する（動作モードは Piano）。タッチポイント毎の (position, depth) と、出てきたイベントを時刻と一緒にためる。ファームは信号処理と解析を 10ms 毎に行うので、同じ条件にするには間引きを 10ms にする
- GUI: 右の欄の「タッチ」タブ（2026-11-08 に右の欄を「電極」「統計」「タッチ」のタブに分けた。イベントが出るたびに欄の高さが変わり見づらかったため）。OLED の Touch ページと同じく Touch1〜4 の position（触れていなければ ---）を、depth と一緒に出す。表示範囲の最後の値（一時停止中は範囲の終わり）。その下に、PC の解析が出したイベント（表示範囲、新しい順に 10 件。行数は固定で、件数が少なくても高さは変わらない）
- `replay_algo`: PC の解析のイベントと、記録の中のファームのイベントを並べて出す

**手順 2 についての注意**: 記録 `qubit_20261027_211117.qlog` は、記録したときのパラメータ（§3.12 の 3 つの対策を切る）で通しても、ファームのイベントと一致しなかった（PC 44 件・記録 121 件。ベロシティから見て output が違う）。記録を始める前からファームが動いていて、基準値・noise_floor が出来上がっていたのに対し、PC は記録の先頭から初期取得を始めるため。手順 2 には、今のファームで**電源を入れたところから**録った記録を使う

## 8. 確かめ方

- qubit_etude の記録（`.qlog`）を `touch_algo` に通した output から、depth・position を求めて表示する（`replay_algo` か GUI に足す）。PC で式を比べてからファームに入れる（`touch_algo` と同じ進め方）

## 9. 未決事項

- §6 の各項目
- ファームの `src/touch/` や定数（`TOTAL_QT_KEYS` など）の key という名前を electrode に揃えるか（§7.2）
