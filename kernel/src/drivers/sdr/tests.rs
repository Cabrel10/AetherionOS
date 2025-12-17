// SDR Driver Tests

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::super::rtlsdr::*;
    use super::super::demodulator::*;
    
    #[test]
    fn test_iq_sample_creation() {
        let sample = IqSample::new(100, -50);
        assert_eq!(sample.i, 100);
        assert_eq!(sample.q, -50);
    }
    
    #[test]
    fn test_iq_sample_from_u8() {
        let sample = IqSample::from_u8(200, 50);
        assert_eq!(sample.i, 73);  // 200 - 127
        assert_eq!(sample.q, -77); // 50 - 127
    }
    
    #[test]
    fn test_iq_sample_magnitude() {
        let sample = IqSample::new(3, 4);
        let mag = sample.magnitude();
        assert!((mag - 5.0).abs() < 0.001); // sqrt(9 + 16) = 5
    }
    
    #[test]
    fn test_iq_sample_phase() {
        let sample = IqSample::new(1, 1);
        let phase = sample.phase();
        assert!((phase - core::f32::consts::FRAC_PI_4).abs() < 0.001); // 45 degrees
    }
    
    #[test]
    fn test_rtlsdr_frequency_range() {
        let sdr = RtlSdr::new();
        
        // Valid frequency
        assert!(sdr.get_frequency() > 0);
        
        // Default frequency should be 100 MHz
        assert_eq!(sdr.get_frequency(), 100_000_000);
    }
    
    #[test]
    fn test_rtlsdr_sample_rate() {
        let sdr = RtlSdr::new();
        
        // Default should be 2.048 MHz
        assert_eq!(sdr.get_sample_rate(), 2_048_000);
    }
    
    #[test]
    fn test_fm_demodulator_creation() {
        let demod = FmDemodulator::new(2_048_000);
        assert_eq!(demod.sample_rate, 2_048_000);
    }
    
    #[test]
    fn test_fm_demodulation_output_size() {
        let mut demod = FmDemodulator::new(2_048_000);
        
        // Create test IQ samples
        let samples = vec![
            IqSample::new(100, 0),
            IqSample::new(0, 100),
            IqSample::new(-100, 0),
            IqSample::new(0, -100),
        ];
        
        let audio = demod.demodulate(&samples);
        
        // Output should have same number of samples
        assert_eq!(audio.len(), samples.len());
    }
    
    #[test]
    fn test_am_demodulator_creation() {
        let demod = AmDemodulator::new();
        assert_eq!(demod.dc_offset, 0.0);
    }
    
    #[test]
    fn test_lowpass_filter_creation() {
        let filter = LowPassFilter::new(1000.0, 48000.0, 51);
        assert_eq!(filter.coefficients.len(), 51);
    }
    
    #[test]
    fn test_decimator_creation() {
        let decimator = Decimator::new(4, 48000.0);
        assert_eq!(decimator.factor, 4);
    }
    
    #[test]
    fn test_decimation() {
        let mut decimator = Decimator::new(2, 48000.0);
        let input = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let output = decimator.decimate(&input);
        
        // Output should be half the size
        assert!(output.len() <= input.len() / 2 + 1);
    }
}
