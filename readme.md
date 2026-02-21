# Loopian::QUBIT in Rust

## 概要

- 2026/2 より開発開始
- Seeed XIAO RP2350 を利用し、組み込みRust & Embassy で開発
- 2025に開発した Arduino 版 Loopian::QUBIT の機能を Rust に移植
- Embassy 周りのかなりの部分を github copilot を利用して開発

## 提供機能

### Touch Sensor による MIDI 送信処理

- QUBIT のタッチ処理(QubitTouch)を Rust に移植
- I2C(Core1) で読み込んだ生値を TOUCH_RAW_DATA に入れ、Mutex で保護
- Core0 の qubit_touch_task で読み込み、解析して MIDI を生成

### I2C (Core1)

- SSD1306 による OLED Display の表示機能の実装
- AT42QT1070 によるタッチセンサー機能の実装
    - PCA9544 により複数個のセンサーを読み込み可能

### NeoPixel (Core0)

- NeoPixel(RGBW) をPIOで制御可能
- USB MIDI の受信メッセージに反応させる
- USB MIDI の送信メッセージにも反応させる

## USB MIDI (Core0)

- USB MIDI 受信機能
- USB MIDI 送信機能
