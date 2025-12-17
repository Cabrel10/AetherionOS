// Digital Signal Processing - Demodulators
// FM, AM, SSB demodulation for SDR

use super::IqSample;
use alloc::vec::Vec;

/// FM Demodulator
pub struct FmDemodulator {
    sample_rate: u32,
    prev_phase: f32,
    dc_offset_i: f32,
    dc_offset_q: f32,
    audio_buffer: Vec<i16>,
}

impl FmDemodulator {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            prev_phase: 0.0,
            dc_offset_i: 0.0,
            dc_offset_q: 0.0,
            audio_buffer: Vec::with_capacity(8192),
        }
    }
    
    /// Demodulate IQ samples to audio (FM)
    /// FM demodulation: Extract instantaneous frequency (derivative of phase)
    pub fn demodulate(&mut self, iq_samples: &[IqSample]) -> Vec<i16> {
        self.audio_buffer.clear();
        
        for sample in iq_samples {
            // Convert to float and remove DC offset
            let i = sample.i as f32 - self.dc_offset_i;
            let q = sample.q as f32 - self.dc_offset_q;
            
            // Update DC offset (simple moving average)
            self.dc_offset_i = self.dc_offset_i * 0.99 + sample.i as f32 * 0.01;
            self.dc_offset_q = self.dc_offset_q * 0.99 + sample.q as f32 * 0.01;
            
            // Calculate instantaneous phase
            let phase = libm::atan2f(q, i);
            
            // Calculate phase difference (derivative)
            let mut delta = phase - self.prev_phase;
            self.prev_phase = phase;
            
            // Wrap phase difference to [-π, π]
            while delta > core::f32::consts::PI {
                delta -= 2.0 * core::f32::consts::PI;
            }
            while delta < -core::f32::consts::PI {
                delta += 2.0 * core::f32::consts::PI;
            }
            
            // Convert phase difference to audio sample
            // Scale by sample rate and convert to i16
            let audio_sample = (delta * 32767.0 / core::f32::consts::PI) as i16;
            self.audio_buffer.push(audio_sample);
        }
        
        self.audio_buffer.clone()
    }
    
    /// Apply de-emphasis filter (75 μs for FM broadcast)
    pub fn deemphasis(&mut self, audio: &mut [i16]) {
        // First-order IIR filter
        // τ = 75 μs for FM broadcast
        
        let tau = 75e-6; // 75 microseconds
        let fs = 48000.0; // Output audio sample rate
        let alpha = 1.0 / (1.0 + 2.0 * core::f32::consts::PI * fs * tau);
        
        let mut prev = 0.0f32;
        
        for sample in audio.iter_mut() {
            let input = *sample as f32;
            let output = alpha * input + (1.0 - alpha) * prev;
            prev = output;
            *sample = output as i16;
        }
    }
}

/// AM Demodulator
pub struct AmDemodulator {
    dc_offset: f32,
}

impl AmDemodulator {
    pub fn new() -> Self {
        Self {
            dc_offset: 0.0,
        }
    }
    
    /// Demodulate IQ samples to audio (AM)
    /// AM demodulation: Extract envelope (magnitude)
    pub fn demodulate(&mut self, iq_samples: &[IqSample]) -> Vec<i16> {
        let mut audio = Vec::with_capacity(iq_samples.len());
        
        for sample in iq_samples {
            // Calculate magnitude (envelope)
            let magnitude = sample.magnitude();
            
            // Remove DC component
            self.dc_offset = self.dc_offset * 0.99 + magnitude * 0.01;
            let audio_sample = magnitude - self.dc_offset;
            
            // Scale to i16 range
            let scaled = (audio_sample * 327.67) as i16;
            audio.push(scaled);
        }
        
        audio
    }
}

/// Low-pass FIR filter
pub struct LowPassFilter {
    coefficients: Vec<f32>,
    buffer: Vec<f32>,
    index: usize,
}

impl LowPassFilter {
    /// Create new low-pass filter
    /// cutoff: cutoff frequency in Hz
    /// sample_rate: sample rate in Hz
    /// num_taps: number of filter taps (odd number recommended)
    pub fn new(cutoff: f32, sample_rate: f32, num_taps: usize) -> Self {
        let mut coefficients = Vec::with_capacity(num_taps);
        
        // Generate sinc-based FIR coefficients
        let fc = cutoff / sample_rate;
        let center = (num_taps / 2) as i32;
        
        for i in 0..num_taps {
            let n = i as i32 - center;
            let coeff = if n == 0 {
                2.0 * fc
            } else {
                let pi_n = core::f32::consts::PI * n as f32;
                libm::sinf(2.0 * core::f32::consts::PI * fc * n as f32) / pi_n
            };
            
            // Apply Hamming window
            let window = 0.54 - 0.46 * libm::cosf(2.0 * core::f32::consts::PI * i as f32 / (num_taps - 1) as f32);
            coefficients.push(coeff * window);
        }
        
        // Normalize
        let sum: f32 = coefficients.iter().sum();
        for coeff in &mut coefficients {
            *coeff /= sum;
        }
        
        Self {
            coefficients,
            buffer: vec![0.0; num_taps],
            index: 0,
        }
    }
    
    /// Filter a single sample
    pub fn filter(&mut self, input: f32) -> f32 {
        // Add new sample to circular buffer
        self.buffer[self.index] = input;
        self.index = (self.index + 1) % self.buffer.len();
        
        // Compute convolution
        let mut output = 0.0;
        for (i, &coeff) in self.coefficients.iter().enumerate() {
            let buf_idx = (self.index + i) % self.buffer.len();
            output += coeff * self.buffer[buf_idx];
        }
        
        output
    }
    
    /// Filter a batch of samples
    pub fn filter_batch(&mut self, input: &[f32]) -> Vec<f32> {
        input.iter().map(|&x| self.filter(x)).collect()
    }
}

/// Decimator - reduce sample rate by integer factor
pub struct Decimator {
    factor: usize,
    counter: usize,
    filter: LowPassFilter,
}

impl Decimator {
    pub fn new(factor: usize, sample_rate: f32) -> Self {
        // Anti-aliasing filter at nyquist frequency of output
        let cutoff = sample_rate / (2.0 * factor as f32);
        let filter = LowPassFilter::new(cutoff, sample_rate, 51);
        
        Self {
            factor,
            counter: 0,
            filter,
        }
    }
    
    /// Decimate samples
    pub fn decimate(&mut self, input: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(input.len() / self.factor);
        
        for &sample in input {
            let filtered = self.filter.filter(sample);
            
            if self.counter == 0 {
                output.push(filtered);
            }
            
            self.counter = (self.counter + 1) % self.factor;
        }
        
        output
    }
}

impl Default for AmDemodulator {
    fn default() -> Self {
        Self::new()
    }
}
