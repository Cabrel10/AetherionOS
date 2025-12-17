# Aetherion OS - Practical Usage Guide
## USB, SDR, and AI Features

**Version**: 1.1  
**Date**: 2025-12-17  
**Target Audience**: Developers and users wanting to use advanced features

---

## 🚀 Quick Start

### Prerequisites
- Aetherion OS installed (see [README.md](README.md))
- USB 3.0 controller (XHCI recommended)
- RTL-SDR dongle (optional, for radio features)
- Microphone (optional, for voice recognition)

---

## 📖 Table of Contents

1. [USB Devices](#1-usb-devices)
2. [Software Defined Radio](#2-software-defined-radio)
3. [AI Speech Recognition](#3-ai-speech-recognition)
4. [Advanced Use Cases](#4-advanced-use-cases)
5. [Troubleshooting](#5-troubleshooting)

---

## 1. USB Devices

### 1.1 USB Keyboard

#### Detection
```bash
# System automatically detects USB keyboards at boot
# Check kernel log:
dmesg | grep USB
# Expected output:
# [USB] Found 1 USB controller(s)
# [USB] Controller: 8086:9d2f at 00:14.0
# [USB] Device found: 046d:c534 (Logitech Keyboard)
```

#### Reading Input
```rust
use aetherion::drivers::usb::hid::UsbKeyboard;

// Initialize keyboard
let mut keyboard = UsbKeyboard::new(device, endpoint);

// Main input loop
loop {
    // Poll for new key presses
    let keys = keyboard.poll();
    
    for ch in keys {
        print!("{}", ch);
    }
    
    // Small delay to avoid CPU spinning
    sleep_ms(10);
}
```

#### Key Features
- **Modifier Keys**: Shift, Ctrl, Alt, Win/Meta
- **Special Keys**: Enter, Escape, Backspace, Tab, Delete
- **Simultaneous Keys**: Up to 6 keys at once
- **Auto-Repeat**: Not implemented (use software debouncing)

#### Example: Simple Text Editor
```rust
use alloc::string::String;

let mut buffer = String::new();
let mut keyboard = UsbKeyboard::new(device, endpoint);

loop {
    let keys = keyboard.poll();
    
    for ch in keys {
        match ch {
            '\n' => {
                // Enter pressed - process line
                println!("You typed: {}", buffer);
                buffer.clear();
            }
            '\x08' => {
                // Backspace
                buffer.pop();
            }
            _ => {
                buffer.push(ch);
            }
        }
    }
    
    // Display current buffer
    print!("\r> {}", buffer);
}
```

### 1.2 USB Mouse

#### Reading Mouse Events
```rust
use aetherion::drivers::usb::hid::UsbMouse;

let mut mouse = UsbMouse::new(device, endpoint);

loop {
    mouse.poll()?;
    
    println!("Position: ({}, {})", mouse.x, mouse.y);
    println!("Buttons: {:08b}", mouse.buttons);
    
    if mouse.buttons & 0x01 != 0 {
        println!("Left button pressed");
    }
}
```

### 1.3 USB Mass Storage
*(Implementation pending - structure defined)*

---

## 2. Software Defined Radio

### 2.1 RTL-SDR Setup

#### Hardware Requirements
- **Device**: RTL-SDR dongle (RTL2832U chipset)
- **Antenna**: Appropriate for target frequency
- **USB**: USB 2.0 or 3.0 port

#### Supported Models
- RTL-SDR.COM V3
- NooElec NESDR Smart
- Generic RTL2832U devices

#### Initial Configuration
```rust
use aetherion::drivers::sdr::rtlsdr::RtlSdr;

let mut sdr = RtlSdr::new();

// Attach USB device (detected automatically)
sdr.attach(usb_device)?;

// Initialize
sdr.init()?;

// Configure
sdr.set_sample_rate(2_048_000)?;  // 2.048 MSPS
sdr.tune(100_000_000)?;           // 100 MHz
```

### 2.2 FM Radio Reception

#### Complete FM Receiver
```rust
use aetherion::drivers::sdr::rtlsdr::RtlSdr;
use aetherion::drivers::sdr::demodulator::FmDemodulator;
use aetherion::drivers::sdr::IqSample;

// Initialize SDR
let mut sdr = RtlSdr::new();
sdr.init()?;

// Tune to FM station
sdr.tune(100_500_000)?;  // 100.5 MHz

// Create demodulator
let mut demod = FmDemodulator::new(sdr.get_sample_rate());

// Allocate buffer
let mut iq_buffer = vec![0u8; 16384];

loop {
    // Read IQ samples from SDR
    let bytes_read = sdr.read_samples(&mut iq_buffer)?;
    
    // Convert to IQ samples
    let iq_samples: Vec<IqSample> = iq_buffer[..bytes_read]
        .chunks(2)
        .map(|chunk| IqSample::from_u8(chunk[0], chunk[1]))
        .collect();
    
    // Demodulate FM
    let mut audio = demod.demodulate(&iq_samples);
    
    // Apply de-emphasis (75 μs for broadcast FM)
    demod.deemphasis(&mut audio);
    
    // Send to audio output
    audio_driver.write(&audio)?;
}
```

#### FM Station Examples (France)

| Frequency | Station | Location |
|-----------|---------|----------|
| 87.8 MHz | France Inter | Paris |
| 89.0 MHz | Virgin Radio | National |
| 89.9 MHz | RMC | National |
| 100.5 MHz | France Inter | National |
| 105.5 MHz | RTL | National |

#### Tuning Command-Line Tool
```bash
# Tune to specific frequency
aether-radio tune 100.5

# Scan for stations
aether-radio scan 87.5 108.0

# Record to file
aether-radio record 100.5 --duration 60 --output recording.wav
```

### 2.3 AM Radio Reception
```rust
use aetherion::drivers::sdr::demodulator::AmDemodulator;

let mut demod = AmDemodulator::new();

loop {
    let iq_samples = read_iq_samples()?;
    let audio = demod.demodulate(&iq_samples);
    play_audio(&audio)?;
}
```

### 2.4 Spectrum Analyzer
```rust
// Display real-time spectrum
loop {
    let iq_samples = read_iq_samples()?;
    
    // Compute FFT
    let spectrum = fft(&iq_samples);
    
    // Display
    for (freq, power) in spectrum.iter().enumerate() {
        let bar_length = (power * 50.0) as usize;
        println!("{:7.3} MHz: {}", 
                 (freq as f32 / 1000.0), 
                 "#".repeat(bar_length));
    }
}
```

---

## 3. AI Speech Recognition

### 3.1 Whisper Model Setup

#### Model Selection

| Model | Parameters | Size | Speed | Accuracy |
|-------|------------|------|-------|----------|
| Tiny | 39M | 75 MB | Fast | Good |
| Base | 74M | 140 MB | Medium | Better |
| Small | 244M | 460 MB | Slow | Excellent |

**Recommendation**: Use `tiny` for real-time, `base` for better accuracy

#### Loading the Model
```rust
use aetherion::ai::whisper::{WhisperModel, WhisperConfig};

// Create model
let mut whisper = WhisperModel::new(WhisperConfig::tiny());

// Load weights from file
let model_data = read_file("/models/whisper-tiny.bin")?;
whisper.load_weights(&model_data)?;
```

### 3.2 Basic Transcription

#### From Audio File
```rust
// Load audio (16 kHz, mono, 16-bit PCM)
let audio = load_audio_file("speech.wav")?;

// Transcribe
let result = whisper.transcribe(&audio)?;

println!("Text: {}", result.text);
println!("Confidence: {:.2}%", result.confidence * 100.0);
println!("Time: {} ms", result.processing_time_ms);
```

#### From Microphone (Real-Time)
```rust
use aetherion::ai::whisper::AudioBuffer;

let mut whisper = WhisperModel::new(WhisperConfig::tiny());
let mut audio_buffer = AudioBuffer::new(16000, 3000); // 3 seconds

loop {
    // Record from microphone
    let samples = microphone.read()?;
    audio_buffer.push(&samples);
    
    // Transcribe when buffer is full
    if audio_buffer.get_samples().len() >= 16000 * 3 {
        let result = whisper.transcribe(audio_buffer.get_samples())?;
        println!("Recognized: {}", result.text);
        audio_buffer.clear();
    }
}
```

### 3.3 Voice Commands

#### Command Parser
```rust
enum VoiceCommand {
    TuneRadio(f32),      // "tune 100.5"
    VolumeUp,            // "volume up"
    VolumeDown,          // "volume down"
    Shutdown,            // "shutdown"
    QueryTime,           // "what time is it"
    Unknown(String),
}

fn parse_command(text: &str) -> VoiceCommand {
    let text = text.to_lowercase();
    
    if text.starts_with("tune") {
        // Extract frequency
        if let Some(freq_str) = text.split_whitespace().nth(1) {
            if let Ok(freq) = freq_str.parse::<f32>() {
                return VoiceCommand::TuneRadio(freq);
            }
        }
    } else if text.contains("volume up") {
        return VoiceCommand::VolumeUp;
    } else if text.contains("volume down") {
        return VoiceCommand::VolumeDown;
    } else if text.contains("shutdown") {
        return VoiceCommand::Shutdown;
    } else if text.contains("time") {
        return VoiceCommand::QueryTime;
    }
    
    VoiceCommand::Unknown(text)
}
```

#### Voice-Controlled Radio
```rust
let mut whisper = WhisperModel::new(WhisperConfig::tiny());
let mut sdr = RtlSdr::new();
let mut audio_buffer = AudioBuffer::new(16000, 2000);

loop {
    // Listen for command
    let mic_samples = microphone.read()?;
    audio_buffer.push(&mic_samples);
    
    if audio_buffer.get_samples().len() >= 16000 * 2 {
        let result = whisper.transcribe(audio_buffer.get_samples())?;
        let command = parse_command(&result.text);
        
        match command {
            VoiceCommand::TuneRadio(freq) => {
                let freq_hz = (freq * 1_000_000.0) as u32;
                sdr.tune(freq_hz)?;
                println!("Tuned to {} MHz", freq);
            }
            VoiceCommand::VolumeUp => {
                increase_volume();
            }
            VoiceCommand::Shutdown => {
                shutdown_system();
            }
            _ => {}
        }
        
        audio_buffer.clear();
    }
}
```

---

## 4. Advanced Use Cases

### 4.1 Voice-Controlled FM Radio

**Scenario**: Hands-free radio tuning via voice commands

```rust
// Integrated system
struct VoiceRadio {
    sdr: RtlSdr,
    whisper: WhisperModel,
    fm_demod: FmDemodulator,
    audio_buffer: AudioBuffer,
}

impl VoiceRadio {
    pub fn run(&mut self) {
        loop {
            // Check for voice command (from microphone input channel)
            if let Some(command_audio) = self.check_for_command() {
                let result = self.whisper.transcribe(&command_audio)?;
                self.process_command(&result.text)?;
            }
            
            // Continue playing radio
            let iq = self.read_radio_samples()?;
            let audio = self.fm_demod.demodulate(&iq);
            self.play_audio(&audio)?;
        }
    }
    
    fn process_command(&mut self, text: &str) -> Result<(), &'static str> {
        match parse_command(text) {
            VoiceCommand::TuneRadio(freq) => {
                self.sdr.tune((freq * 1_000_000.0) as u32)?;
                speak(&format!("Tuning to {} megahertz", freq));
            }
            _ => {}
        }
        Ok(())
    }
}
```

### 4.2 USB File Browser with Voice Navigation

**Scenario**: Navigate USB drive files using voice

```rust
// Voice-controlled file browser
loop {
    let command = voice_input()?;
    
    match parse_command(&command) {
        "show files" => {
            list_directory("/")?;
        }
        "open <filename>" => {
            open_file(&extract_filename(&command))?;
        }
        "go back" => {
            change_directory("..")?;
        }
        _ => {}
    }
}
```

### 4.3 Real-Time Audio Transcription

**Scenario**: Transcribe live audio stream

```rust
let mut transcription_log = Vec::new();
let window_size = 16000 * 5; // 5 seconds

loop {
    let audio = capture_audio(window_size)?;
    let result = whisper.transcribe(&audio)?;
    
    transcription_log.push(result.text.clone());
    
    // Display with timestamp
    println!("[{}] {}", get_timestamp(), result.text);
}
```

---

## 5. Troubleshooting

### 5.1 USB Issues

**Problem**: USB device not detected

**Solutions**:
```bash
# Check PCI bus
lspci | grep USB

# Check kernel messages
dmesg | grep -i usb

# Verify XHCI controller is initialized
cat /proc/usb_controllers
```

**Problem**: Keyboard input not working

**Solutions**:
- Check endpoint configuration
- Verify HID report descriptor
- Try different USB port
- Check for driver conflicts

### 5.2 SDR Issues

**Problem**: No signal received

**Solutions**:
- Check antenna connection
- Verify frequency is in range (24 MHz - 1.7 GHz)
- Adjust gain settings
- Try different sample rate

**Problem**: Audio quality poor

**Solutions**:
```rust
// Increase sample rate
sdr.set_sample_rate(2_400_000)?;

// Adjust de-emphasis filter
demod.deemphasis(&mut audio);

// Apply noise reduction
apply_noise_gate(&mut audio, threshold);
```

### 5.3 AI/ML Issues

**Problem**: Transcription accuracy low

**Solutions**:
- Use larger model (base instead of tiny)
- Improve audio quality (reduce noise)
- Adjust microphone gain
- Use language-specific model

**Problem**: Slow inference

**Solutions**:
- Use smaller model (tiny)
- Enable quantization (if supported)
- Reduce audio length
- Increase inference batch size

---

## 📚 Additional Resources

- [USB Specification](https://www.usb.org/documents)
- [RTL-SDR Documentation](https://www.rtl-sdr.com/)
- [Whisper Model Paper](https://arxiv.org/abs/2212.04356)
- [Aetherion OS API Docs](https://aetherion-os.dev/api)

---

## 🤝 Contributing

Found a bug or want to add a feature? See [CONTRIBUTING.md](CONTRIBUTING.md)

---

## 📄 License

MIT License - See [LICENSE](LICENSE) for details

