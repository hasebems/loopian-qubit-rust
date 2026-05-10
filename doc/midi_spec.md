SWAM の MIDI Spec. について

マニュアルを確認しました。結論から言うと、SWAM Solo Strings の Violin は普通のGM音源のように ノートだけ送れば鳴る 方式ではなく、Expression を一緒に動かす前提の入力設計です。参照したのは loopian_qubit/doc/SWAM Solo Strings v3.12.0 - User Manual.pdf です。

Violin を鳴らすための最小セットはこれです。
1. Note On / Note Off
2. Expression
3. できれば Vibrato Depth
4. 必要に応じて Sustain か Key Switch

重要なのは Expression です。マニュアルでは、インスタンス生成後に Expression が動かされていないと実質的に音が出ないと明記されています。デフォルト割り当ては CC11 です。Expression pedal が無い場合の推奨として、Expression を CC1、Vibrato Depth を Aftertouch に割り当てる案も書かれています。

実装目線で重要な MIDI 入力仕様は次です。
1. MIDI Profile は MPE と Legacy の両対応です。通常のファーム実装ならまず Legacy 前提で十分です。
2. MIDI Channel はデフォルトで ANY にできます。Key Switch 用チャンネルは別に分けられます。
3. パラメータ割り当ては CC、CC-HIRES、Aftertouch、NRPN に対応しています。
4. Note On Velocity は主にアタック感や Bow Pressure 側に効きます。単純な音量ではありません。
5. Violin の通常音域は G2 から F6 です。設定で Middle C 表記が C4 系なら G3 から F7 表記になります。

演奏上の挙動もかなり MIDI に依存します。
1. ボウイング系の主表現は Expression です。弓速に相当します。
2. Legato はノートをオーバーラップさせて作ります。
3. Portamento は重ねた次ノートの低い Velocity、または Portamento Time を割り当てた CC で制御できます。
4. Sustain pedal は一部のボウ挙動や特殊操作で使われます。
5. Key Switch でも多くの奏法切替ができます。

Violin 用に把握しておくとよい Key Switch はこれです。
1. C: Play Mode 切替
低速 = Bow
中速 = Pizzicato
高速 = Col Legno
2. C#: Manual Bowing
3. D: Gesture Mode
低速 = Expression
中速 = Bipolar
高速 = Bowing
4. D#: Alternate Fingering
5. E: Bow Lift
6. F: Bow Start
7. G: Keep Bow Direction
8. G#: Tremolo
9. A: Tremolo Mode
10. A#: Sordino
11. B: 2nd page 切替

ファームウェアから最初に試すなら、この送信設計が一番現実的です。
1. Note On / Off を送る
2. CC11 を常時送る
3. 必要なら CC1 か Aftertouch で Vibrato Depth を送る
4. Legato 用にノートの重なりを制御する
5. あとで Key Switch を追加する

最小の MIDI 例はこうです。チャンネル 1 の例です。
1. CC11 = 20 から開始
2. Note On G4 velocity 64
3. 発音中に CC11 を 20 → 55 → 80 と上げる
4. 必要なら Aftertouch を少し上げて vibrato を加える
5. Note Off

バイト列で書くと例えばこうです。
```text
B0 0B 14    ; CC11 Expression = 20
90 43 40    ; Note On G4 vel 64
B0 0B 37    ; CC11 = 55
B0 0B 50    ; CC11 = 80
D0 20       ; Channel Aftertouch = 32
80 43 40    ; Note Off G4
```

実装の優先順位としては、まずこれが妥当です。
1. 必須: Note On/Off + CC11
2. 次点: overlap による legato と low velocity による portamento
3. 拡張: Aftertouch または別CCで Vibrato Depth
4. その後: Key Switch 群

必要なら次に進めます。
1. loopian_qubit 側で SWAM Violin 向けの MIDI 出力仕様を設計する
2. Rust で送るべき CC と Key Switch を定数表にする
3. 最小演奏フローをコードに落とす



=======================
演奏中に音程がAからBに変わる際、Aon-Bon-Aoff-Boff の順番だとレガートになり、Aon-Aoff-Bon-Boffの順番だと、別の印象になる、といった記述はありますか？
=======================

レガート判定の条件だけをマニュアル本文から絞って確認します。ノートの重なり順序と、staccato/legato の解釈に関する記述を拾います。

あります。かなり近い記述があります。参照元は loopian_qubit/doc/SWAM Solo Strings v3.12.0 - User Manual.pdf です。

要点だけ先に言うと、SWAM Violin は ノートが重なるかどうか と ノート間の離れ方 で解釈が変わります。

1. Aon-Bon-Aoff-Boff
このように次の音 B が、前の音 A がまだ鳴っている間に入る形は、マニュアルでは overlap として扱われます。これが legato 系の基本条件です。
同マニュアルには、
「A pure slurred legato is performed by overlapping the second note to the first」
「A glissando between two notes is obtained by overlapping the second note to the first」
とあり、重ねた上で B の Note On velocity が高いと legato 寄り、低いと portamento 寄りになります。

2. Aon-Aoff-Bon-Boff
こちらは音が重なっていないので、legato の条件から外れます。
さらにマニュアルには、
「Staccato Interval Time: minimum time separation between a note-off event and a consequent note-on event to be interpreted as staccato articulation」
とあり、Aoff と Bon の間の時間差で staccato 判定が行われます。

3. ただし完全に二値ではない
ここが大事です。Bow Direction Behaviour の説明では、
「Very short separations between notes are interpreted as legato connections」
とあります。
つまり、Aoff-Bon の順でも、間隔がごく短ければ legato connection として解釈される場合があります。逆に、明確な間があれば bow はリセットされ、別発音らしい印象になります。

整理すると、実装上はこう考えるのがよいです。

1. 明確に legato へしたいなら、A と B を重ねる
Aon-Bon-Aoff-Boff

2. 明確に別発音へしたいなら、Aoff の後に少し間を空けてから Bon
Aon-Aoff-少し間-Bon-Boff

3. Aon-Aoff-Bon-Boff でも、間隔が極小だと legato connection 扱いになりうる
なので、単に On/Off 順だけでなく、時間差も重要です

4. overlap した場合の B の velocity で、legato と portamento の質感が変わる
高めなら legato、低めなら portamento に寄る

少し補足すると、マニュアルには Sustain pedal を押しながら、
1. ノートを分離すると détaché
2. ノートを重ねると slurred legato
とも書かれています。ですが、これは奏法説明としての記述で、ノート連結そのものの基本判定は overlap と separation が中心です。

実装指針としては、SWAM 用には少なくとも次の3状態に分けるのがよいです。

1. Overlap
次音開始が前音終了より先
意図: legato / portamento

2. Near-gap
次音開始が前音終了の直後
意図: 軽い接続、場合によっては legato connection

3. Clear-gap
次音開始が前音終了より十分後
意図: 別発音、staccato 系

必要なら次に、loopian_qubit 向けに
1. overlap を何 ms 以上にするか
2. near-gap を何 ms までにするか
3. B の velocity をどう使い分けるか
を、loopian_qubit/doc/midi_spec.md にそのまま書ける形で提案します。



======================
マニュアル上、legato と portamento はどちらも 次音を前音に overlap させる ところから始まります。そのうえで違いを作る主因は次です。

次音の Note On velocity
高い値だと slurred legato 寄り、低い値だと portamento 寄りです。

Portamento Max Time
マニュアルに、portamento と legato の threshold は Portamento Max Time に影響されるとあります。つまり境界は固定ではありません。

Portamento Control
Velocity 制御か CC 制御かで、portamento 量をより明示的に変えられます。CC 制御なら、かなり連続的に滑り量を作れます。

Dynamic Transitions
legato transition の quality に影響するとあり、接続の質感をさらに滑らかに変えます。

なので、実装感覚としては 二値切替 ではなく、連続的なパラメータ空間の中で、結果として legato っぽく聞こえる領域と portamento っぽく聞こえる領域がある、と考えるのが合っています。

イメージとしてはこうです。

overlap あり

次音 velocity が高め

portamento time が短め
結果: legato に聞こえやすい

overlap あり

次音 velocity が低め

portamento time が長め
結果: portamento に聞こえやすい

つまり、変化はおおむね連続的です。ただし内部的には threshold という言葉も使われているので、ある条件付近で知覚上の切り替わりが少しはっきり出る可能性はあります。完全な物理連続関数というより、しきい値を含む連続遷移です。

loopian_qubit で使うなら、設計はこうすると扱いやすいです。

overlap を前提条件にする
接続タイプを 0.0 から 1.0 の連続値で持つ
その値から
velocity
portamento CC
expression 変化速度
を連動させる



