# タッチセンサ信号処理（基準値の自前管理・ノイズ対策） 設計書

## 1. 背景と目的

### 1.1 現状

- `src/touch/read_touch.rs` は AT42QT1070 から次の 2 種類を読み、その差を入力値としている
    - Key Signal（レジスタ 4–15）: 生値
    - Reference Data（レジスタ 18–29）: チップ内部の基準値。12 スキャンに 1 回 `set_reference()` で全キー分を読み直す
- 入力値は `raw - reference - reference_adjust`（いずれも飽和減算）で、`TOUCH_RAW_DATA` 経由で Core0 の `QubitTouch` に渡る
- `reference_adjust` は設定画面（`WORK_MODE_DISPLAY == true`）にいる間、`raw - reference` の最大値を記録する。値は増える方向にしか更新されず、リセットする処理が無い
- Core0 側では `Pad` が 4 サンプルの移動平均をかけてから位置を検出している（`qtouch.rs` の `MAX_MOVING_AVERAGE`）
- Core1 のスキャンループは周期を持たず、`yield_now()` だけで回っている。Core0 の `qubit_touch_task` は `Timer::after(10ms)` で回るので、実際の周期は 10ms＋処理時間になる

### 1.2 問題点

**(a) チップの基準値が追従しない**

データシート（`doc/Atmel-9596-AT42-QTouch-BSW-AT42QT1070_Datasheet.pdf`）を見ると、チップの基準値は「ボタンの ON/OFF 判定」を目的に作られており、QUBIT のように差分の大きさを連続量として使う用途には合わない。

| 項目 | データシートの記述 | QUBIT への影響 |
|---|---|---|
| ドリフト速度（§2.11.1） | タッチ方向へ 1 カウント / 2.88 秒、反対方向へ 1 カウント / 0.96 秒 | 数十カウントのずれを吸収するのに数十秒〜数分かかる |
| Drift Hold Time（§2.11.6） | 1 キーでも検出中なら、同じチップの全キーでドリフトが止まる。離してから約 4 秒は止まったまま | 演奏中は 6 キーのどれかが触れられていることが多く、基準値がほぼ固定になる |
| 検出しきい値 NTHR | 既定 20 カウント。検出中のキーはドリフトしない | こちらの判定（`TOUCH_THRESHOLD = 32`、隣接比較）とずれるため、いつ止まり、いつ動くのかを外から予測できない |
| Max On Duration（§2.11.4） | `init()` で 0（無効）にしている | 検出状態のまま固まったキーが二度と再校正されない |
| Positive Recalibration（§2.11.5） | 生値が基準値より 4 カウント以上下がると即座に再校正 | 下方向の追従は速いので、この挙動は自前実装でも踏襲する |

加えて、チップ側で基準値が上がると `reference_adjust` の固定オフセットと二重に引かれ、その分だけデッドゾーンが広がる。

**(b) ノイズが多い**

これまでの経験から、生値にはかなりのノイズが乗っている。現状は Core0 側の 4 サンプル移動平均と、`qtouch.rs` の単発スパイク除去（`SINGLE_PAD_SPIKE_*`）で対処している。

**(c) 触れる速さを取る土台が無い**

今後、「どれだけ速く触れたか（少しずつ値が増えたのか、急に大きくなったのか）」を取りたい。速さの判定そのものは `qtouch.rs` 側の仕事だが、今の信号の渡し方では時間の情報が失われている。

- 1.1 のとおり、チップ（8ms）・Core1（不定）・Core0（約 10ms）がそれぞれ独立した周期で動いている。同じ値を 2 回読んだり、1 サンプル飛ばしたりする
- 1 周期ごとの値に時刻の情報が付いていないため、Core0 からは値がいつ測られたものか分からない
- OLED への転送（1024 バイト、31 バイトずつ 34 回の I2C 書き込み）は 400kHz で約 25ms かかると見積もられる。100ms 毎の転送の間、タッチのスキャンが止まる（実測はまだしていない）

### 1.3 AT42QT1070 の測定タイミング（データシートより）

- 約 8ms の固定サイクルで動作する（§2.5）。LP_MODE（レジスタ 54）が 0 または 1 なら毎サイクル（8ms 毎）に測定する。現在の `init()` は LP_MODE = 0 に設定しているので、すでに最速で動いている
- Averaging Factor（AVE、レジスタ 39–45。既定 8）を大きくすると S/N は良くなるが、測定時間が延びる。全キーの測定が 8ms を超えると、ステータス（レジスタ 2）の OVERFLOW ビットが立ち、サイクルが延びる（§2.5, §5.4）
- 16 個のチップはそれぞれ自走しており、互いに同期していない。PCA9544 は I2C の経路を切り替えるだけなので、各キーの生値はおよそ 8ms 毎に更新される

したがって、**生値の時間分解能は 8ms が上限**になる。

### 1.4 方針

1. チップからは **生値（Key Signal）だけ** を読み、基準値はファームウェアでキーごとに管理する
2. ノイズ除去は信号処理（`read_touch` 側）で行う。移動平均を信号処理と `qtouch` の二重にかけないようにする
3. スキャンは `doc/task_architecture.md` の改修で **一定周期** になり、スキャン → 信号処理 → 解析（`qtouch`）が Core1 の `touch_task` の中でひと続きに行われる。本書の信号処理は、その間に入る
4. 速さ検出の土台として、キーごとの **立ち上がり開始時刻** をスキャン毎に記録し、解析に渡す（§3.8）。速さの判定・ベロシティへの変換は別の設計書で扱う
5. **遅い方向（ゆっくり触れた）を確実に取れること** を優先する。速い方向は 8ms の分解能とノイズ除去による遅れがあるため、「ある速さ以上はすべて速い」とまとめて扱うことを許容する
6. 基準値の追従規則・速度・停止条件、フィルタの強さはすべて `const` で調整できるようにする
7. 基準値とノイズ幅（デッドゾーン）を別の量として管理し、二重に引かれないようにする

## 2. 用語

| 用語 | 意味 |
|---|---|
| raw | AT42QT1070 の Key Signal。指が近づくと値が増える |
| filtered | raw にノイズ除去（§3.3）をかけた値 |
| baseline | 未タッチ時の filtered の推定値。本設計で自前管理する |
| delta | `filtered - baseline`（符号付き）。正がタッチ方向 |
| noise_floor | 未タッチ時の delta の揺れ幅。出力から差し引くデッドゾーン |
| output | `max(0, delta - noise_floor)`。解析（`QubitTouch`）に渡す値 |
| onset | 立ち上がり開始時刻。静かな状態から delta が上がり始めた時刻（§3.8） |
| 近傍 | リング上で自キーから ±`NEIGHBOR_RANGE` 以内のキー（`TOTAL_QT_KEYS` で折り返す） |

キー番号はすべて `TOUCH_INDEX_SHIFT` で回転した後のインデックス（`shifted_index`）で扱う。こうすると、近傍がリング上の物理的な隣と一致する。

時間に関する定数は、すべて **ミリ秒で定義** する。スキャン周期を変えても意味が変わらないようにするため。

## 3. 仕様

### 3.1 信号処理の流れ

```
[Core1: touch_task]
  AT42QT1070 (8ms 周期で自走)
    │ I2C 読み取り（SCAN_PERIOD_MS 毎）
    ▼
  raw ──► hi/lo ずれ補正 ──► スパイク除去（3 点メディアン）──► 平滑化（短い移動平均）──► filtered
                                                                                              │
                                   baseline（§3.4–3.7、BASELINE_UPDATE_INTERVAL_MS 毎に更新）◄┤
                                                                                              ▼
                                                        delta = filtered - baseline ──► output
                                                                                    └──► onset（§3.8）
    │ 関数呼び出しで渡す（同じタスクの中。コアはまたがない）
    ▼
  QubitTouch（qtouch.rs）: 位置検出・追跡・ノート変換（Pad の移動平均は外す）
  速さ検出（別設計書）
    │ ノートイベント
    ▼
  MIDI_TX ──► Core0: midi_tx_task
```

### 3.2 スキャン周期

- Core1 のスキャンループを `Ticker` で **`SCAN_PERIOD_MS` 毎** に回す。サンプル間隔が一定になり、フィルタの係数や傾きの計算が時間として意味を持つ
- `SCAN_PERIOD_MS` はチップのサイクル（8ms）以上にする。8ms より速く読んでも、同じ値を重複して読むだけになるため
- 1 周のスキャン時間は、見積もりでは 1 チャンネルあたり約 0.5ms × 16 チャンネル ≒ 8ms 前後。これを **実測** し、`SCAN_PERIOD_MS` を決める（8ms か 10ms の想定）
- OLED 転送（約 25ms と見積もり）がスキャンを止める問題は、`doc/task_architecture.md` で OLED を Core0 の I2C0 に移すことで解消する。周期化もそちらの段階 5 で行う
- 周期に間に合わなかった場合（`Ticker` の遅れ）でも、フレームの時刻は実際にスキャンした時刻を記録する

### 3.3 ノイズ除去

キーごとに、次の順で処理する。

1. **hi/lo ずれ補正**: 現状の対策（前回値より 200 以上大きければ 256 を引く）を残す。overflow しないよう `saturating_sub` / `saturating_add` を使う
2. **スパイク除去**: 直近 3 サンプルのメディアン。単発のスパイクを取り除く。遅れは 1 サンプル
3. **平滑化**: 直近 `SMOOTH_SAMPLES` サンプル（初期値 2〜4）の移動平均。遅れは (N−1)/2 サンプル

速さへの影響を次の表に示す（`SCAN_PERIOD_MS` = 8ms の場合）。

| 構成 | 合計の遅れ | 立ち上がりの鈍り |
|---|---|---|
| メディアン 3 + 平均 2 | 約 12ms | 小 |
| メディアン 3 + 平均 4 | 約 20ms | 中（現状の `Pad` の平均 4 と同程度） |

- 遅い方向（数十〜数百 ms かけた立ち上がり）の計測には、どちらの構成でもほとんど影響しない
- 速い方向（1〜2 サンプルで立ち上がる）は平滑化で鈍るため、「一定以上速い」とまとめて扱う（方針 5）
- `qtouch.rs` の `Pad` の移動平均（`MAX_MOVING_AVERAGE` = 4）は二重になるため外す（1 にする）。`qtouch.rs` の単発スパイク除去（空間方向）は目的が違うので残す
- 読み取りに失敗したサンプルは、フィルタの履歴に入れない（前回の filtered をそのまま使う）

### 3.4 基準値のデータ構造（キーごと）

| フィールド | 型 | 内容 |
|---|---|---|
| `baseline_q` | `u32` | 基準値の固定小数点表現（Q8、`filtered << 8` 相当）。遅い追従が丸め誤差で消えないようにする |
| `noise_floor` | `u16` | デッドゾーン |
| `quiet_since` | `Instant` | 近傍が静かになった時刻 |
| `neg_since` | `Option<Instant>` | delta が大きく負になり始めた時刻（即時再校正用） |
| `active_since` | `Option<Instant>` | delta が `ACTIVE_THRESHOLD` を超え始めた時刻（固着検出用） |
| `active_min` / `active_max` | `i16` | 固着検出区間の delta の最小・最大 |

フィルタの履歴（キーあたり数サンプル）と onset（§3.8）を合わせても、96 キーで数 KB に収まる。`ReadTouch` は Core1 のタスク（静的に確保される future）に置かれるため、スタックへの影響は小さい。

全体の状態として、次を持つ。

- `state`: `Acquiring`（初期取得中）/ `Running`（通常）/ `Calibrating`（設定画面中）
- `last_update`: 基準値を最後に更新した時刻
- `prev_work_mode_display`: 設定画面に入った瞬間を検出するための前回値

### 3.5 初期取得（Acquiring）

1. 起動後 `ACQUIRE_MS` の間、キーごとに filtered を積算する
2. 平均値を `baseline_q` の初期値にする
3. 取得中の output は全キー 0 とする（`QubitTouch` にタッチとして見えないようにする）
4. 読み取りに失敗したサンプルは積算から除外し、成功回数で割る。1 回も成功しなかったキーは、次に読めたときの filtered をそのまま初期値にする

起動時に触れていた場合、そのキーの baseline は高めに取得される。指を離すと delta が大きく負になるので、§3.6 の下方向の速い追従で自動的に回復する。

### 3.6 基準値の追従規則（Running）

`BASELINE_UPDATE_INTERVAL_MS` 毎に、キーごとに上から順に判定して、最初に当てはまった規則を適用する。

| # | 条件 | 動作 | 意図 |
|---|---|---|---|
| R1 | 読み取り失敗中（前回値を流用している） | 更新しない | 古い値で基準値を動かさない |
| R2 | `delta < -NEG_RECAL_THRESHOLD` が `NEG_RECAL_MS` 以上続いた | `baseline = filtered`（即時再校正） | チップの Positive Recalibration 相当。起動時タッチ・急な環境変化からの復帰 |
| R3 | `delta < 0` | `baseline += (filtered - baseline) >> FALL_SHIFT` | 下方向は速めに追従 |
| R4 | 近傍が静かで、その状態が `QUIET_HOLD_MS` 以上続いている | `baseline += (filtered - baseline) >> RISE_SHIFT`。ただし 1 回の上昇は `RISE_MAX_STEP_Q` まで | 上方向（タッチ方向）はゆっくり追従。上限を付けて、手をかざしたままでも吸収速度を抑える |
| R5 | それ以外（近傍が触れられている） | 更新しない | 演奏中のタッチを基準値に取り込まない |

**「近傍が静か」の定義**

- 自キーと近傍の全キーで `delta < QUIET_THRESHOLD`
- 近傍を含めるのは、指の周辺のキーも近接で raw が上がるため。自キーだけで判定すると、指の周辺キーの基準値が少しずつ上がってしまう
- `QUIET_THRESHOLD` は `ACTIVE_THRESHOLD` より小さくし、ヒステリシスを持たせる
- 近傍が一度でも静かでなくなったら `quiet_since` をリセットする（チップの Drift Hold Time の考え方を、チップ単位ではなく近傍単位にしたもの）

**ゆっくりしたタッチを吸収しないための制約**

ゆっくり近づく指は、基準値から見ると「ゆっくり上がるドリフト」と見分けがつかない。遅い方向の速さを正しく取るため、次を満たすようにパラメータを決める。

- 想定する最も遅いタッチ（`SLOWEST_ATTACK_MS` かけて `ACTIVE_THRESHOLD` まで上がる）の間に、基準値が上がる量が `QUIET_THRESHOLD / 2` 以下であること
- R4 で上がり得る量の上限は「`QUIET_THRESHOLD` を下回っている時間 × 上昇速度の上限」。上昇速度の上限は `RISE_MAX_STEP_Q / 256` カウント ÷ `BASELINE_UPDATE_INTERVAL_MS` で決まる
- 例: `SLOWEST_ATTACK_MS` = 1000ms、`QUIET_THRESHOLD` = 8 なら、`QUIET_THRESHOLD` 未満の区間はおよそ 1000 × 8/24 ≒ 330ms。上昇速度の上限を 3 カウント/秒とすると、吸収されるのは約 1 カウントで、条件（4 以下）を満たす
- さらに `QUIET_HOLD_MS` の待ちがあるため、直前まで演奏していた場所ではこの吸収は起きない

### 3.7 固着からの回復・ノイズ幅・設定画面での校正

**固着からの回復（Running）**: 異物が載った、湿気で局所的に値が上がった、などで delta が高いまま動かないキーを救済する。チップの Max On Duration に相当する。

- `delta >= ACTIVE_THRESHOLD` になった時点で `active_since` を記録し、`active_min` / `active_max` の追跡を始める
- `delta < ACTIVE_THRESHOLD` に戻ったら追跡をやめる
- 追跡中に `active_max - active_min > STUCK_VARIATION` になったら、人の指とみなして追跡をやり直す（`active_since` を現在時刻に、min/max を現在の delta にする）。指は必ず揺れるので、長いロングトーンでも固着とは判定されない
- `now - active_since >= STUCK_TIME_MS` に達したら、そのキーを `baseline = filtered` で再校正する
- Violin モードで指を静止させたまま長く押さえると、誤って再校正される可能性がある。そのため `STUCK_TIME_MS` は長め（30 秒程度）から始め、実機で確認する。必要なら「固着回復は Piano モードのみ」とする

**ノイズ幅（noise_floor）**

- 初期値は `NOISE_FLOOR_DEFAULT`（全キー共通）
- 設定画面で計測し直す（下記）
- output は `max(0, delta - noise_floor)` とする。baseline とは独立しているので、二重に引かれることはない
- ノイズ除去（§3.3）の後の delta で測るので、フィルタを強くすれば noise_floor は小さくなる

**設定画面での校正（Calibrating）**: `SETTING_MODE`（旧 `WORK_MODE_DISPLAY`）が false → true に変わった瞬間に Calibrating に入り、true → false で Running に戻る。

- **入ったとき**
    - `noise_floor` を全キー 0 にリセットする
    - 固着検出の追跡をすべて解除する
- **Calibrating 中**（`BASELINE_UPDATE_INTERVAL_MS` 毎）
    - 近傍判定をせず、全キーで `baseline += (filtered - baseline) >> CALIB_SHIFT` と速く追従する
    - `CALIB_SETTLE_MS` が経過したら、`|delta|` の最大値を `noise_floor` として記録し始める（上がる方向のみ更新）
    - `noise_floor` の上限は `NOISE_FLOOR_MAX`。上限に張り付いたキーは、校正中に触れられていた可能性がある
- **出たとき**
    - 設定画面にいた時間が `CALIB_SETTLE_MS` より短く、`noise_floor` を記録できなかったキーは、`NOISE_FLOOR_DEFAULT` に戻す

CLAUDE.md にある「設定画面ではセンサーに触れない」という運用は、そのまま維持する。

### 3.8 立ち上がり開始時刻（onset）

速さ検出の土台として、キーごとに「静かな状態から上がり始めた時刻」を記録する。`QubitTouch` の解析は当面 10ms 毎（`task_architecture.md` §4.1 の `ANALYSIS_DIVIDER`）なので、しきい値をまたいだ瞬間は、スキャン毎に動く信号処理の側で捉えておく。

- キーが「静か」（delta < `QUIET_THRESHOLD`）から `delta >= ONSET_THRESHOLD` に上がったとき、そのスキャンの時刻を `onset` に記録する
    - `ONSET_THRESHOLD` は noise_floor より少し上（`noise_floor + ONSET_MARGIN`）にする。ノイズでの誤記録を避けつつ、なるべく早い段階で捉えるため
- delta が `QUIET_THRESHOLD` 未満に戻ったら `onset` をクリアする
- 後段は「Note On を決めた時刻 − onset」を **立ち上がり時間** として使える。遅いタッチほど長くなる
    - 遅い方向: 立ち上がりに数十〜数百 ms かかるので、8ms の分解能とフィルタの遅れ（最大 20ms 程度）に比べて十分長く、きちんと区別できる
    - 速い方向: 立ち上がり時間が 1〜3 サンプル程度になり、分解能の限界に当たる。「速い」とまとめて扱う
- 時刻は `u32` のミリ秒（Core1 起動からの経過時間）で持つ。0 を「未記録」とする

### 3.9 解析（QubitTouch）への受け渡し

信号処理と解析は、どちらも Core1 の `touch_task` の中にある（`doc/task_architecture.md` §4.1）。受け渡しはコアをまたがず、関数呼び出しで行う。

- `TouchSignal::process()` が返す `output` を、従来どおり `QubitTouch::set_value()` で渡す
- `onset_ms` は、速さ検出を実装するときに `QubitTouch` に渡す口を作る。それまでは使わない
- PC に送るデバッグ用のフレーム（raw・filtered・baseline・output・onset）は、`doc/debug_env.md` の `DEBUG_FRAMES` で Core0 の送信タスクに渡す

### 3.10 デバッグ表示

- デバッグ表示用の `POINT0`–`POINT5` はキー 0–5 の値を出す。何を出すか（raw / filtered / baseline / output）は `const` で切り替えられるようにする
- 1 周のスキャン時間（§3.2 の実測用）を `ELAPSED_TIME` などの既存の仕組みで確認できるようにする

### 3.11 調整パラメータ（初期値は仮。実機で調整する）

`read_touch.rs`（またはベースラインの新モジュール）の先頭に集める。

| 定数 | 仮の値 | 意味 |
|---|---|---|
| `SCAN_PERIOD_MS` | 8 または 10 | スキャン周期。実測してから決める（§3.2） |
| `SMOOTH_SAMPLES` | 2 | 平滑化の移動平均のサンプル数（§3.3） |
| `BASELINE_UPDATE_INTERVAL_MS` | 20 | 基準値の更新周期 |
| `ACQUIRE_MS` | 300 | 起動時に平均を取る時間 |
| `NEIGHBOR_RANGE` | 2 | 静かさの判定に使う近傍の片側キー数 |
| `QUIET_THRESHOLD` | 8 | これ未満なら「静か」 |
| `ACTIVE_THRESHOLD` | 24 | これ以上なら「触れられている」（固着検出の開始条件） |
| `ONSET_MARGIN` | 4 | noise_floor にこれを足した値を onset のしきい値にする |
| `QUIET_HOLD_MS` | 1000 | 静かになってから上方向に追従し始めるまでの待ち時間 |
| `RISE_SHIFT` | 8 | 上方向の追従率 1/256。20ms 周期で時定数は約 5 秒 |
| `RISE_MAX_STEP_Q` | 16（= 1/16 カウント） | 1 回の更新での上昇の上限（Q8）。最大でも約 3 カウント/秒 |
| `SLOWEST_ATTACK_MS` | 1000 | 正しく計測したい最も遅いタッチ。§3.6 の制約の確認に使う（コードでは使わない） |
| `FALL_SHIFT` | 3 | 下方向の追従率 1/8。時定数は約 160ms |
| `NEG_RECAL_THRESHOLD` | 8 | この値より大きく負なら即時再校正の候補 |
| `NEG_RECAL_MS` | 100 | 即時再校正に必要な継続時間 |
| `STUCK_TIME_MS` | 30000 | 固着とみなすまでの時間 |
| `STUCK_VARIATION` | 6 | この値を超えて揺れていれば人の指とみなす |
| `CALIB_SHIFT` | 2 | 校正中の追従率 1/4 |
| `CALIB_SETTLE_MS` | 500 | 校正に入ってからノイズを計り始めるまでの時間 |
| `NOISE_FLOOR_DEFAULT` | 4 | 校正前のデッドゾーン |
| `NOISE_FLOOR_MAX` | 16 | デッドゾーンの上限 |

`qtouch.rs` の `TOUCH_THRESHOLD`（現在 32）は、これまで「チップの基準値 + `reference_adjust`」を引き、Core0 で移動平均をかけた値に合わせて調整されている。新方式では output の大きさが変わるので、調整し直す。

## 4. 現状の実装からの変更点

### 4.1 変更の全体像

| ファイル | 変更内容 |
|---|---|
| `src/touch/baseline.rs`（新規） | ノイズ除去・基準値・onset の計算（`TouchSignal`）。I2C には触れない純粋な計算だけにする。PC アプリと共有するため、`crates/touch_algo`（no_std）として切り出す予定（`doc/debug_env.md` §4.5） |
| `src/touch/mod.rs` | `pub mod baseline;` を追加 |
| `src/touch/read_touch.rs` | チップの基準値を読む処理と `reference_adjust` を削除し、`TouchSignal` を呼び出して output と onset を得る |
| `src/devices/at42qt.rs` | `read_6key` の `reference` 引数を削除する |
| `src/tasks/touch.rs` | `touch_task` で、`read_touch` の結果（output）を従来どおり `QubitTouch` に渡す |
| `src/touch/qtouch.rs` | `Pad` の移動平均を外す（`MAX_MOVING_AVERAGE` = 1）。`TOUCH_THRESHOLD` を調整し直す |
| `CLAUDE.md` | `read_touch.rs` の説明、設定画面の説明を新方式に合わせて更新する |

### 4.2 `src/touch/baseline.rs`（新規）

ノイズ除去・基準値・onset の計算を I2C の読み取りから切り離し、1 スキャン分の raw 配列を渡すと output と onset が返る形にする。こうしておくと、ロジックだけを読み・直しやすくなる（ユニットテストは無いが、将来ホスト側でテストする余地も残せる）。

```rust
pub struct TouchSignal {
    keys: [KeySignal; TOTAL_QT_KEYS], // フィルタ履歴・基準値・onset など
    state: BaselineState,             // Acquiring / Running / Calibrating
    acquire_started: Instant,
    last_update: Instant,
    calib_started: Instant,
}

impl TouchSignal {
    pub const fn new() -> Self;

    /// 1 スキャン分の raw を受け取り、output と onset を書き出す。
    /// valid[i] が false のキーは読み取り失敗として扱い、フィルタ履歴にも基準値にも反映しない。
    pub fn process(
        &mut self,
        raw: &[u16; TOTAL_QT_KEYS],
        valid: &[bool; TOTAL_QT_KEYS],
        calibrating: bool,         // SETTING_MODE
        now: Instant,
        output: &mut [u16; TOTAL_QT_KEYS],
        onset_ms: &mut [u32; TOTAL_QT_KEYS],
    );

    /// デバッグ表示用
    pub fn filtered(&self, key: usize) -> u16;
    pub fn baseline(&self, key: usize) -> u16;
    pub fn noise_floor(&self, key: usize) -> u16;
}
```

`process()` の中の処理の流れは次のとおり。

1. `calibrating` の立ち上がり・立ち下がりを検出して、状態を切り替える（§3.7）
2. 読み取りに成功したキーだけ、ノイズ除去をかけて filtered を更新する（§3.3）
3. 全キーの delta を計算する（`filtered - (baseline_q >> 8)`、`i32` で計算）
4. `now - last_update >= BASELINE_UPDATE_INTERVAL_MS` なら、状態に応じて基準値を更新する
    - Acquiring: 積算する（§3.5）
    - Running: 近傍の静かさを先に全キー分判定し、その後で R1–R5 と固着回復を適用する（§3.6, §3.7）。判定と更新を分けるのは、更新済みのキーの値で近傍判定がぶれないようにするため
    - Calibrating: 速い追従とノイズの計測（§3.7）
5. output と onset を計算する（§3.7, §3.8）

### 4.3 `src/touch/read_touch.rs`

**削除するもの**

- フィールド `reference`, `reference_adjust`, `reference_counter`
- `set_reference()` と、12 スキャンに 1 回それを呼ぶ処理
    - これで 12 スキャンに 1 回、全 16 チャンネル分の読み取り（PCA9544 の切り替えを含む）が無くなり、スキャン時間のばらつきも減る
- スキャンループ内の `reference_adjust` の更新と、差し引き

**追加・変更するもの**

- フィールド `signal: TouchSignal`、`valid: [bool; TOTAL_QT_KEYS]`
- `touch_sensor_scan()`
    1. 従来どおり全チャンネルを読み、`raw_value` を更新する。読み取りに成功したキーは `valid = true`、失敗したキーは `valid = false` にする
    2. hi/lo ずれ補正を `saturating_sub(256)` / `saturating_add(200)` に変える（dev ビルドでの overflow panic を防ぐ）
    3. 全チャンネルを読み終えたら、`signal.process(...)` を呼んで output と onset を得て、呼び出し元（`touch_task`）に返す
    4. `POINT0`–`POINT5` を更新する（§3.10）

**初期化**

- `init_touch_sensors()` は変更しない。チップの LP_MODE / MAX_DUR の設定はチップ内部の判定用で、raw には影響しないため、現状のままで問題ない
- AVE（レジスタ 39–45）を上げてチップ側で S/N を稼ぐ案は、§7 の未決事項とする

### 4.4 `touch_task`

- スキャンの周期化と、スキャン → 解析をひと続きにする構成は、`doc/task_architecture.md` で導入済みとする
- `touch_task` は `read_touch` から output を受け取り、従来どおり `QubitTouch` に渡す
- 速さ検出を入れるまでは、onset は使わない
- 毎フレームの解析（フレーム駆動）と、`QubitTouch` の時間基準化は、速さ検出の設計で扱う

### 4.5 `src/devices/`

- `at42qt.rs`: `read_6key(i2c, result, reference: bool)` から `reference` 引数を削除し、Key Signal（レジスタ 4）だけを読む関数にする。チップの基準値を比較用に見たくなる可能性があるので、`read_6key_reference()` を `#[allow(dead_code)]` 付きで残すかどうかは実装時に決める

### 4.6 `src/touch/qtouch.rs`

- `Pad::MAX_MOVING_AVERAGE` を 1 にする（ノイズ除去は信号処理に移るため）。構造ごと外すかどうかは、実機で確認してから決める
- `TOUCH_THRESHOLD` を調整し直す
- 位置検出・追跡・ノート変換のロジックは変えない

### 4.7 変更しないもの

- `output` の意味（未タッチで 0 付近、タッチで正の値）
- `QubitTouch` の位置検出ロジック
- エラーコード（新しい失敗経路は増えないので、追加しない）
- タスク構成（`task_architecture.md` の構成のまま）

## 5. 実装の手順

作業は `doc/task_architecture.md`（コア・タスク構成の改修）→ `doc/debug_env.md`（デバッグ環境）→ 本書、の順で行う。

本設計の方針は一度には決めず、実際に触りながら決めていく。そのため、まず `doc/debug_env.md` のデバッグ環境（USB CDC で生値を PC に送り、PC アプリで表示・記録・アルゴリズムを試す）を作る。実験は PCA9544 無しの 6 キー構成（`no_pca9544`）で行う。以下の手順は、PC 上で方針が固まった後にファームへ移すときの目安とする。§3 の仮の値や規則は、PC での実験結果で置き換えていく。

各ステップの終わりに `cargo build`、`--features test_mode`、`--features no_pca9544`、clippy がすべて通ることを確認し、ステップごとにコミットする。どの段階で挙動が変わったかを追えるようにするため。

スキャン時間の実測・周期化・OLED の分離は、`task_architecture.md` で済んでいる。

1. **基準値の自前管理**: `baseline.rs` を作り、Acquiring と Running の R1–R5 だけを実装する。`read_touch.rs` を差し替え、`at42qt.rs` の引数を整理する。この段階ではノイズ除去は無し（filtered = raw）、`Pad` の移動平均も残す
2. **実機確認と調整**: §6 の 6.1–6.3 を確認し、`TOUCH_THRESHOLD` と追従パラメータを調整する
3. **ノイズ除去の移設**: 信号処理にメディアンと移動平均を入れ、`Pad` の移動平均を外す。6.9 を確認する
4. **Calibrating**: 設定画面での校正（§3.7）を追加し、6.4 を確認する
5. **固着回復**: §3.7 を追加し、6.5, 6.6 を確認する
6. **onset**: onset の記録を追加する。6.10 を確認する
7. **ドキュメント**: `CLAUDE.md` を更新する

## 6. 実機での確認項目

| # | シナリオ | 期待する結果 |
|---|---|---|
| 6.1 | 何も触れずに起動し、数分放置 | output が全キー 0 付近のまま。ゴーストノートが出ない |
| 6.2 | 通常の演奏（スライド・複数点・素早い連打） | 反応が従来と同等以上。演奏中に感度が落ちていかない |
| 6.3 | 起動時にパッドに触れておき、その後離す | 離してから約 100ms 以内にそのキーの output が 0 に戻る |
| 6.4 | 設定画面に入って数秒待ち、出る | noise_floor が記録され、無操作時のちらつきが減る |
| 6.5 | パッドに導電性の物を載せたまま放置 | `STUCK_TIME_MS` 後に再校正され、ノートが止まる |
| 6.6 | Violin モードで 30 秒以上ロングトーン | 途中で音が切れない（固着と誤判定されない） |
| 6.7 | 手のひらでパッドを暖める / 息を吹きかける（温度・湿度の変化） | 数秒〜十数秒で基準値が追従し、感度が戻る |
| 6.8 | 手をリングの上にかざしたまま待つ | 近傍が静かでないので基準値は上がらず、手を下ろせばすぐ反応する |
| 6.9 | 無操作時と演奏時のノイズ（OLED の POINT 表示で確認） | 無操作時の output の揺れが従来より小さい。反応の遅れは体感で増えない |
| 6.10 | 約 1 秒かけてゆっくり指を近づける / 素早く叩く | ゆっくりのときは onset から発音までが数百 ms、素早いときは数十 ms 以下と、はっきり区別できる。ゆっくりのときに基準値が吸収して発音しない、ということが起きない |

## 7. 未決事項

- **1 周のスキャン時間**: 実測してから `SCAN_PERIOD_MS` を 8ms にするか 10ms にするかを決める（§3.2）。10ms にすると、チップの 8ms 周期とずれて、ときどきサンプルを 1 つ飛ばす
- **平滑化の強さ**: `SMOOTH_SAMPLES` を 2 にするか 4 にするか。ノイズの実測を見て決める
- **AVE の変更**: チップの AVE を 8 → 16 / 32 に上げて S/N を稼ぐか。その場合、OVERFLOW ビットを確認して 8ms に収まるかを見る
- **近傍単位の追従による偏り**: ほぼ常にどこかが触れられている演奏では、触れられていない範囲だけが追従する。リング全体が一様に温度変化する場合、演奏中の指の周辺だけ基準値が遅れる。これを問題とみなすかどうかは実機で判断する
- **固着回復の適用範囲**: Violin モードでも有効にするか（§3.7）
- **noise_floor の保存**: 電源を切っても残すか（フラッシュへの保存）。今回の範囲には含めない
- **速さ検出の設計**: onset を使った立ち上がり時間の計算、ベロシティへの変換、Note On の遅延との兼ね合いは、別の設計書で扱う
