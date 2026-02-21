# Task List

## Core0
* qubit_touch_task(sender):
    - Touch Sensor の状態を i2ctask から Mutex で取得
    - Note On/Off を生成し、MIDI/ringledに送信
    - 10msec 周期

* usb_task(usb)
    - USB Task

* midi_rx_task(receiver)
    - MIDI 受信を ringled に送る

* ringled_task(common, sm0, p.DMA_CH0, p.PIN_26, ws2812_program)
    - NeoPixel の表示処理
    - MIDI 出力表示（４つのタッチの位置）
    - MIDI 入力表示（全音程のon/off）

## Core1
* core1_led_task(led)
    - onboard LED 点滅

* core1_i2c_task(i2c)
    - Touch Sensor を全key読み込む処理
        - PCA9544 で ch 選択
        - AT42QT1070
    - SSD1306 へのbitmap転送

* core1_oled_ui_task()
    - 表示したい変数の値を得る
    - GUI の表示イメージを作成し、i2c_task にそのまま送る
