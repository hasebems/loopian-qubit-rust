フィーチャーフラグの整理：

| フィーチャー | 効果 |
|---|---|
| なし（デフォルト） | PCA9544: 4ch×4台、ADC: 3ch |
| `test_mode` | PCA9544: 1ch×1台、ADC: 3ch |
| `no_pca9544` | PCA9544 無し（AT42QT1070 1個を I2C に直結）、`test_mode` を含む |
| `adc_ch4` | PCA9544: 4ch×4台、ADC: 4ch |
| `test_mode,adc_ch4` | PCA9544: 1ch×1台、ADC: 4ch |

`cargo clippy --features adc_ch4` のように個別に指定できます。

git push したときの clippy
`cargo clippy --all-features -- --deny=warnings`