use embassy_time::{Duration, Timer, with_timeout};
use portable_atomic::Ordering;

use crate::constants::*;
use crate::error;
use crate::shared::{AD_VALUE0, AD_VALUE1, AD_VALUE2, SETTING_MODE, WORK_MODE};
use crate::tasks::midi::queue_midi;
use crate::touch;

//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
//      Pressure Task (Core0): ADC (GP26/27/28) の連続サンプリング、圧力の計算、
//      Violin モードの CC11 (Expression) の送信
//+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
#[embassy_executor::task]
pub async fn pressure_task(
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
    let mut pressure_midi = touch::pressure::PressureMidiState::new();

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
                    error::set(error::ADC_READ);
                }
            },
            Err(_) => {
                // タイムアウトエラー
                error::set(error::ADC_READ);
            }
        }

        // 圧力を計算
        let wmd = SETTING_MODE.load(Ordering::Relaxed);
        touch::pressure::update_pressure(
            &samples,
            &mut baseline_history,
            &mut baseline_sums,
            &mut baseline_index,
            adc_counter,
            wmd,
        );
        adc_counter = adc_counter.wrapping_add(1);

        // Violin モードの CC11 (と、モードに入ったときの All Sound Off) を送信キューに入れる
        let work_mode = WORK_MODE
            .load(Ordering::Relaxed)
            .try_into()
            .unwrap_or(WorkMode::Piano);
        touch::pressure::pressure_cc11_if_needed(&mut pressure_midi, work_mode, queue_midi);
    }
}
