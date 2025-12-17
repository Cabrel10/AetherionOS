// Tensor operations for ML inference
// Simplified tensor library for kernel space

use alloc::vec::Vec;

/// N-dimensional tensor
#[derive(Debug, Clone)]
pub struct Tensor {
    data: Vec<f32>,
    shape: Vec<usize>,
}

impl Tensor {
    /// Create tensor filled with zeros
    pub fn zeros(shape: &[usize]) -> Self {
        let size = shape.iter().product();
        Self {
            data: vec![0.0; size],
            shape: shape.to_vec(),
        }
    }
    
    /// Create tensor filled with ones
    pub fn ones(shape: &[usize]) -> Self {
        let size = shape.iter().product();
        Self {
            data: vec![1.0; size],
            shape: shape.to_vec(),
        }
    }
    
    /// Create tensor from data
    pub fn from_slice(data: &[f32], shape: &[usize]) -> Self {
        assert_eq!(data.len(), shape.iter().product());
        Self {
            data: data.to_vec(),
            shape: shape.to_vec(),
        }
    }
    
    /// Get tensor shape
    pub fn shape(&self) -> &[usize] {
        &self.shape
    }
    
    /// Get tensor data
    pub fn data(&self) -> &[f32] {
        &self.data
    }
    
    /// Get mutable tensor data
    pub fn data_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }
    
    /// Matrix multiplication (2D tensors only)
    pub fn matmul(&self, other: &Tensor) -> Result<Tensor, &'static str> {
        if self.shape.len() != 2 || other.shape.len() != 2 {
            return Err("Matmul only supports 2D tensors");
        }
        
        let m = self.shape[0];
        let k = self.shape[1];
        let n = other.shape[1];
        
        if k != other.shape[0] {
            return Err("Incompatible dimensions for matmul");
        }
        
        let mut result = Tensor::zeros(&[m, n]);
        
        for i in 0..m {
            for j in 0..n {
                let mut sum = 0.0;
                for p in 0..k {
                    sum += self.data[i * k + p] * other.data[p * n + j];
                }
                result.data[i * n + j] = sum;
            }
        }
        
        Ok(result)
    }
    
    /// Element-wise addition
    pub fn add(&self, other: &Tensor) -> Result<Tensor, &'static str> {
        if self.shape != other.shape {
            return Err("Shape mismatch for addition");
        }
        
        let data: Vec<f32> = self.data.iter()
            .zip(other.data.iter())
            .map(|(a, b)| a + b)
            .collect();
        
        Ok(Tensor::from_slice(&data, &self.shape))
    }
    
    /// Element-wise multiplication
    pub fn mul(&self, other: &Tensor) -> Result<Tensor, &'static str> {
        if self.shape != other.shape {
            return Err("Shape mismatch for multiplication");
        }
        
        let data: Vec<f32> = self.data.iter()
            .zip(other.data.iter())
            .map(|(a, b)| a * b)
            .collect();
        
        Ok(Tensor::from_slice(&data, &self.shape))
    }
    
    /// Apply ReLU activation
    pub fn relu(&mut self) {
        for x in &mut self.data {
            *x = x.max(0.0);
        }
    }
    
    /// Apply softmax (last dimension)
    pub fn softmax(&mut self) {
        if self.shape.is_empty() {
            return;
        }
        
        let last_dim = *self.shape.last().unwrap();
        let n_groups = self.data.len() / last_dim;
        
        for i in 0..n_groups {
            let start = i * last_dim;
            let end = start + last_dim;
            let slice = &mut self.data[start..end];
            
            // Find max for numerical stability
            let max_val = slice.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
            
            // Compute exp and sum
            let mut sum = 0.0;
            for x in slice.iter_mut() {
                *x = libm::expf(*x - max_val);
                sum += *x;
            }
            
            // Normalize
            for x in slice {
                *x /= sum;
            }
        }
    }
    
    /// Layer normalization
    pub fn layer_norm(&mut self, eps: f32) {
        if self.shape.is_empty() {
            return;
        }
        
        let last_dim = *self.shape.last().unwrap();
        let n_groups = self.data.len() / last_dim;
        
        for i in 0..n_groups {
            let start = i * last_dim;
            let end = start + last_dim;
            let slice = &mut self.data[start..end];
            
            // Compute mean
            let mean: f32 = slice.iter().sum::<f32>() / last_dim as f32;
            
            // Compute variance
            let variance: f32 = slice.iter()
                .map(|x| (x - mean).powi(2))
                .sum::<f32>() / last_dim as f32;
            
            // Normalize
            let std_dev = libm::sqrtf(variance + eps);
            for x in slice {
                *x = (*x - mean) / std_dev;
            }
        }
    }
}
