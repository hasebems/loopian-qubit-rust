use embassy_time::{Duration, Timer, with_timeout};
use portable_atomic::Ordering;

use crate::constants::*;
use crate::error::ERROR_CODE;
use crate::shared::{AD_VALUE0, AD_VALUE1, AD_VALUE2, WORK_MODE_DISPLAY};
use crate::touch;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      ADC Task (Core0): GP27/GP28 の連続サンプリング
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn adc_task(
    mut adc: embassy_rp::adc::Adc<'static, embassy_rp::adc::Async>,
    adc_a1: embassy_rp::adc::Channel<'static>,
    adc_a2: embassy_rp::adc::Channel<'static>,
    adc_a3: embassy_rp::adc::Channel<'static>,
    mut adc_dma: embassy_rp::dma::Channel<'static>,
) {
    const ADC_READ_TIMEOUT_MS: u64 = 20;

    let mut channels = [adc_a1, adc_a2, adc_a3];
    let mut ad_value = [0u16; 3];
    let mut adc_counter = 0u32;
    let mut baseline_index = 0usize;
    let mut baseline_history =
        [[0u16; touch::pressure::PRESSURE_BASELINE_WINDOW]; MAX_ADC_CHANNELS];
    let mut baseline_sums = [0u64; MAX_ADC_CHANNELS];
    let mut samples = [0u32; MAX_ADC_CHANNELS];

    loop {
        // ADC読み込み準備時間を確保
        Timer::after_millis(10).await; // 1sensorあたり10msec

        let read_result = with_timeout(
            Duration::from_millis(ADC_READ_TIMEOUT_MS),
            adc.read_many_multichannel(&mut channels, &mut ad_value, 0, &mut adc_dma),
        )
        .await;

        match read_result {
            Ok(adc_result) => match adc_result {
                Ok(()) => {
                    AD_VALUE0.store(ad_value[0] as u32, Ordering::Relaxed);
                    AD_VALUE1.store(ad_value[1] as u32, Ordering::Relaxed);
                    AD_VALUE2.store(ad_value[2] as u32, Ordering::Relaxed);
                    samples[0] = ad_value[0] as u32;
                    samples[1] = ad_value[1] as u32;
                    samples[2] = ad_value[2] as u32;
                }
                Err(_) => {
                    // ADC内部エラー
                    ERROR_CODE.store(13, Ordering::Relaxed);
                }
            },
            Err(_) => {
                // タイムアウトエラー
                ERROR_CODE.store(13, Ordering::Relaxed);
            }
        }

        // 圧力を計算
        let wmd = WORK_MODE_DISPLAY.load(Ordering::Relaxed);
        touch::pressure::update_pressure(
            &samples,
            &mut baseline_history,
            &mut baseline_sums,
            &mut baseline_index,
            adc_counter,
            wmd,
        );
        adc_counter = adc_counter.wrapping_add(1);
    }
}
