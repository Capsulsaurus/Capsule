//! Upload client module for Capsule SDK
//!
//! This module provides chunked upload functionality with adaptive chunk sizing
//! based on network throughput measurements.

// TODO: Use this module in SDK

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use crate::AuthenticatedClient;

// Constants for chunk sizes (4KB aligned)
const KB: u64 = 1024;
const CHUNK_SIZE_256KB: u64 = 256 * KB;
const CHUNK_SIZE_1MB: u64 = 1024 * KB;
const CHUNK_SIZE_4MB: u64 = 4 * 1024 * KB;
const CHUNK_SIZE_16MB: u64 = 16 * 1024 * KB;

/// All chunks MUST be multiples of 4KB (4096 bytes)
const ALIGNMENT: u64 = 4096;
/// Window size for throughput measurements
const THROUGHPUT_WINDOW_SECS: f64 = 30.0;
/// Minimum bytes before scaling chunk size
const MIN_BYTES_BEFORE_SCALING: u64 = 8 * 1024 * 1024; // 8MB
/// Minimum chunks before scaling chunk size
const MIN_CHUNKS_BEFORE_SCALING: u32 = 5;
/// Maximum number of recent chunks to keep in history
const MAX_RECENT_CHUNKS: usize = 1000;

/// Chunk size strategy for adaptive upload
/// It is recommended
#[derive(Debug, Clone)]
pub struct AdaptiveChunkSizeStrategy {
    /// Current chunk size
    pub current_size: u64,
    /// Minimum allowed chunk size based on file size tier
    pub min_size: u64,
    /// Maximum allowed chunk size based on file size tier
    pub max_size: u64,
    /// Number of successful chunks at current size
    pub successful_chunks: u32,
    /// Total bytes uploaded
    pub bytes_uploaded: u64,
    /// Total time spent uploading
    pub total_upload_time: Duration,
    /// Recent chunk records for windowed throughput (bytes, duration, timestamp)
    /// Using VecDeque for efficient sliding window
    recent_chunks: VecDeque<(u64, Duration, std::time::Instant)>,
    // TODO: ^^ there is a chance the adaptive logic needs to be redone as it may very will slow down uploads unnecessarily.
}

impl AdaptiveChunkSizeStrategy {
    /// Create a new chunk size strategy based on total file size
    pub fn for_file_size(total_size: u64) -> Self {
        let (min_size, max_size, current_size) = if total_size < 10 * 1024 * KB {
            // < 10MB: 256KB - 1MB chunks
            (CHUNK_SIZE_256KB, CHUNK_SIZE_1MB, CHUNK_SIZE_256KB)
        } else if total_size < 100 * 1024 * KB {
            // < 100MB: 1MB - 4MB chunks
            (CHUNK_SIZE_1MB, CHUNK_SIZE_4MB, CHUNK_SIZE_1MB)
        } else {
            // >= 100MB: 4MB - 16MB chunks
            (CHUNK_SIZE_4MB, CHUNK_SIZE_16MB, CHUNK_SIZE_4MB)
        };

        Self {
            current_size,
            min_size,
            max_size,
            successful_chunks: 0,
            bytes_uploaded: 0,
            total_upload_time: Duration::ZERO,
            recent_chunks: VecDeque::with_capacity(32),
        }
    }

    /// Calculate current throughput in bytes per second (windowed)
    pub fn throughput_bytes_per_second(&self) -> f64 {
        let now = std::time::Instant::now();
        let window_start = now
            .checked_sub(Duration::from_secs_f64(THROUGHPUT_WINDOW_SECS))
            .unwrap_or(now);

        let (bytes, time) = self
            .recent_chunks
            .iter()
            .filter(|(_, _, timestamp)| *timestamp >= window_start)
            .fold((0u64, Duration::ZERO), |(acc_b, acc_t), (b, t, _)| {
                (acc_b + b, acc_t + *t)
            });

        if time.as_secs_f64() == 0.0 {
            return 0.0;
        }
        bytes as f64 / time.as_secs_f64()
    }

    /// Record a successful chunk upload and potentially adjust chunk size
    pub fn record_chunk(&mut self, chunk_size: u64, upload_duration: Duration) {
        self.successful_chunks += 1;
        self.bytes_uploaded += chunk_size;
        self.total_upload_time += upload_duration;

        // Record for windowed throughput
        let now = std::time::Instant::now();
        self.recent_chunks
            .push_back((chunk_size, upload_duration, now));

        // Enforce max size (circular buffer behavior)
        if self.recent_chunks.len() > MAX_RECENT_CHUNKS {
            self.recent_chunks.pop_front();
        }

        // Cleanup old chunks (older than window)
        // Since it's sorted by time, we can just pop from front until we see a new enough one
        if let Some(window_start) = now.checked_sub(Duration::from_secs_f64(THROUGHPUT_WINDOW_SECS))
        {
            while let Some((_, _, timestamp)) = self.recent_chunks.front() {
                if *timestamp < window_start {
                    self.recent_chunks.pop_front();
                } else {
                    break;
                }
            }
        }

        // Adaptive scaling based on throughput
        if self.bytes_uploaded >= MIN_BYTES_BEFORE_SCALING
            || self.successful_chunks >= MIN_CHUNKS_BEFORE_SCALING
        {
            let throughput = self.throughput_bytes_per_second();

            // If throughput is high (> 5 MB/s) and we're not at max, double chunk size
            if throughput > 5.0 * 1024.0 * 1024.0 && self.current_size < self.max_size {
                self.current_size = (self.current_size * 2).min(self.max_size);
                self.successful_chunks = 0; // Reset counter after scaling
            }
            // If throughput is low (< 1 MB/s) and we're not at min, halve chunk size
            else if throughput < 1.0 * 1024.0 * 1024.0 && self.current_size > self.min_size {
                self.current_size = (self.current_size / 2).max(self.min_size);
                self.successful_chunks = 0;
            }
        }
    }

    /// Get the next chunk size to use
    pub fn next_chunk_size(&self) -> u64 {
        debug_assert!(
            self.current_size.is_multiple_of(ALIGNMENT),
            "Chunk size must be 4KB aligned"
        );
        self.current_size
    }
}

/// Upload client for managing chunked uploads to Capsule API
pub struct UploadClient {
    /// Capsule API Client
    client: AuthenticatedClient,
}

impl UploadClient {
    /// Create a new upload client
    pub fn new(client: AuthenticatedClient) -> Self {
        Self { client }
    }

    /// Upload a file using adaptive chunked upload
    ///
    /// # Arguments
    /// * `file_path` - Path to the file to upload
    /// * `filename` - Optional filename to use (defaults to file name from path)
    /// * `content_type` - Optional content type (defaults to auto-detection)
    ///
    /// # Returns
    /// The upload session ID on success
    pub async fn upload_file(
        &self,
        _file_path: &Path,
        _filename: Option<&str>,
        _content_type: Option<&str>,
    ) -> Result<String, UploadError> {
        // TODO: Implement file upload
        // 1. Get file size and metadata
        // 2. Create upload session via API
        // 3. Create AdaptiveChunkSizeStrategy based on file size
        // 4. Upload chunks with adaptive sizing
        // 5. Return session ID on completion
        todo!("Upload file not yet implemented - waiting for API client generation")
    }

    /// Create an upload session
    pub async fn create_session(
        &self,
        _total_size: u64,
        _filename: Option<&str>,
        _content_type: Option<&str>,
    ) -> Result<CreateSessionResponse, UploadError> {
        // TODO: Call POST /upload with X-Capsule-Content-Length header
        todo!("Create session not yet implemented")
    }

    /// Upload a single chunk
    pub async fn upload_chunk(
        &self,
        _session_id: &str,
        _data: &[u8],
        _offset: u64,
    ) -> Result<u64, UploadError> {
        // TODO: Call PATCH /upload/{id} with X-Capsule-Offset header
        // Returns the new offset on success
        todo!("Upload chunk not yet implemented")
    }
}

/// Response from creating an upload session
#[derive(Debug)]
pub struct CreateSessionResponse {
    /// Upload session ID
    pub id: String,
    /// Upload URL
    pub upload_url: String,
    /// Suggested chunk size from server
    pub suggested_chunk_size: u64,
}

/// Upload client errors
#[derive(Debug)]
pub enum UploadError {
    /// IO error reading file
    IoError(std::io::Error),
    /// Network error
    NetworkError(String),
    /// Server returned an error
    ServerError { status: u16, message: String },
    /// Checksum mismatch
    ChecksumMismatch,
    /// Session not found or expired
    SessionNotFound,
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadError::IoError(e) => write!(f, "IO error: {}", e),
            UploadError::NetworkError(msg) => write!(f, "Network error: {}", msg),
            UploadError::ServerError { status, message } => {
                write!(f, "Server error ({}): {}", status, message)
            }
            UploadError::ChecksumMismatch => write!(f, "Checksum mismatch"),
            UploadError::SessionNotFound => write!(f, "Session not found or expired"),
        }
    }
}

impl std::error::Error for UploadError {}

impl From<std::io::Error> for UploadError {
    fn from(err: std::io::Error) -> Self {
        UploadError::IoError(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_size_strategy_small_file() {
        let strategy = AdaptiveChunkSizeStrategy::for_file_size(5 * 1024 * 1024); // 5MB
        assert_eq!(strategy.current_size, CHUNK_SIZE_256KB);
        assert_eq!(strategy.min_size, CHUNK_SIZE_256KB);
        assert_eq!(strategy.max_size, CHUNK_SIZE_1MB);
    }

    #[test]
    fn test_chunk_size_strategy_medium_file() {
        let strategy = AdaptiveChunkSizeStrategy::for_file_size(50 * 1024 * 1024); // 50MB
        assert_eq!(strategy.current_size, CHUNK_SIZE_1MB);
        assert_eq!(strategy.min_size, CHUNK_SIZE_1MB);
        assert_eq!(strategy.max_size, CHUNK_SIZE_4MB);
    }

    #[test]
    fn test_chunk_size_strategy_large_file() {
        let strategy = AdaptiveChunkSizeStrategy::for_file_size(200 * 1024 * 1024); // 200MB
        assert_eq!(strategy.current_size, CHUNK_SIZE_4MB);
        assert_eq!(strategy.min_size, CHUNK_SIZE_4MB);
        assert_eq!(strategy.max_size, CHUNK_SIZE_16MB);
    }

    #[test]
    fn test_adaptive_scaling_up() {
        let mut strategy = AdaptiveChunkSizeStrategy::for_file_size(50 * 1024 * 1024);
        // Simulate 3 fast uploads (high throughput)
        for _ in 0..3 {
            strategy.record_chunk(CHUNK_SIZE_1MB, Duration::from_millis(100));
        }
        // After 3 fast chunks, should scale up
        assert!(strategy.current_size > CHUNK_SIZE_1MB);
    }
}
