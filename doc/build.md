フィーチャーフラグの整理：

| フィーチャー | 効果 |
|---|---|
| なし（デフォルト） | PCA9544: 4ch×4台、ADC: 3ch |
| `test_mode` | PCA9544: 1ch×1台、ADC: 3ch |
| `adc_ch4` | PCA9544: 4ch×4台、ADC: 4ch |
| `test_mode,adc_ch4` | PCA9544: 1ch×1台、ADC: 4ch |

`cargo clippy --features adc_ch4` のように個別に指定できます。

