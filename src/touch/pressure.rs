use portable_atomic::Ordering;
use crate::constants::*;
use crate::PRESSURE;

const PRESSURE_THRESHOLD: u32 = 32;

pub fn update_pressure(samples: &[u32; MAX_ADC_CHANNELS], sums: &mut [u64; MAX_ADC_CHANNELS], adc_counter: u32) {
	if adc_counter > 100 {
		// 4箇所のセンサーおのおの、前回までの積算値から平均値を計算する
		let mut averages = [0u32; MAX_ADC_CHANNELS];
		for i in 0..MAX_ADC_CHANNELS {
			averages[i] = (sums[i] / adc_counter as u64) as u32;
		}

		// 今回の4箇所のサンプルと平均値の差を計算し、サンプル側が大きい場合は0、小さい場合は差分値として保持
		let mut diffs = [0u32; MAX_ADC_CHANNELS];
		for i in 0..MAX_ADC_CHANNELS {
			diffs[i] = averages[i].saturating_sub(samples[i]);
		}

		// 差分値がある一定値以上なら印加圧力とみなし、４つのセンサーの圧力を加算して保存
		let mut total_pressure = 0u32;
		for diff in diffs.iter() {
			if *diff >= PRESSURE_THRESHOLD {
				total_pressure = total_pressure.saturating_add(*diff);
			}
		}
		PRESSURE.store(total_pressure, Ordering::Relaxed);
	}

	// 次回の平均値計算に備えて積算値を更新する
	for i in 0..MAX_ADC_CHANNELS {
		sums[i] = sums[i].wrapping_add(samples[i] as u64);
	}
}


