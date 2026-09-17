mod auth;
mod chunked;
mod parallel;
mod single;

use std::path::PathBuf;
use std::sync::Arc;

use crate::progress::InstallProgress;

pub(crate) type DownloadProgressCallback = Arc<dyn Fn(InstallProgress) + Send + Sync>;

const RACING_CONNECTIONS: usize = 3;
const RACING_STAGGER_MS: u64 = 200;

/// Minimum file size to use chunked downloads (10MB)
const CHUNKED_DOWNLOAD_THRESHOLD: u64 = 10 * 1024 * 1024;

/// Global download concurrency limit
/// Total number of concurrent connections across all downloads to avoid
/// overwhelming servers and the local network. Based on industry best practices
/// (npm uses 20-50, we use a conservative 20 for HTTP/1.1 compatibility).
const GLOBAL_DOWNLOAD_CONCURRENCY: usize = 20;

/// Maximum concurrent chunk downloads per file
/// Chosen to divide GLOBAL_DOWNLOAD_CONCURRENCY among multiple large file downloads.
/// With 20 global concurrency, we can have 3-4 large files downloading concurrently.
const MAX_CONCURRENT_CHUNKS: usize = 6;

/// Maximum retry attempts for failed chunk downloads
const MAX_CHUNK_RETRIES: u32 = 3;

#[derive(Debug, Clone)]
pub(crate) struct DownloadResult {
    pub blob_path: PathBuf,
    /// Position of the originating request in the batch handed to
    /// `download_streaming`, so the caller can pair a result with its item.
    pub index: usize,
}

pub(crate) use parallel::{DownloadRequest, ParallelDownloader};
