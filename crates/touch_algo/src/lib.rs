//! Loopian::QUBIT のタッチ信号処理（doc/touch_baseline.md §3）
//!
//! 1 スキャン分の生値を `TouchSignal::process` に渡すと、キー毎に
//! filtered（ノイズ除去後）・baseline（基準値）・delta・output・onset を更新する。
//! I2C や時計には触れない純粋な計算だけで、ファーム（no_std）と PC アプリ（tools/qubit_monitor）で共有する。
//!
//! - 時刻は u32 の µs（ファームの `embassy_time::Instant` の µs、PC ではフレームの `time_us`）。
//!   約 71 分で一周するので、時間の差は `wrapping_sub` で求める
//! - キー毎の状態は、呼び出し側が用意した `[KeyState]`（ファームは配列、PC は Vec）に持つ
#![no_std]

/// 調整パラメータ（doc/touch_baseline.md §3.11）。既定値は設計書の仮の値
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// hi/lo ずれとみなす増加量（前回値よりこれ以上大きければ 256 を引く）
    pub hi_lo_jump: u16,
    /// 平滑化の移動平均のサンプル数（1..=MAX_SMOOTH_SAMPLES）
    pub smooth_samples: u8,
    /// 基準値の更新周期
    pub baseline_update_interval_ms: u32,
    /// 起動時に平均を取る時間
    pub acquire_ms: u32,
    /// 静かさの判定に使う近傍の片側キー数
    pub neighbor_range: u8,
    /// delta がこれ未満なら「静か」
    pub quiet_threshold: u16,
    /// delta がこれ以上なら「触れられている」（固着検出の開始条件）
    pub active_threshold: u16,
    /// noise_floor にこれを足した値を onset のしきい値にする
    pub onset_margin: u16,
    /// 静かになってから上方向に追従し始めるまでの待ち時間
    pub quiet_hold_ms: u32,
    /// 上方向の追従率 1/2^n
    pub rise_shift: u8,
    /// 1 回の更新での上昇の上限（Q8。16 = 1/16 カウント）
    pub rise_max_step_q: u32,
    /// 下方向の追従率 1/2^n
    pub fall_shift: u8,
    /// この値より大きく負なら即時再校正の候補
    pub neg_recal_threshold: u16,
    /// 即時再校正に必要な継続時間
    pub neg_recal_ms: u32,
    /// 固着とみなすまでの時間
    pub stuck_time_ms: u32,
    /// この値を超えて揺れていれば人の指とみなす
    pub stuck_variation: u16,
    /// 校正中の追従率 1/2^n
    pub calib_shift: u8,
    /// 校正に入ってからノイズを計り始めるまでの時間
    pub calib_settle_ms: u32,
    /// 校正前のデッドゾーン
    pub noise_floor_default: u16,
    /// デッドゾーンの上限
    pub noise_floor_max: u16,
}

impl Params {
    pub const DEFAULT: Self = Self {
        hi_lo_jump: 200,
        smooth_samples: 2,
        baseline_update_interval_ms: 20,
        acquire_ms: 300,
        neighbor_range: 2,
        quiet_threshold: 8,
        active_threshold: 24,
        onset_margin: 4,
        quiet_hold_ms: 1000,
        rise_shift: 8,
        rise_max_step_q: 16,
        fall_shift: 3,
        neg_recal_threshold: 8,
        neg_recal_ms: 100,
        stuck_time_ms: 30_000,
        stuck_variation: 6,
        calib_shift: 2,
        calib_settle_ms: 500,
        noise_floor_default: 4,
        noise_floor_max: 16,
    };
}

impl Default for Params {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// 平滑化の移動平均のサンプル数の上限
pub const MAX_SMOOTH_SAMPLES: usize = 8;

/// 全体の状態（doc/touch_baseline.md §3.4）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// 起動直後、基準値の初期値を取得中。output は全キー 0
    Acquiring,
    /// 通常
    Running,
    /// 設定画面での校正中
    Calibrating,
}

/// キー毎の状態と、最後のスキャンの結果
#[derive(Clone, Copy, Debug)]
pub struct KeyState {
    // --- ノイズ除去（§3.3）
    last_corrected: u16, // hi/lo ずれ補正をかけた前回の値（0: まだ無い）
    median: [u16; 3],
    median_len: u8,
    smooth: [u16; MAX_SMOOTH_SAMPLES],
    smooth_len: u8,
    smooth_pos: u8,
    filtered: u16,
    has_filtered: bool,
    valid: bool, // 最後のスキャンで読めたか

    // --- 基準値（§3.4）
    baseline_q: u32, // Q8
    has_baseline: bool,
    acquire_sum: u32,
    acquire_count: u32,
    noise_floor: u16,
    noise_measured: bool, // 校正で noise_floor を計り始めたか
    quiet_since: Option<u32>,
    neg_since: Option<u32>,
    active_since: Option<u32>,
    active_min: i32,
    active_max: i32,

    // --- 結果
    delta: i32,
    output: u16,
    onset_us: Option<u32>,
    onset_armed: bool, // 静かな状態を通ってきた（次に上がったら onset を記録する）
}

impl KeyState {
    pub const INITIAL: Self = Self {
        last_corrected: 0,
        median: [0; 3],
        median_len: 0,
        smooth: [0; MAX_SMOOTH_SAMPLES],
        smooth_len: 0,
        smooth_pos: 0,
        filtered: 0,
        has_filtered: false,
        valid: false,
        baseline_q: 0,
        has_baseline: false,
        acquire_sum: 0,
        acquire_count: 0,
        noise_floor: 0,
        noise_measured: false,
        quiet_since: None,
        neg_since: None,
        active_since: None,
        active_min: 0,
        active_max: 0,
        delta: 0,
        output: 0,
        onset_us: None,
        onset_armed: false,
    };

    /// ノイズ除去後の値
    pub fn filtered(&self) -> u16 {
        self.filtered
    }

    /// 基準値（Q8 を四捨五入した値）
    pub fn baseline(&self) -> u16 {
        (self.baseline_q.saturating_add(128) >> 8).min(u16::MAX as u32) as u16
    }

    /// filtered − baseline（基準値がまだ無いときは 0）
    pub fn delta(&self) -> i32 {
        self.delta
    }

    /// max(0, delta − noise_floor)。初期取得中は 0
    pub fn output(&self) -> u16 {
        self.output
    }

    pub fn noise_floor(&self) -> u16 {
        self.noise_floor
    }

    /// 静かな状態から上がり始めた時刻（µs）。静かに戻ると None
    pub fn onset_us(&self) -> Option<u32> {
        self.onset_us
    }

    /// 最後のスキャンで読み取りに成功したか
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    fn baseline_value(&self) -> i32 {
        self.baseline() as i32
    }

    fn set_baseline(&mut self, value: u16) {
        self.baseline_q = (value as u32) << 8;
        self.has_baseline = true;
    }

    /// baseline += (filtered − baseline) >> shift（Q8 で計算する）
    fn track(&mut self, shift: u8, max_rise_q: Option<u32>) {
        let target = (self.filtered as i64) << 8;
        let mut step = (target - self.baseline_q as i64) >> shift.min(31);
        if let Some(max) = max_rise_q {
            step = step.min(max as i64);
        }
        self.baseline_q = (self.baseline_q as i64 + step).clamp(0, (u16::MAX as i64) << 8) as u32;
    }

    /// ノイズ除去（§3.3）。読めたサンプルだけ履歴に入れる
    fn filter(&mut self, raw: u16, params: &Params) {
        // 1. hi/lo ずれ補正（ファームの read_touch と同じ条件）
        let mut value = raw;
        if self.last_corrected != 0 && raw > self.last_corrected.saturating_add(params.hi_lo_jump) {
            value = raw.saturating_sub(256);
        }
        self.last_corrected = value;

        // 2. スパイク除去: 直近 3 サンプルのメディアン（そろうまでは最新の値）
        self.median = [self.median[1], self.median[2], value];
        self.median_len = (self.median_len + 1).min(3);
        let median = if self.median_len < 3 {
            value
        } else {
            let [a, b, c] = self.median;
            a.max(b).min(a.min(b).max(c))
        };

        // 3. 平滑化: 直近 smooth_samples サンプルの移動平均
        let n = (params.smooth_samples as usize).clamp(1, MAX_SMOOTH_SAMPLES);
        if self.smooth_len as usize > n {
            // サンプル数を減らしたとき
            self.smooth_len = 0;
            self.smooth_pos = 0;
        }
        self.smooth[self.smooth_pos as usize % n] = median;
        self.smooth_pos = ((self.smooth_pos as usize + 1) % n) as u8;
        self.smooth_len = (self.smooth_len + 1).min(n as u8);
        let len = self.smooth_len as usize;
        let sum: u32 = self.smooth[..len].iter().map(|&v| v as u32).sum();
        self.filtered = ((sum + len as u32 / 2) / len as u32) as u16;
        self.has_filtered = true;
    }
}

impl Default for KeyState {
    fn default() -> Self {
        Self::INITIAL
    }
}

/// ノイズ除去・基準値・onset の計算。K はキー毎の状態の置き場所（`[KeyState; N]` や `Vec<KeyState>`）
pub struct TouchSignal<K> {
    keys: K,
    pub params: Params,
    state: State,
    started: bool,
    state_since: u32, // 今の状態に入った時刻
    last_update: u32, // 基準値を最後に更新した時刻
    prev_calibrating: bool,
}

impl<K: AsRef<[KeyState]> + AsMut<[KeyState]>> TouchSignal<K> {
    /// keys は全キー分の `KeyState::INITIAL` で埋めて渡す
    pub fn new(keys: K, params: Params) -> Self {
        Self {
            keys,
            params,
            state: State::Acquiring,
            started: false,
            state_since: 0,
            last_update: 0,
            prev_calibrating: false,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn keys(&self) -> &[KeyState] {
        self.keys.as_ref()
    }

    /// 1 スキャン分を処理する
    /// - `now_us`: スキャンの時刻（µs）
    /// - `raw`: キー毎の生値（hi/lo ずれ補正の前）
    /// - `valid`: 読み取り成功フラグ（1 bit / キー）。失敗したキーは前回の filtered を使う
    /// - `calibrating`: 設定画面にいるか（false → true で校正に入り、true → false で戻る）
    pub fn process(&mut self, now_us: u32, raw: &[u16], valid: u128, calibrating: bool) {
        let params = self.params;
        let elapsed_ms = |since: u32| now_us.wrapping_sub(since) / 1000;
        if !self.started {
            self.started = true;
            self.state = State::Acquiring;
            self.state_since = now_us;
            self.last_update = now_us;
        }

        // ノイズ除去
        for (k, key) in self.keys.as_mut().iter_mut().enumerate() {
            key.valid = k < 128 && valid & (1u128 << k) != 0 && k < raw.len();
            if key.valid {
                key.filter(raw[k], &params);
            }
        }

        match self.state {
            State::Acquiring => {
                // 初期取得（§3.5）: filtered を積算し、ACQUIRE_MS 後に平均を基準値にする
                for key in self.keys.as_mut().iter_mut().filter(|k| k.valid) {
                    key.acquire_sum += key.filtered as u32;
                    key.acquire_count += 1;
                }
                if elapsed_ms(self.state_since) >= params.acquire_ms {
                    for key in self.keys.as_mut().iter_mut() {
                        if key.acquire_count > 0 {
                            key.set_baseline((key.acquire_sum / key.acquire_count) as u16);
                        }
                        key.noise_floor = params.noise_floor_default;
                        key.quiet_since = Some(now_us);
                    }
                    self.enter(State::Running, now_us);
                }
            }
            State::Running | State::Calibrating => {
                // 1 回も読めなかったキーは、次に読めたときの filtered を初期値にする
                for key in self.keys.as_mut().iter_mut() {
                    if key.valid && !key.has_baseline {
                        key.set_baseline(key.filtered);
                    }
                }
                self.update_delta();

                // 設定画面の出入り（§3.7）
                if calibrating && !self.prev_calibrating {
                    for key in self.keys.as_mut().iter_mut() {
                        key.noise_floor = 0;
                        key.noise_measured = false;
                        key.active_since = None;
                        key.neg_since = None;
                    }
                    self.enter(State::Calibrating, now_us);
                } else if !calibrating && self.prev_calibrating {
                    for key in self.keys.as_mut().iter_mut() {
                        if !key.noise_measured {
                            key.noise_floor = params.noise_floor_default;
                        }
                        key.quiet_since = None;
                    }
                    self.enter(State::Running, now_us);
                }
                self.prev_calibrating = calibrating;

                if self.state == State::Running {
                    self.track_running_conditions(now_us);
                }
                if elapsed_ms(self.last_update) >= params.baseline_update_interval_ms {
                    self.last_update = now_us;
                    match self.state {
                        State::Running => self.update_baseline_running(now_us),
                        State::Calibrating => self.update_baseline_calibrating(now_us),
                        State::Acquiring => {}
                    }
                }
                self.update_delta();
            }
        }
        self.update_output_and_onset(now_us);
    }

    fn enter(&mut self, state: State, now_us: u32) {
        self.state = state;
        self.state_since = now_us;
        self.last_update = now_us;
    }

    fn update_delta(&mut self) {
        for key in self.keys.as_mut().iter_mut() {
            key.delta = if key.has_baseline && key.has_filtered {
                key.filtered as i32 - key.baseline_value()
            } else {
                0
            };
        }
    }

    /// 毎スキャン追う条件: 近傍の静かさ、大きく負の継続、固着（§3.6, §3.7）
    fn track_running_conditions(&mut self, now_us: u32) {
        let params = self.params;
        let keys = self.keys.as_mut();
        let n = keys.len();
        let range = (params.neighbor_range as usize).min(n / 2);
        let quiet_threshold = params.quiet_threshold as i32;
        for k in 0..n {
            // 近傍（リングとして折り返す）が全部静かか
            let quiet = (0..=2 * range).all(|i| {
                let j = (k + n + i - range) % n;
                keys[j].delta < quiet_threshold
            });
            let key = &mut keys[k];
            if quiet {
                key.quiet_since.get_or_insert(now_us);
            } else {
                key.quiet_since = None;
            }

            if key.delta < -(params.neg_recal_threshold as i32) {
                key.neg_since.get_or_insert(now_us);
            } else {
                key.neg_since = None;
            }

            // 固着の追跡（§3.7）。指は必ず揺れるので、揺れが大きければ追跡をやり直す
            if key.delta >= params.active_threshold as i32 {
                match key.active_since {
                    None => {
                        key.active_since = Some(now_us);
                        key.active_min = key.delta;
                        key.active_max = key.delta;
                    }
                    Some(_) => {
                        key.active_min = key.active_min.min(key.delta);
                        key.active_max = key.active_max.max(key.delta);
                        if key.active_max - key.active_min > params.stuck_variation as i32 {
                            key.active_since = Some(now_us);
                            key.active_min = key.delta;
                            key.active_max = key.delta;
                        }
                    }
                }
                if let Some(since) = key.active_since
                    && now_us.wrapping_sub(since) / 1000 >= params.stuck_time_ms
                    && key.valid
                {
                    key.set_baseline(key.filtered);
                    key.active_since = None;
                }
            } else {
                key.active_since = None;
            }
        }
    }

    /// 追従規則 R1〜R5（§3.6）。上から順に判定し、最初に当てはまったものを適用する
    fn update_baseline_running(&mut self, now_us: u32) {
        let params = self.params;
        for key in self.keys.as_mut().iter_mut() {
            if !key.valid || !key.has_baseline {
                continue; // R1
            }
            if let Some(since) = key.neg_since
                && now_us.wrapping_sub(since) / 1000 >= params.neg_recal_ms
            {
                key.set_baseline(key.filtered); // R2: 即時再校正
                key.neg_since = None;
            } else if key.delta < 0 {
                key.track(params.fall_shift, None); // R3: 下方向は速めに追従
            } else if let Some(since) = key.quiet_since
                && now_us.wrapping_sub(since) / 1000 >= params.quiet_hold_ms
            {
                key.track(params.rise_shift, Some(params.rise_max_step_q)); // R4: 上方向はゆっくり
            }
            // R5: それ以外は更新しない
        }
    }

    /// 校正中（§3.7）: 近傍判定をせずに速く追従し、落ち着いたら |delta| の最大を noise_floor にする
    fn update_baseline_calibrating(&mut self, now_us: u32) {
        let params = self.params;
        let settled = now_us.wrapping_sub(self.state_since) / 1000 >= params.calib_settle_ms;
        for key in self.keys.as_mut().iter_mut() {
            if !key.valid || !key.has_baseline {
                continue;
            }
            if settled {
                let noise = key.delta.unsigned_abs().min(params.noise_floor_max as u32) as u16;
                key.noise_floor = key.noise_floor.max(noise);
                key.noise_measured = true;
            }
            key.track(params.calib_shift, None);
        }
    }

    /// output と onset（§3.7, §3.8）
    fn update_output_and_onset(&mut self, now_us: u32) {
        let params = self.params;
        let acquiring = self.state == State::Acquiring;
        for key in self.keys.as_mut().iter_mut() {
            key.output = if acquiring || !key.has_baseline {
                0
            } else {
                (key.delta - key.noise_floor as i32).clamp(0, u16::MAX as i32) as u16
            };
            if acquiring {
                continue;
            }
            let onset_threshold = key.noise_floor as i32 + params.onset_margin as i32;
            if key.delta < params.quiet_threshold as i32 {
                key.onset_us = None;
                key.onset_armed = true;
            } else if key.onset_armed && key.delta >= onset_threshold {
                key.onset_us = Some(now_us);
                key.onset_armed = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = 6;
    const ALL: u128 = (1 << N) - 1;
    const STEP_US: u32 = 8000;

    struct Sim {
        signal: TouchSignal<[KeyState; N]>,
        now: u32,
    }

    impl Sim {
        fn new() -> Self {
            Self {
                signal: TouchSignal::new([KeyState::INITIAL; N], Params::DEFAULT),
                now: 0,
            }
        }

        /// raw を ms の間、8ms 毎に流す
        fn run(&mut self, raw: [u16; N], ms: u32, calibrating: bool) {
            for _ in 0..(ms * 1000 / STEP_US) {
                self.signal.process(self.now, &raw, ALL, calibrating);
                self.now = self.now.wrapping_add(STEP_US);
            }
        }

        fn key(&self, k: usize) -> &KeyState {
            &self.signal.keys()[k]
        }
    }

    #[test]
    fn acquires_baseline_then_outputs_touch() {
        let mut sim = Sim::new();
        sim.run([900; N], 200, false);
        assert_eq!(sim.signal.state(), State::Acquiring);
        assert_eq!(sim.key(0).output(), 0);
        sim.run([900; N], 200, false);
        assert_eq!(sim.signal.state(), State::Running);
        assert_eq!(sim.key(0).baseline(), 900);

        // key2 に触れる
        let mut raw = [900; N];
        raw[2] = 960;
        sim.run(raw, 100, false);
        assert_eq!(sim.key(2).delta(), 60);
        assert_eq!(
            sim.key(2).output(),
            60 - Params::DEFAULT.noise_floor_default
        );
        assert!(sim.key(2).onset_us().is_some());
        assert_eq!(sim.key(0).output(), 0);
    }

    #[test]
    fn median_removes_single_spike() {
        let mut sim = Sim::new();
        sim.run([900; N], 400, false);
        let mut raw = [900; N];
        raw[1] = 990; // 単発のスパイク（hi/lo ずれの条件には当たらない）
        sim.signal.process(sim.now, &raw, ALL, false);
        sim.now += STEP_US;
        sim.run([900; N], 8, false);
        assert_eq!(sim.key(1).filtered(), 900);
    }

    #[test]
    fn hi_lo_jump_is_corrected() {
        let mut sim = Sim::new();
        sim.run([900; N], 400, false);
        let mut raw = [900; N];
        raw[0] = 900 + 256; // hi/lo ずれ
        sim.run(raw, 40, false);
        assert_eq!(sim.key(0).filtered(), 900);
    }

    #[test]
    fn slow_drift_is_followed_only_when_quiet() {
        let mut sim = Sim::new();
        sim.run([900; N], 400, false);
        // 全キーが静かなまま、ゆっくり 6 カウント上がる → 基準値が追従する
        sim.run([906; N], 10_000, false);
        assert!(sim.key(0).baseline() > 903, "{}", sim.key(0).baseline());

        // key3 に触れている間は、その近傍（1〜5）は追従しない
        let mut sim = Sim::new();
        sim.run([900; N], 400, false);
        let mut raw = [906; N];
        raw[3] = 1000;
        sim.run(raw, 10_000, false);
        assert_eq!(sim.key(1).baseline(), 900);
        assert_eq!(sim.key(3).baseline(), 900);
    }

    #[test]
    fn falls_quickly_and_recalibrates_when_negative() {
        let mut sim = Sim::new();
        // 起動時に key0 に触れていた
        let mut raw = [900; N];
        raw[0] = 1000;
        sim.run(raw, 400, false);
        assert_eq!(sim.key(0).baseline(), 1000);
        // 離すと大きく負になり、即時再校正される
        sim.run([900; N], 200, false);
        assert_eq!(sim.key(0).baseline(), 900);
    }

    #[test]
    fn stuck_key_recovers_but_wobbling_finger_does_not() {
        let mut sim = Sim::new();
        sim.run([900; N], 400, false);
        let mut raw = [900; N];
        raw[2] = 960; // 揺れない（異物）
        sim.run(raw, 31_000, false);
        assert_eq!(sim.key(2).baseline(), 960);

        let mut sim = Sim::new();
        sim.run([900; N], 400, false);
        for i in 0..400 {
            let mut raw = [900; N];
            raw[2] = if i % 2 == 0 { 950 } else { 970 }; // 指は揺れる
            sim.run(raw, 80, false);
        }
        assert_eq!(sim.key(2).baseline(), 900);
    }

    #[test]
    fn calibration_measures_noise_floor() {
        let mut sim = Sim::new();
        sim.run([900; N], 400, false);
        sim.run([900; N], 100, true);
        assert_eq!(sim.signal.state(), State::Calibrating);
        assert_eq!(sim.key(0).noise_floor(), 0);
        for i in 0..100 {
            let v = if i % 2 == 0 { 897 } else { 903 }; // ±3 のノイズ
            sim.run([v; N], 16, true);
        }
        sim.run([900; N], 8, false);
        assert_eq!(sim.signal.state(), State::Running);
        let nf = sim.key(0).noise_floor();
        assert!((2..=4).contains(&nf), "{nf}");

        // 短すぎる校正では計れないので既定値に戻す
        sim.run([900; N], 100, true);
        sim.run([900; N], 8, false);
        assert_eq!(
            sim.key(0).noise_floor(),
            Params::DEFAULT.noise_floor_default
        );
    }

    #[test]
    fn invalid_samples_keep_previous_filtered() {
        let mut sim = Sim::new();
        sim.run([900; N], 400, false);
        let mut raw = [900; N];
        raw[4] = 2000;
        sim.signal.process(sim.now, &raw, ALL & !(1 << 4), false);
        assert_eq!(sim.key(4).filtered(), 900);
        assert!(!sim.key(4).is_valid());
    }

    #[test]
    fn time_wraps() {
        let mut sim = Sim::new();
        sim.now = u32::MAX - 100_000;
        sim.run([900; N], 400, false);
        assert_eq!(sim.signal.state(), State::Running);
    }
}
