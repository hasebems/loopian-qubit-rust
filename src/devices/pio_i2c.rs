//  Created by Hasebe Masahiko on 2026/11/25.
//  Copyright (c) 2026 Hasebe Masahiko.
//  Released under the MIT license
//  https://opensource.org/licenses/mit-license.php
//
//! PIO で動く I2C のマスター。OLED と PCF8574（スイッチ）のバスに使う（doc/hw96_firmware261114.md §3）
//!
//! embedded-hal-async の `I2c` を実装するので、`ssd1306`・`pcf8574` のドライバは PIO かどうかを知らなくてよい。
//!
//! PIO のプログラムは pico-examples の `pio/i2c/i2c.pio` を移したもの
//! （Copyright (c) 2020 Raspberry Pi (Trading) Ltd. SPDX-License-Identifier: BSD-3-Clause）。
//! 変えたのは NACK のところだけ（下の「NACK」）。
//!
//! 動作の覚え書き
//! - SCL は SDA の隣（SDA + 1）でなければならない。`wait 1 pin, 1` は IN の基点（SDA）の次のピン（SCL）を待つ
//!   （クロックストレッチ）。基板では SDA=GP3、SCL=GP4
//! - ピンの OE を反転させる。pindirs = 1 でピンを離し（プルアップで High）、0 で Low に引く。出力値は常に Low
//! - TX FIFO の 1 語（32 ビットの上位 16 ビットを使う）は 2 種類:
//!     - データ: [15:10] = 0、[9] = FINAL（ACK の位置で SDA が High でも止まらない）、[8:1] = バイト、
//!       [0] = ACK の位置で SDA を離すか（書き込みとアドレスは 1 で相手の ACK を見る。読み込みは 0 = ACK、最後だけ 1 = NACK）
//!     - 命令: [15:10] = n。続く n + 1 語を命令として実行する（START・STOP・repeated START を作る）
//! - 送ったバイトは、書き込みでも毎回 RX FIFO に入る（`in pins, 1` を常に行うため）。読み捨てないと RX があふれて SM が止まる
//! - NACK: SM は IRQ を立てて止まる。pico-examples は `irq wait` で止まるが、embassy-rp の `Irq::wait` はフラグを
//!   すぐ下ろすので、そのままだと SM が先へ進んでしまう。そこで `irq`（待たない）の後、自分へ飛ぶループで止める
//! - 転送の終わり: 最後の語を FIFO に入れても、バスの上ではまだ送っている。TX が空で、SM が FIFO を待って止まった
//!   （TXSTALL）ことを見て終わりとする。最後のバイトの NACK も、それまでに見る
//! - 途中で打ち切られた（`with_timeout` で future が捨てられた）・NACK のときは、次の転送の前にバスを戻す
//!   （SM を止めて、SDA を離したまま SCL を 9 回動かし、STOP を出し、SM を初期化し直す）。起動後の最初の転送の前にも行う
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_rp::Peri;
use embassy_rp::clocks::clk_sys_freq;
use embassy_rp::gpio::{Level, Pull};
use embassy_rp::pio::{
    Common, Config, Direction, Instance, Irq, IrqFlags, LoadedProgram, Pin, PioPin, ShiftConfig,
    ShiftDirection, StateMachine,
};
use embassy_time::Timer;
use embedded_hal_async::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};
use fixed::FixedU32;
use fixed::types::extra::U8;

// TX FIFO の語のビット位置（上の覚え書き）
const ICOUNT_LSB: u32 = 10;
const FINAL_LSB: u32 = 9;
const DATA_LSB: u32 = 1;
const NAK_LSB: u32 = 0;

// START・STOP を作る命令の番号（SCL・SDA の状態。0 = Low に引く、1 = 離す）
const SC0_SD0: usize = 0;
const SC0_SD1: usize = 1;
const SC1_SD0: usize = 2;
const SC1_SD1: usize = 3;

// バスの復帰で SCL を動かすときの半周期（µs）。100kHz 以下にする
const RECOVERY_HALF_PERIOD_US: u64 = 5;
// 転送の終わりを待つときに見る間隔（µs）。1 バイトは 400kHz で約 22.5µs
const IDLE_POLL_US: u64 = 20;

/// PIO に載せた I2C のプログラム（19 命令）
pub struct PioI2cProgram<'d, PIO: Instance> {
    prg: LoadedProgram<'d, PIO>,
    entry_point: u8,
    /// FIFO から実行させる START・STOP 用の命令（遅延付き）
    fifo_instr: [u16; 4],
    /// バスの復帰で CPU から直接実行する命令（遅延無し。間隔は CPU 側で待つ）
    direct_instr: [u16; 4],
}

impl<'d, PIO: Instance> PioI2cProgram<'d, PIO> {
    /// プログラムを PIO の命令メモリの先頭に載せる（`.origin 0`。その PIO には他のプログラムを載せない）
    pub fn new(common: &mut Common<'d, PIO>) -> Self {
        let prg = embassy_rp::pio::program::pio_asm!(
            r#"
                .side_set 1 opt pindirs
                .origin 0

                do_nack:
                    jmp y-- entry_point        ; FINAL のバイトなら、NACK でも続ける
                    irq 0 rel                  ; NACK: IRQ を立てて
                nack_halt:
                    jmp nack_halt              ; 止まる（ドライバが SM を初期化し直すまで）
                do_byte:
                    set x, 7                   ; 8 ビット
                bitloop:
                    out pindirs, 1         [7] ; データを SDA に出す（読み込みのときは 1 = 離す）
                    nop             side 1 [2] ; SCL の立ち上がり
                    wait 1 pin, 1          [4] ; クロックストレッチを待つ
                    in pins, 1             [7] ; SCL が High の中ほどで SDA を読む
                    jmp x-- bitloop side 0 [7] ; SCL の立ち下がり

                    out pindirs, 1         [7] ; ACK の位置（読み込みのときは、こちらが ACK を返す）
                    nop             side 1 [7] ; SCL の立ち上がり
                    wait 1 pin, 1          [7] ; クロックストレッチを待つ
                    jmp pin do_nack side 0 [2] ; SDA が High なら NACK

                public entry_point:
                .wrap_target
                    out x, 6                   ; 命令の数（0 ならデータ）
                    out y, 1                   ; FINAL
                    jmp !x do_byte
                    out null, 32               ; 命令のときは、この語の残りを捨てる
                do_exec:
                    out exec, 16               ; 1 語に 1 命令を実行する
                    jmp x-- do_exec
                .wrap
            "#
        );
        // START・STOP 用の命令。i2c と同じ side-set の設定で組み立てる（命令の符号が side-set の設定で変わるため）
        let table = embassy_rp::pio::program::pio_asm!(
            r#"
                .side_set 1 opt
                    set pindirs, 0 side 0 [7] ; SC0_SD0
                    set pindirs, 1 side 0 [7] ; SC0_SD1
                    set pindirs, 0 side 1 [7] ; SC1_SD0
                    set pindirs, 1 side 1 [7] ; SC1_SD1
                    set pindirs, 0 side 0
                    set pindirs, 1 side 0
                    set pindirs, 0 side 1
                    set pindirs, 1 side 1
            "#
        );
        let code = &table.program.code;
        Self {
            prg: common.load_program(&prg.program),
            entry_point: prg.public_defines.entry_point as u8,
            fifo_instr: [code[0], code[1], code[2], code[3]],
            direct_instr: [code[4], code[5], code[6], code[7]],
        }
    }
}

/// PIO の I2C のエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// 相手が ACK を返さなかった（デバイスが無い・アドレスの違いなど）
    Nack,
}

impl embedded_hal_async::i2c::Error for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Nack => ErrorKind::NoAcknowledge(NoAcknowledgeSource::Unknown),
        }
    }
}

/// PIO で動く I2C のマスター。SM 番号の IRQ（`irq 0 rel` なので IRQ SM）を NACK の知らせに使う
pub struct PioI2c<'d, PIO: Instance, const SM: usize> {
    sm: StateMachine<'d, PIO, SM>,
    irq: Irq<'d, PIO, SM>,
    irq_flags: IrqFlags<'d, PIO>,
    _sda: Pin<'d, PIO>,
    _scl: Pin<'d, PIO>,
    entry_point: u8,
    fifo_instr: [u16; 4],
    direct_instr: [u16; 4],
    /// 次の転送の前にバスを戻すか（起動直後・打ち切られた転送の後）
    needs_recovery: bool,
    /// 起動後の最初の復帰か（回数に数えない）
    first_recovery: bool,
    /// バスを戻した回数（起動直後の 1 回を除く）。`take_recovery_count` で読み出す
    recovery_count: u32,
}

impl<'d, PIO: Instance, const SM: usize> PioI2c<'d, PIO, SM> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        common: &mut Common<'d, PIO>,
        mut sm: StateMachine<'d, PIO, SM>,
        irq: Irq<'d, PIO, SM>,
        irq_flags: IrqFlags<'d, PIO>,
        sda: Peri<'d, impl PioPin + 'd>,
        scl: Peri<'d, impl PioPin + 'd>,
        program: &PioI2cProgram<'d, PIO>,
        frequency: u32,
    ) -> Self {
        let mut sda = common.make_pio_pin(sda);
        let mut scl = common.make_pio_pin(scl);
        // 外付けのプルアップが前提。内蔵は補助（400kHz では内蔵だけには頼らない）
        sda.set_pull(Pull::Up);
        scl.set_pull(Pull::Up);
        // バスに Low のひげを出さないよう、High を出してから OE を反転させ（= 離す）、出力値を Low にする
        sm.set_pins(Level::High, &[&sda, &scl]);
        sm.set_pin_dirs(Direction::Out, &[&sda, &scl]);
        sda.set_output_enable_inversion(true);
        scl.set_output_enable_inversion(true);
        sm.set_pins(Level::Low, &[&sda, &scl]);

        let mut cfg = Config::default();
        cfg.use_program(&program.prg, &[&scl]);
        cfg.set_out_pins(&[&sda]);
        cfg.set_set_pins(&[&sda]);
        cfg.set_in_pins(&[&sda, &scl]);
        cfg.set_jmp_pin(&sda);
        cfg.shift_out = ShiftConfig {
            threshold: 16,
            direction: ShiftDirection::Left,
            auto_fill: true,
        };
        cfg.shift_in = ShiftConfig {
            threshold: 8,
            direction: ShiftDirection::Left,
            auto_fill: true,
        };
        // 1 ビットは PIO の 32 サイクル。分周比は小数 8 ビット（clk_sys が 150MHz なら 11.71875）
        let div_bits = (clk_sys_freq() as u64 * 256 / (32 * frequency as u64)) as u32;
        cfg.clock_divider = FixedU32::<U8>::from_bits(div_bits);
        sm.set_config(&cfg);

        let mut this = Self {
            sm,
            irq,
            irq_flags,
            _sda: sda,
            _scl: scl,
            entry_point: program.entry_point,
            fifo_instr: program.fifo_instr,
            direct_instr: program.direct_instr,
            needs_recovery: true,
            first_recovery: true,
            recovery_count: 0,
        };
        this.restart_sm();
        this
    }

    /// バスを戻した回数を読み出して 0 にする（診断用）
    pub fn take_recovery_count(&mut self) -> u32 {
        core::mem::take(&mut self.recovery_count)
    }

    /// SM を初期化し直して、entry_point から動かす
    fn restart_sm(&mut self) {
        self.sm.set_enable(false);
        self.sm.clear_fifos();
        self.sm.restart();
        self.sm.clkdiv_restart();
        // SAFETY: 止めた SM を、プログラムの entry_point へ飛ばすだけ
        unsafe { self.sm.exec_jmp(self.entry_point) };
        self.irq_flags.clear(SM);
        self.sm.set_enable(true);
    }

    /// CPU から SM に命令を 1 つ実行させ、半周期待つ
    async fn exec_direct(&mut self, index: usize) {
        // SAFETY: SM は止めてある。SET PINDIRS（と side-set）だけで、プログラムの流れは変えない
        unsafe { self.sm.exec_instr(self.direct_instr[index]) };
        Timer::after_micros(RECOVERY_HALF_PERIOD_US).await;
    }

    /// バスを戻す: SM を止め、SDA を離したまま SCL を 9 回動かし（相手が送りかけのバイトを出し切って SDA を離す）、
    /// STOP を出してから SM を初期化し直す
    async fn recover(&mut self) {
        self.sm.set_enable(false);
        self.sm.clear_fifos();
        self.sm.restart();
        for _ in 0..9 {
            self.exec_direct(SC0_SD1).await;
            self.exec_direct(SC1_SD1).await;
        }
        self.exec_direct(SC0_SD0).await;
        self.exec_direct(SC1_SD0).await;
        self.exec_direct(SC1_SD1).await;
        self.restart_sm();
        if self.first_recovery {
            self.first_recovery = false;
        } else {
            self.recovery_count = self.recovery_count.wrapping_add(1);
        }
    }

    /// TX FIFO に 1 語入れる。NACK で SM が止まったらエラー
    async fn push(&mut self, word: u32) -> Result<(), Error> {
        match select(self.sm.tx().wait_push(word << 16), self.irq.wait()).await {
            Either::First(()) => Ok(()),
            Either::Second(()) => Err(Error::Nack),
        }
    }

    /// FIFO から実行させる命令の並びを送る
    async fn push_instrs(&mut self, indices: &[usize]) -> Result<(), Error> {
        self.push(((indices.len() as u32) - 1) << ICOUNT_LSB)
            .await?;
        for &i in indices {
            self.push(self.fifo_instr[i] as u32).await?;
        }
        Ok(())
    }

    /// 向き（読み・書き）が同じ操作の続き 1 つを送る: アドレス → 各バイト。
    /// 送ったバイトの数だけ RX にバイトが入るので、すべて受け取るまで待つ（読み込みは書き込み先へ、他は捨てる）
    async fn run_segment(
        &mut self,
        address: u8,
        ops: &mut [Operation<'_>],
        read: bool,
    ) -> Result<(), Error> {
        let total = 1 + ops.iter().map(op_len).sum::<usize>(); // アドレスを含む
        let mut sent = 0;
        let mut received = 0;
        while received < total {
            let word = (sent < total).then(|| tx_word(address, ops, read, sent, total));
            let (rx, tx) = self.sm.rx_tx();
            match word {
                Some(word) => {
                    match select3(tx.wait_push(word << 16), rx.wait_pull(), self.irq.wait()).await {
                        Either3::First(()) => sent += 1,
                        Either3::Second(value) => {
                            store_rx(ops, read, received, value as u8);
                            received += 1;
                        }
                        Either3::Third(()) => return Err(Error::Nack),
                    }
                }
                None => match select(rx.wait_pull(), self.irq.wait()).await {
                    Either::First(value) => {
                        store_rx(ops, read, received, value as u8);
                        received += 1;
                    }
                    Either::Second(()) => return Err(Error::Nack),
                },
            }
        }
        Ok(())
    }

    /// STOP を出し終えるまで待つ。最後のバイトの NACK もここで見る
    async fn wait_idle(&mut self) -> Result<(), Error> {
        // TXSTALL は立ったままになるので、一度下ろしてから、もう一度立つのを待つ
        let _ = self.sm.tx().stalled();
        loop {
            if self.irq_flags.check(SM as u8) {
                return Err(Error::Nack);
            }
            if self.sm.tx().empty() && self.sm.tx().stalled() {
                return Ok(());
            }
            Timer::after_micros(IDLE_POLL_US).await;
        }
    }

    async fn run(&mut self, address: u8, ops: &mut [Operation<'_>]) -> Result<(), Error> {
        let mut i = 0;
        while i < ops.len() {
            // 向きが同じ操作は続けて送る（間に repeated START を入れない。embedded-hal の約束）
            let read = is_read(&ops[i]);
            let mut j = i + 1;
            while j < ops.len() && is_read(&ops[j]) == read {
                j += 1;
            }
            if i == 0 {
                self.push_instrs(&[SC1_SD0, SC0_SD0]).await?; // START
            } else {
                self.push_instrs(&[SC0_SD1, SC1_SD1, SC1_SD0, SC0_SD0])
                    .await?; // repeated START
            }
            self.run_segment(address, &mut ops[i..j], read).await?;
            i = j;
        }
        self.push_instrs(&[SC0_SD0, SC1_SD0, SC1_SD1]).await?; // STOP
        self.wait_idle().await
    }
}

fn is_read(op: &Operation<'_>) -> bool {
    matches!(op, Operation::Read(_))
}

fn op_len(op: &Operation<'_>) -> usize {
    match op {
        Operation::Read(buf) => buf.len(),
        Operation::Write(buf) => buf.len(),
    }
}

/// 操作の続きの中の通し番号 n のバイトが、どの操作の何バイト目か
fn locate(ops: &[Operation<'_>], mut n: usize) -> (usize, usize) {
    for (i, op) in ops.iter().enumerate() {
        let len = op_len(op);
        if n < len {
            return (i, n);
        }
        n -= len;
    }
    (ops.len(), 0)
}

/// k 番目（0 はアドレス）に送る語
fn tx_word(address: u8, ops: &[Operation<'_>], read: bool, k: usize, total: usize) -> u32 {
    if k == 0 {
        // アドレスと R/W。相手の ACK を見る
        let byte = ((address as u32) << 1) | read as u32;
        return (byte << DATA_LSB) | (1 << NAK_LSB);
    }
    if read {
        // 0xFF を送って SDA を離し、相手のデータを読む。最後のバイトだけ NACK を返し、それで止まらない
        let last = k == total - 1;
        let tail = if last {
            (1 << FINAL_LSB) | (1 << NAK_LSB)
        } else {
            0
        };
        (0xFF << DATA_LSB) | tail
    } else {
        let (i, pos) = locate(ops, k - 1);
        let byte = match ops.get(i) {
            Some(Operation::Write(buf)) => buf[pos] as u32,
            _ => 0,
        };
        (byte << DATA_LSB) | (1 << NAK_LSB)
    }
}

/// k 番目（0 はアドレス）のバイトの RX を受け取る。読み込みのバイトだけ書き込み先に入れる
fn store_rx(ops: &mut [Operation<'_>], read: bool, k: usize, value: u8) {
    if !read || k == 0 {
        return;
    }
    let (i, pos) = locate(ops, k - 1);
    if let Some(Operation::Read(buf)) = ops.get_mut(i) {
        buf[pos] = value;
    }
}

impl<PIO: Instance, const SM: usize> ErrorType for PioI2c<'_, PIO, SM> {
    type Error = Error;
}

impl<PIO: Instance, const SM: usize> I2c for PioI2c<'_, PIO, SM> {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        if operations.is_empty() {
            return Ok(());
        }
        if self.needs_recovery {
            self.recover().await;
        }
        // 途中で打ち切られたら（future が捨てられたら）立ったまま残り、次の転送の前にバスを戻す
        self.needs_recovery = true;
        let result = self.run(address, operations).await;
        if result.is_err() {
            self.recover().await;
        }
        self.needs_recovery = false;
        result
    }
}
