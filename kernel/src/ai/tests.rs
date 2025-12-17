// AI Module Tests

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::super::tensor::*;
    use super::super::whisper::*;
    
    #[test]
    fn test_tensor_creation_zeros() {
        let tensor = Tensor::zeros(&[2, 3]);
        assert_eq!(tensor.shape(), &[2, 3]);
        assert_eq!(tensor.data().len(), 6);
        assert!(tensor.data().iter().all(|&x| x == 0.0));
    }
    
    #[test]
    fn test_tensor_creation_ones() {
        let tensor = Tensor::ones(&[2, 2]);
        assert_eq!(tensor.shape(), &[2, 2]);
        assert!(tensor.data().iter().all(|&x| x == 1.0));
    }
    
    #[test]
    fn test_tensor_from_slice() {
        let data = vec![1.0, 2.0, 3.0, 4.0];
        let tensor = Tensor::from_slice(&data, &[2, 2]);
        assert_eq!(tensor.data(), &[1.0, 2.0, 3.0, 4.0]);
    }
    
    #[test]
    fn test_tensor_matmul() {
        // Create 2x2 matrices
        let a = Tensor::from_slice(&[1.0, 2.0, 3.0, 4.0], &[2, 2]);
        let b = Tensor::from_slice(&[5.0, 6.0, 7.0, 8.0], &[2, 2]);
        
        let c = a.matmul(&b).unwrap();
        
        // Expected result:
        // [1 2] * [5 6] = [19 22]
        // [3 4]   [7 8]   [43 50]
        assert_eq!(c.data()[0], 19.0);
        assert_eq!(c.data()[1], 22.0);
        assert_eq!(c.data()[2], 43.0);
        assert_eq!(c.data()[3], 50.0);
    }
    
    #[test]
    fn test_tensor_add() {
        let a = Tensor::from_slice(&[1.0, 2.0, 3.0], &[3]);
        let b = Tensor::from_slice(&[4.0, 5.0, 6.0], &[3]);
        
        let c = a.add(&b).unwrap();
        assert_eq!(c.data(), &[5.0, 7.0, 9.0]);
    }
    
    #[test]
    fn test_tensor_mul() {
        let a = Tensor::from_slice(&[2.0, 3.0, 4.0], &[3]);
        let b = Tensor::from_slice(&[5.0, 6.0, 7.0], &[3]);
        
        let c = a.mul(&b).unwrap();
        assert_eq!(c.data(), &[10.0, 18.0, 28.0]);
    }
    
    #[test]
    fn test_tensor_relu() {
        let mut tensor = Tensor::from_slice(&[-1.0, 0.0, 1.0, 2.0], &[4]);
        tensor.relu();
        assert_eq!(tensor.data(), &[0.0, 0.0, 1.0, 2.0]);
    }
    
    #[test]
    fn test_tensor_softmax() {
        let mut tensor = Tensor::from_slice(&[1.0, 2.0, 3.0], &[3]);
        tensor.softmax();
        
        // Sum should be approximately 1.0
        let sum: f32 = tensor.data().iter().sum();
        assert!((sum - 1.0).abs() < 0.001);
        
        // Each value should be between 0 and 1
        assert!(tensor.data().iter().all(|&x| x >= 0.0 && x <= 1.0));
    }
    
    #[test]
    fn test_whisper_config_tiny() {
        let config = WhisperConfig::tiny();
        assert_eq!(config.n_vocab, 51864);
        assert_eq!(config.n_audio_layer, 4);
        assert_eq!(config.n_text_layer, 4);
        assert_eq!(config.n_audio_state, 384);
    }
    
    #[test]
    fn test_whisper_config_base() {
        let config = WhisperConfig::base();
        assert_eq!(config.n_vocab, 51864);
        assert_eq!(config.n_audio_layer, 6);
        assert_eq!(config.n_text_layer, 6);
        assert_eq!(config.n_audio_state, 512);
    }
    
    #[test]
    fn test_whisper_model_creation() {
        let config = WhisperConfig::tiny();
        let model = WhisperModel::new(config.clone());
        assert_eq!(model.config.n_vocab, config.n_vocab);
    }
    
    #[test]
    fn test_audio_buffer_creation() {
        let buffer = AudioBuffer::new(16000, 1000); // 1 second
        assert_eq!(buffer.sample_rate, 16000);
        assert_eq!(buffer.capacity, 16000);
    }
    
    #[test]
    fn test_audio_buffer_push() {
        let mut buffer = AudioBuffer::new(16000, 100); // 100ms = 1600 samples
        let samples = vec![1, 2, 3, 4, 5];
        
        buffer.push(&samples);
        assert_eq!(buffer.get_samples().len(), 5);
    }
    
    #[test]
    fn test_audio_buffer_overflow() {
        let mut buffer = AudioBuffer::new(16000, 10); // Only 160 samples capacity
        let samples = vec![0i16; 200];
        
        buffer.push(&samples);
        
        // Should not exceed capacity
        assert!(buffer.get_samples().len() <= 160);
    }
    
    #[test]
    fn test_tensor_layer_norm() {
        let mut tensor = Tensor::from_slice(&[1.0, 2.0, 3.0, 4.0], &[4]);
        tensor.layer_norm(1e-5);
        
        // After normalization, mean should be ~0 and std dev should be ~1
        let mean: f32 = tensor.data().iter().sum::<f32>() / 4.0;
        assert!(mean.abs() < 0.001);
    }
}
