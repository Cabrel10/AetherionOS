// Software Defined Radio (SDR) Driver Module

pub mod rtlsdr;
pub mod demodulator;

use alloc::vec::Vec;

/// SDR Device trait
pub trait SdrDevice {
    /// Initialize the SDR device
    fn init(&mut self) -> Result<(), &'static str>;
    
    /// Tune to a specific frequency (Hz)
    fn tune(&mut self, freq_hz: u32) -> Result<(), &'static str>;
    
    /// Set sample rate (Hz)
    fn set_sample_rate(&mut self, rate: u32) -> Result<(), &'static str>;
    
    /// Read IQ samples
    fn read_samples(&mut self, buffer: &mut [u8]) -> Result<usize, &'static str>;
    
    /// Get current frequency
    fn get_frequency(&self) -> u32;
    
    /// Get current sample rate
    fn get_sample_rate(&self) -> u32;
}

/// IQ Sample (In-phase and Quadrature)
#[derive(Debug, Copy, Clone)]
pub struct IqSample {
    pub i: i16,  // In-phase
    pub q: i16,  // Quadrature
}

impl IqSample {
    pub fn new(i: i16, q: i16) -> Self {
        Self { i, q }
    }
    
    /// Convert from 8-bit unsigned to 16-bit signed
    pub fn from_u8(i: u8, q: u8) -> Self {
        Self {
            i: (i as i16) - 127,
            q: (q as i16) - 127,
        }
    }
    
    /// Compute magnitude (sqrt(i^2 + q^2))
    pub fn magnitude(&self) -> f32 {
        let i = self.i as f32;
        let q = self.q as f32;
        libm::sqrtf(i * i + q * q)
    }
    
    /// Compute phase (atan2(q, i))
    pub fn phase(&self) -> f32 {
        libm::atan2f(self.q as f32, self.i as f32)
    }
}

/// Initialize SDR subsystem
pub fn init() -> Result<(), &'static str> {
    serial_print!("[SDR] Initializing Software Defined Radio subsystem...\n");
    
    // Detect RTL-SDR devices via USB
    // They use Realtek RTL2832U chipset
    // Vendor ID: 0x0bda, multiple product IDs
    
    serial_print!("[SDR] Scanning for RTL-SDR devices...\n");
    
    // TODO: Scan USB devices for RTL-SDR
    // For now, we assume one is available
    
    serial_print!("[SDR] RTL-SDR subsystem initialized\n");
    
    Ok(())
}
