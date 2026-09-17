use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Mutex, Semaphore, mpsc};

use zb_core::Error;
use zb_store::BlobCache;

use super::single::Downloader;
use super::{DownloadProgressCallback, DownloadResult, GLOBAL_DOWNLOAD_CONCURRENCY};

pub struct DownloadRequest {
    pub url: String,
    pub sha256: String,
    pub name: String,
}

type InflightMap = HashMap<String, Arc<tokio::sync::broadcast::Sender<Result<PathBuf, String>>>>;

pub struct ParallelDownloader {
    downloader: Arc<Downloader>,
    semaphore: Arc<Semaphore>,
    inflight: Arc<Mutex<InflightMap>>,
}

impl ParallelDownloader {
    pub fn new(blob_cache: BlobCache) -> Self {
        let semaphore = Arc::new(Semaphore::new(GLOBAL_DOWNLOAD_CONCURRENCY));
        Self {
            downloader: Arc::new(Downloader::with_semaphore(
                blob_cache,
                Some(semaphore.clone()),
            )),
            semaphore,
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_concurrency(blob_cache: BlobCache, concurrency: usize) -> Self {
        let semaphore = Arc::new(Semaphore::new(concurrency));
        Self {
            downloader: Arc::new(Downloader::with_semaphore(
                blob_cache,
                Some(semaphore.clone()),
            )),
            semaphore,
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn remove_blob(&self, sha256: &str) -> bool {
        self.downloader.remove_blob(sha256)
    }

    pub async fn download_single(
        &self,
        request: DownloadRequest,
        progress: Option<DownloadProgressCallback>,
    ) -> Result<PathBuf, Error> {
        Self::download_with_dedup(
            self.downloader.clone(),
            self.semaphore.clone(),
            self.inflight.clone(),
            request,
            progress,
        )
        .await
    }

    /// Download every request concurrently, yielding each outcome as it lands.
    ///
    /// The index of the originating request is reported for failures too, not
    /// just successes, so a caller can name the package a download error
    /// belongs to.
    pub fn download_streaming(
        &self,
        requests: Vec<DownloadRequest>,
        progress: Option<DownloadProgressCallback>,
    ) -> mpsc::Receiver<(usize, Result<DownloadResult, Error>)> {
        let (tx, rx) = mpsc::channel(requests.len().max(1));

        for (index, req) in requests.into_iter().enumerate() {
            let downloader = self.downloader.clone();
            let semaphore = self.semaphore.clone();
            let inflight = self.inflight.clone();
            let progress = progress.clone();
            let tx = tx.clone();

            tokio::spawn(async move {
                let result =
                    Self::download_with_dedup(downloader, semaphore, inflight, req, progress).await;
                let _ = tx
                    .send((index, result.map(|blob_path| DownloadResult { blob_path })))
                    .await;
            });
        }

        rx
    }

    async fn download_with_dedup(
        downloader: Arc<Downloader>,
        semaphore: Arc<Semaphore>,
        inflight: Arc<Mutex<InflightMap>>,
        req: DownloadRequest,
        progress: Option<DownloadProgressCallback>,
    ) -> Result<PathBuf, Error> {
        let mut receiver = {
            let mut map = inflight.lock().await;

            if let Some(sender) = map.get(&req.sha256) {
                Some(sender.subscribe())
            } else {
                let (tx, _) = tokio::sync::broadcast::channel(1);
                map.insert(req.sha256.clone(), Arc::new(tx));
                None
            }
        };

        if let Some(ref mut rx) = receiver {
            let result = rx
                .recv()
                .await
                .map_err(Error::network("broadcast recv error"))?;

            return result.map_err(|msg| Error::NetworkFailure { message: msg });
        }

        let _permit = semaphore
            .acquire()
            .await
            .map_err(Error::network("semaphore error"))?;

        let result = downloader
            .download_with_progress(&req.url, &req.sha256, Some(req.name), progress)
            .await;

        {
            let mut map = inflight.lock().await;
            if let Some(sender) = map.remove(&req.sha256) {
                let broadcast_result = match &result {
                    Ok(path) => Ok(path.clone()),
                    Err(e) => Err(e.to_string()),
                };
                let _ = sender.send(broadcast_result);
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use zb_store::BlobCache;

    use super::super::GLOBAL_DOWNLOAD_CONCURRENCY;
    use super::*;

    #[tokio::test]
    async fn peak_concurrent_downloads_within_limit() {
        let mock_server = MockServer::start().await;
        let concurrent_count = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        let content = b"test content";
        let count_clone = concurrent_count.clone();
        let max_clone = max_concurrent.clone();

        Mock::given(method("GET"))
            .respond_with(move |_: &wiremock::Request| {
                let current = count_clone.fetch_add(1, Ordering::SeqCst) + 1;
                max_clone.fetch_max(current, Ordering::SeqCst);

                std::thread::sleep(Duration::from_millis(50));

                count_clone.fetch_sub(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_bytes(content.to_vec())
            })
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = ParallelDownloader::new(blob_cache);

        let requests: Vec<_> = (0..5)
            .map(|i| {
                let sha256 = format!("{:064x}", i);
                DownloadRequest {
                    url: format!("{}/file{i}.tar.gz", mock_server.uri()),
                    sha256,
                    name: format!("pkg{i}"),
                }
            })
            .collect();

        let mut rx = downloader.download_streaming(requests, None);
        while rx.recv().await.is_some() {}

        let peak = max_concurrent.load(Ordering::SeqCst);
        assert!(
            peak <= GLOBAL_DOWNLOAD_CONCURRENCY,
            "peak concurrent downloads was {peak}, expected <= {GLOBAL_DOWNLOAD_CONCURRENCY}"
        );
    }

    #[tokio::test]
    async fn same_blob_requested_multiple_times_fetches_once() {
        let mock_server = MockServer::start().await;
        let content = b"deduplicated content";

        let actual_sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(content);
            zb_core::checksum::sha256_hex(hasher)
        };

        Mock::given(method("GET"))
            .and(path("/dedup.tar.gz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(content.to_vec())
                    .set_delay(Duration::from_millis(100)),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = ParallelDownloader::new(blob_cache);

        let requests: Vec<_> = (0..5)
            .map(|i| DownloadRequest {
                url: format!("{}/dedup.tar.gz", mock_server.uri()),
                sha256: actual_sha256.clone(),
                name: format!("dedup{i}"),
            })
            .collect();

        let mut rx = downloader.download_streaming(requests, None);
        let mut results = Vec::new();
        while let Some((_, result)) = rx.recv().await {
            results.push(result.unwrap());
        }

        assert_eq!(results.len(), 5);
        for result in &results {
            assert!(result.blob_path.exists());
        }
    }
}
