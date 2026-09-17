use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT_RANGES, AUTHORIZATION, CONTENT_RANGE};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Semaphore, mpsc};
use zb_core::Error;
use zb_core::progress::InstallProgress;
use zb_store::{BlobCache, BlobWriter};

use super::auth::{
    TokenCache, bearer_header, fetch_bearer_token_internal, fetch_download_response_internal,
    fetch_range_response_internal, get_cached_token_for_url_internal,
};
use super::single::download_response_internal;
use super::{DownloadProgressCallback, MAX_CHUNK_RETRIES, MAX_CONCURRENT_CHUNKS};

const MIN_CHUNK_SIZE: u64 = 5 * 1024 * 1024;
const MAX_CHUNK_SIZE: u64 = 20 * 1024 * 1024;

/// Buffer used to stream the finished blob back through the hasher.
const HASH_READ_BUFFER_SIZE: usize = 1024 * 1024;

struct ChunkDownloadContext<'a> {
    client: &'a reqwest::Client,
    token_cache: &'a TokenCache,
    url: &'a str,
    progress: Option<DownloadProgressCallback>,
    name: Option<String>,
    file_size: u64,
    total_downloaded: Arc<AtomicU64>,
}

pub(crate) struct ChunkedDownloadContext<'a> {
    pub(crate) blob_cache: &'a BlobCache,
    pub(crate) client: &'a reqwest::Client,
    pub(crate) token_cache: &'a TokenCache,
    pub(crate) url: &'a str,
    pub(crate) expected_sha256: &'a str,
    pub(crate) name: Option<String>,
    pub(crate) progress: Option<DownloadProgressCallback>,
    pub(crate) file_size: u64,
    pub(crate) global_semaphore: &'a Arc<Semaphore>,
}

struct ChunkRange {
    offset: u64,
    size: u64,
}

pub(crate) fn server_supports_ranges(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "bytes")
        .unwrap_or(false)
}

fn calculate_chunk_size(file_size: u64) -> u64 {
    let target_chunks = MAX_CONCURRENT_CHUNKS as u64;
    let chunk_size = file_size / target_chunks;
    chunk_size.clamp(MIN_CHUNK_SIZE, MAX_CHUNK_SIZE)
}

fn calculate_chunk_ranges(file_size: u64) -> Vec<ChunkRange> {
    let chunk_size = calculate_chunk_size(file_size);
    let mut chunks = Vec::new();
    let mut offset = 0;

    while offset < file_size {
        let remaining = file_size - offset;
        let chunk_size = remaining.min(chunk_size);
        chunks.push(ChunkRange {
            offset,
            size: chunk_size,
        });
        offset += chunk_size;
    }

    chunks
}

async fn download_chunk(
    ctx: &ChunkDownloadContext<'_>,
    chunk: &ChunkRange,
) -> Result<Vec<u8>, Error> {
    let range_header = format!("bytes={}-{}", chunk.offset, chunk.offset + chunk.size - 1);

    let mut last_error = None;

    for attempt in 0..=MAX_CHUNK_RETRIES {
        let cached_token = get_cached_token_for_url_internal(ctx.token_cache, ctx.url).await;

        let mut request = ctx
            .client
            .get(ctx.url)
            .header("Range", range_header.clone());
        if let Some(token) = &cached_token {
            request = request.header(AUTHORIZATION, bearer_header(token)?);
        }

        match request.send().await {
            Ok(response) => {
                if response.status() == StatusCode::UNAUTHORIZED {
                    let www_auth = match response.headers().get(reqwest::header::WWW_AUTHENTICATE) {
                        Some(value) => value.to_str().map_err(|_| Error::NetworkFailure {
                            message: "WWW-Authenticate header contains invalid characters"
                                .to_string(),
                        })?,
                        None => {
                            return Err(Error::NetworkFailure {
                                message: "server returned 401 without WWW-Authenticate header"
                                    .to_string(),
                            });
                        }
                    };

                    match fetch_bearer_token_internal(ctx.client, ctx.token_cache, www_auth).await {
                        Ok(_new_token) => {
                            last_error = Some(Error::NetworkFailure {
                                message: "token expired, retrying with new token".to_string(),
                            });
                            continue;
                        }
                        Err(e) => {
                            return Err(Error::network("failed to refresh token")(e));
                        }
                    }
                }

                if let Some(content_range) = response.headers().get(CONTENT_RANGE) {
                    let range_str = content_range.to_str().unwrap_or("");
                    if !range_str.contains(&format!(
                        "{}-{}",
                        chunk.offset,
                        chunk.offset + chunk.size - 1
                    )) {
                        return Err(Error::NetworkFailure {
                            message: format!(
                                "invalid content-range: expected bytes {}-{}, got: {}",
                                chunk.offset,
                                chunk.offset + chunk.size - 1,
                                range_str
                            ),
                        });
                    }
                }

                if !response.status().is_success() {
                    let err = Error::NetworkFailure {
                        message: format!("chunk download returned HTTP {}", response.status()),
                    };

                    if response.status().is_server_error() && attempt < MAX_CHUNK_RETRIES {
                        last_error = Some(err);
                        tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
                        continue;
                    }
                    return Err(err);
                }

                let mut chunk_data = Vec::with_capacity(chunk.size as usize);
                let mut stream = response.bytes_stream();

                while let Some(item) = stream.next().await {
                    let bytes = item.map_err(Error::network("failed to read chunk bytes"))?;

                    chunk_data.extend_from_slice(&bytes);

                    if let (Some(cb), Some(n)) = (&ctx.progress, &ctx.name) {
                        let downloaded = ctx
                            .total_downloaded
                            .fetch_add(bytes.len() as u64, Ordering::Release);
                        cb(InstallProgress::DownloadProgress {
                            name: n.clone(),
                            downloaded: downloaded + bytes.len() as u64,
                            total_bytes: Some(ctx.file_size),
                        });
                    }
                }

                if chunk_data.len() != chunk.size as usize {
                    return Err(Error::NetworkFailure {
                        message: format!(
                            "chunk size mismatch: expected {} bytes, got {} bytes",
                            chunk.size,
                            chunk_data.len()
                        ),
                    });
                }

                return Ok(chunk_data);
            }
            Err(e) => {
                last_error = Some(Error::network("chunk download failed")(e));

                if attempt < MAX_CHUNK_RETRIES {
                    tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
                    continue;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| Error::NetworkFailure {
        message: "chunk download failed after retries".to_string(),
    }))
}

pub(crate) async fn download_with_chunks(
    ctx: &ChunkedDownloadContext<'_>,
) -> Result<PathBuf, Error> {
    if !validate_range_support(ctx).await? {
        let response =
            fetch_download_response_internal(ctx.client, ctx.token_cache, ctx.url).await?;
        return download_response_internal(
            ctx.blob_cache,
            response,
            ctx.expected_sha256,
            ctx.name.clone(),
            ctx.progress.clone(),
        )
        .await;
    }

    let chunks = calculate_chunk_ranges(ctx.file_size);

    if let (Some(cb), Some(n)) = (&ctx.progress, &ctx.name) {
        cb(InstallProgress::DownloadStarted {
            name: n.clone(),
            total_bytes: Some(ctx.file_size),
        });
    }

    let writer = ctx
        .blob_cache
        .start_write(ctx.expected_sha256)
        .map_err(Error::network("failed to create blob writer"))?;

    let expected_chunks: BTreeMap<u64, u64> = chunks.iter().map(|c| (c.offset, c.size)).collect();
    let total_chunks = chunks.len();

    // Only chunk metadata (offset, length) travels over this channel. The chunk
    // bytes themselves are written straight to the blob file and then hashed by
    // reading that file back, so peak memory stays at a few chunks rather than
    // the whole download.
    let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel::<(u64, u64)>();

    let total_downloaded = Arc::new(AtomicU64::new(0));

    let writer = Arc::new(Mutex::new(writer));

    let mut handles = Vec::new();
    for chunk in chunks {
        let client = ctx.client.clone();
        let token_cache = ctx.token_cache.clone();
        let url = ctx.url.to_string();
        let global_semaphore = ctx.global_semaphore.clone();
        let total_downloaded = total_downloaded.clone();
        let progress = ctx.progress.clone();
        let name = ctx.name.clone();
        let chunk_tx = chunk_tx.clone();
        let file_size = ctx.file_size;
        let writer = writer.clone();

        let handle = tokio::spawn(async move {
            let _permit = global_semaphore
                .acquire()
                .await
                .map_err(Error::network("global semaphore error"))?;

            let chunk_ctx = ChunkDownloadContext {
                client: &client,
                token_cache: &token_cache,
                url: &url,
                progress: progress.clone(),
                name: name.clone(),
                file_size,
                total_downloaded: total_downloaded.clone(),
            };

            let chunk_data = download_chunk(&chunk_ctx, &chunk).await?;

            {
                let mut writer = writer.lock().await;
                writer
                    .seek(std::io::SeekFrom::Start(chunk.offset))
                    .map_err(|e| Error::NetworkFailure {
                        message: format!("failed to seek to offset {}: {e}", chunk.offset),
                    })?;
                writer
                    .write_all(&chunk_data)
                    .map_err(|e| Error::NetworkFailure {
                        message: format!("failed to write chunk at offset {}: {e}", chunk.offset),
                    })?;
            }

            chunk_tx
                .send((chunk.offset, chunk_data.len() as u64))
                .map_err(Error::network("failed to send chunk metadata"))?;

            Ok::<(), Error>(())
        });

        handles.push(handle);
    }

    drop(chunk_tx);

    let mut received_chunks: BTreeMap<u64, u64> = BTreeMap::new();
    let mut chunks_written = 0u64;

    while let Some((offset, len)) = chunk_rx.recv().await {
        let expected_size = expected_chunks
            .get(&offset)
            .ok_or_else(|| Error::NetworkFailure {
                message: format!("received unexpected chunk at offset {}", offset),
            })?;

        if len != *expected_size {
            return Err(Error::NetworkFailure {
                message: format!(
                    "chunk size mismatch at offset {}: expected {} bytes, got {} bytes",
                    offset, expected_size, len
                ),
            });
        }

        received_chunks.insert(offset, len);
        chunks_written += 1;
    }

    for handle in handles {
        handle
            .await
            .map_err(Error::network("chunk download task failed"))??;
    }

    if chunks_written as usize != total_chunks {
        return Err(Error::NetworkFailure {
            message: format!(
                "expected {} chunks, received {}",
                total_chunks, chunks_written
            ),
        });
    }

    let mut total_size = 0u64;
    for (offset, len) in received_chunks {
        if offset != total_size {
            return Err(Error::NetworkFailure {
                message: format!(
                    "chunk gap detected: expected offset {}, got {}",
                    total_size, offset
                ),
            });
        }
        total_size += len;
    }

    if total_size != ctx.file_size {
        return Err(Error::NetworkFailure {
            message: format!(
                "incomplete write: expected {} bytes, wrote {} bytes",
                ctx.file_size, total_size
            ),
        });
    }

    // All chunk tasks have been joined above, so this is the only remaining
    // reference to the writer.
    let mut writer = Arc::try_unwrap(writer)
        .map_err(|_| Error::NetworkFailure {
            message: "failed to unwrap writer Arc".to_string(),
        })?
        .into_inner();

    writer
        .flush()
        .map_err(Error::network("failed to flush download"))?;

    let actual_hash = hash_written_blob(&mut writer, ctx.file_size)?;

    if actual_hash != ctx.expected_sha256 {
        return Err(Error::ChecksumMismatch {
            expected: ctx.expected_sha256.to_string(),
            actual: actual_hash,
        });
    }

    if let (Some(cb), Some(n)) = (&ctx.progress, &ctx.name) {
        cb(InstallProgress::DownloadCompleted {
            name: n.clone(),
            total_bytes: ctx.file_size,
        });
    }

    writer.commit()
}

/// Hash the bytes already written to the blob file.
///
/// The chunks were streamed to disk as they arrived, so re-reading the file is
/// what lets us verify the digest without holding the whole download in memory.
/// Reading also confirms the bytes actually landed at the expected offsets,
/// which the per-chunk bookkeeping alone cannot prove.
fn hash_written_blob(writer: &mut BlobWriter, expected_size: u64) -> Result<String, Error> {
    writer
        .seek(std::io::SeekFrom::Start(0))
        .map_err(Error::network("failed to rewind blob for hashing"))?;

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_READ_BUFFER_SIZE];
    let mut hashed = 0u64;

    loop {
        let n = writer
            .read(&mut buf)
            .map_err(Error::network("failed to read blob for hashing"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        hashed += n as u64;
    }

    if hashed != expected_size {
        return Err(Error::NetworkFailure {
            message: format!(
                "incomplete write: expected {} bytes on disk, found {} bytes",
                expected_size, hashed
            ),
        });
    }

    Ok(zb_core::checksum::sha256_hex(hasher))
}

async fn validate_range_support(ctx: &ChunkedDownloadContext<'_>) -> Result<bool, Error> {
    let response =
        fetch_range_response_internal(ctx.client, ctx.token_cache, ctx.url, "bytes=0-0").await?;

    if response.status() != StatusCode::PARTIAL_CONTENT {
        return Ok(false);
    }

    let content_range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    Ok(content_range.contains("0-0"))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use zb_store::BlobCache;

    use super::super::single::Downloader;
    use super::MAX_CONCURRENT_CHUNKS;
    use std::sync::Arc;

    #[tokio::test]
    async fn chunked_download_for_large_files() {
        let mock_server = MockServer::start().await;

        let large_content = vec![0xABu8; 15 * 1024 * 1024];
        let actual_sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&large_content);
            zb_core::checksum::sha256_hex(hasher)
        };

        Mock::given(method("HEAD"))
            .and(path("/large.tar.gz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Accept-Ranges", "bytes")
                    .append_header("Content-Length", large_content.len().to_string()),
            )
            .mount(&mock_server)
            .await;

        let range_requests = Arc::new(AtomicUsize::new(0));
        let range_requests_clone = range_requests.clone();
        let large_content_for_closure = large_content.clone();

        Mock::given(method("GET"))
            .and(path("/large.tar.gz"))
            .respond_with(move |req: &wiremock::Request| {
                if let Some(range_header) = req.headers.get("Range") {
                    range_requests_clone.fetch_add(1, Ordering::SeqCst);

                    let range_str = range_header.to_str().unwrap();
                    let range_part = range_str.strip_prefix("bytes=").unwrap();
                    let (start_str, end_str) = range_part.split_once('-').unwrap();
                    let start: usize = start_str.parse().unwrap();
                    let end: usize = end_str.parse().unwrap();

                    let chunk = &large_content_for_closure[start..=end];
                    ResponseTemplate::new(206)
                        .append_header("Content-Length", chunk.len().to_string())
                        .append_header(
                            "Content-Range",
                            format!(
                                "bytes {}-{}/{}",
                                start,
                                end,
                                large_content_for_closure.len()
                            ),
                        )
                        .set_body_bytes(chunk.to_vec())
                } else {
                    ResponseTemplate::new(200).set_body_bytes(large_content_for_closure.clone())
                }
            })
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = Downloader::with_semaphore(blob_cache, None);

        let url = format!("{}/large.tar.gz", mock_server.uri());
        let result = downloader
            .download_with_progress(&url, &actual_sha256, None, None)
            .await;

        assert!(result.is_ok(), "Download failed: {:?}", result.err());
        let blob_path = result.unwrap();
        assert!(blob_path.exists());

        let range_count = range_requests.load(Ordering::SeqCst);
        assert!(
            range_count > 0,
            "Expected multiple Range requests, got {}",
            range_count
        );

        let downloaded_content = std::fs::read(&blob_path).unwrap();
        assert_eq!(downloaded_content.len(), large_content.len());
        assert_eq!(downloaded_content, large_content);
    }

    /// The digest is computed by reading the assembled blob back off disk, so a
    /// server that serves well-formed, correctly-sized chunks whose bytes do not
    /// match the expected hash must still be rejected, and no blob left behind.
    #[tokio::test]
    async fn chunked_download_rejects_checksum_mismatch() {
        let mock_server = MockServer::start().await;

        let large_content = vec![0xABu8; 15 * 1024 * 1024];
        let wrong_sha256 = "0".repeat(64);

        Mock::given(method("HEAD"))
            .and(path("/large.tar.gz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Accept-Ranges", "bytes")
                    .append_header("Content-Length", large_content.len().to_string()),
            )
            .mount(&mock_server)
            .await;

        let body = large_content.clone();
        Mock::given(method("GET"))
            .and(path("/large.tar.gz"))
            .respond_with(move |req: &wiremock::Request| {
                if let Some(range_header) = req.headers.get("Range") {
                    let range_str = range_header.to_str().unwrap();
                    let range_part = range_str.strip_prefix("bytes=").unwrap();
                    let (start_str, end_str) = range_part.split_once('-').unwrap();
                    let start: usize = start_str.parse().unwrap();
                    let end: usize = end_str.parse().unwrap();

                    let chunk = &body[start..=end];
                    ResponseTemplate::new(206)
                        .append_header("Content-Length", chunk.len().to_string())
                        .append_header(
                            "Content-Range",
                            format!("bytes {}-{}/{}", start, end, body.len()),
                        )
                        .set_body_bytes(chunk.to_vec())
                } else {
                    ResponseTemplate::new(200).set_body_bytes(body.clone())
                }
            })
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = Downloader::with_semaphore(blob_cache, None);

        let url = format!("{}/large.tar.gz", mock_server.uri());
        let result = downloader
            .download_with_progress(&url, &wrong_sha256, None, None)
            .await;

        assert!(
            result.is_err(),
            "expected a checksum mismatch to be rejected"
        );
        assert!(
            !tmp.path()
                .join("blobs")
                .join(format!("{wrong_sha256}.tar.gz"))
                .exists(),
            "a blob that failed verification must not be persisted"
        );
    }

    #[tokio::test]
    async fn fallback_to_normal_download_when_ranges_not_supported() {
        let mock_server = MockServer::start().await;

        let large_content = vec![0xCDu8; 15 * 1024 * 1024];
        let actual_sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&large_content);
            zb_core::checksum::sha256_hex(hasher)
        };

        Mock::given(method("HEAD"))
            .and(path("/large.tar.gz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Content-Length", large_content.len().to_string()),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/large.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(large_content.clone()))
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = Downloader::with_semaphore(blob_cache, None);

        let url = format!("{}/large.tar.gz", mock_server.uri());
        let result = downloader
            .download_with_progress(&url, &actual_sha256, None, None)
            .await;

        assert!(result.is_ok());
        let blob_path = result.unwrap();
        assert!(blob_path.exists());

        let downloaded_content = std::fs::read(&blob_path).unwrap();
        assert_eq!(downloaded_content, large_content);
    }

    #[tokio::test]
    async fn small_files_dont_use_chunked_download() {
        let mock_server = MockServer::start().await;

        let small_content = vec![0xEFu8; 1024 * 1024];
        let actual_sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&small_content);
            zb_core::checksum::sha256_hex(hasher)
        };

        Mock::given(method("HEAD"))
            .and(path("/small.tar.gz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Accept-Ranges", "bytes")
                    .append_header("Content-Length", small_content.len().to_string()),
            )
            .mount(&mock_server)
            .await;

        let range_used = Arc::new(AtomicUsize::new(0));
        let range_used_clone = range_used.clone();
        let small_content_for_closure = small_content.clone();

        Mock::given(method("GET"))
            .and(path("/small.tar.gz"))
            .respond_with(move |req: &wiremock::Request| {
                if req.headers.get("Range").is_some() {
                    range_used_clone.fetch_add(1, Ordering::SeqCst);
                }
                ResponseTemplate::new(200).set_body_bytes(small_content_for_closure.clone())
            })
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = Downloader::with_semaphore(blob_cache, None);

        let url = format!("{}/small.tar.gz", mock_server.uri());
        let result = downloader
            .download_with_progress(&url, &actual_sha256, None, None)
            .await;

        assert!(result.is_ok());
        let blob_path = result.unwrap();
        assert!(blob_path.exists());

        let range_count = range_used.load(Ordering::SeqCst);
        assert_eq!(
            range_count, 0,
            "Small files should not use chunked download"
        );

        let downloaded_content = std::fs::read(&blob_path).unwrap();
        assert_eq!(downloaded_content, small_content);
    }

    #[tokio::test]
    async fn chunked_download_respects_concurrency_limit() {
        let mock_server = MockServer::start().await;

        let large_content = vec![0xABu8; 40 * 1024 * 1024];
        let actual_sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&large_content);
            zb_core::checksum::sha256_hex(hasher)
        };

        Mock::given(method("HEAD"))
            .and(path("/large.tar.gz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Accept-Ranges", "bytes")
                    .append_header("Content-Length", large_content.len().to_string()),
            )
            .mount(&mock_server)
            .await;

        let concurrent_count = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let concurrent_clone = concurrent_count.clone();
        let max_clone = max_concurrent.clone();
        let large_content_for_closure = large_content.clone();

        Mock::given(method("GET"))
            .and(path("/large.tar.gz"))
            .respond_with(move |req: &wiremock::Request| {
                if let Some(range_header) = req.headers.get("Range") {
                    let current = concurrent_clone.fetch_add(1, Ordering::SeqCst) + 1;
                    max_clone.fetch_max(current, Ordering::SeqCst);

                    let range_str = range_header.to_str().unwrap();
                    let range_part = range_str.strip_prefix("bytes=").unwrap();
                    let (start_str, end_str) = range_part.split_once('-').unwrap();
                    let start: usize = start_str.parse().unwrap();
                    let end: usize = end_str.parse().unwrap();

                    std::thread::sleep(Duration::from_millis(50));

                    let chunk = &large_content_for_closure[start..=end];

                    concurrent_clone.fetch_sub(1, Ordering::SeqCst);

                    ResponseTemplate::new(206)
                        .append_header("Content-Length", chunk.len().to_string())
                        .append_header(
                            "Content-Range",
                            format!(
                                "bytes {}-{}/{}",
                                start,
                                end,
                                large_content_for_closure.len()
                            ),
                        )
                        .set_body_bytes(chunk.to_vec())
                } else {
                    ResponseTemplate::new(200).set_body_bytes(large_content_for_closure.clone())
                }
            })
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = Downloader::with_semaphore(blob_cache, None);

        let url = format!("{}/large.tar.gz", mock_server.uri());
        let result = downloader
            .download_with_progress(&url, &actual_sha256, None, None)
            .await;

        assert!(result.is_ok(), "Download failed: {:?}", result.err());
        let blob_path = result.unwrap();
        assert!(blob_path.exists());

        let peak = max_concurrent.load(Ordering::SeqCst);
        assert!(
            peak <= MAX_CONCURRENT_CHUNKS,
            "Peak concurrent downloads was {peak}, expected <= {MAX_CONCURRENT_CHUNKS}"
        );

        let downloaded_content = std::fs::read(&blob_path).unwrap();
        assert_eq!(downloaded_content.len(), large_content.len());
        assert_eq!(downloaded_content, large_content);
    }

    #[tokio::test]
    async fn chunk_retry_logic_succeeds_after_transient_failure() {
        let mock_server = MockServer::start().await;

        let large_content = vec![0xABu8; 15 * 1024 * 1024];
        let actual_sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&large_content);
            zb_core::checksum::sha256_hex(hasher)
        };

        Mock::given(method("HEAD"))
            .and(path("/large.tar.gz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Accept-Ranges", "bytes")
                    .append_header("Content-Length", large_content.len().to_string()),
            )
            .mount(&mock_server)
            .await;

        let attempt_count = Arc::new(AtomicUsize::new(0));
        let attempt_count_clone = attempt_count.clone();
        let large_content_for_closure = large_content.clone();

        Mock::given(method("GET"))
            .and(path("/large.tar.gz"))
            .respond_with(move |req: &wiremock::Request| {
                if let Some(range_header) = req.headers.get("Range") {
                    let current_attempt = attempt_count_clone.fetch_add(1, Ordering::SeqCst);

                    if current_attempt == 0 {
                        return ResponseTemplate::new(500);
                    }

                    let range_str = range_header.to_str().unwrap();
                    let range_part = range_str.strip_prefix("bytes=").unwrap();
                    let (start_str, end_str) = range_part.split_once('-').unwrap();
                    let start: usize = start_str.parse().unwrap();
                    let end: usize = end_str.parse().unwrap();

                    let chunk = &large_content_for_closure[start..=end];
                    ResponseTemplate::new(206)
                        .append_header("Content-Length", chunk.len().to_string())
                        .append_header(
                            "Content-Range",
                            format!(
                                "bytes {}-{}/{}",
                                start,
                                end,
                                large_content_for_closure.len()
                            ),
                        )
                        .set_body_bytes(chunk.to_vec())
                } else {
                    ResponseTemplate::new(200).set_body_bytes(large_content_for_closure.clone())
                }
            })
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = Downloader::with_semaphore(blob_cache, None);

        let url = format!("{}/large.tar.gz", mock_server.uri());
        let result = downloader
            .download_with_progress(&url, &actual_sha256, None, None)
            .await;

        assert!(result.is_ok(), "Download should succeed after retry");
        let blob_path = result.unwrap();
        assert!(blob_path.exists());

        let total_attempts = attempt_count.load(Ordering::SeqCst);
        assert!(
            total_attempts > 3,
            "Expected retry to occur (attempts: {})",
            total_attempts
        );

        let downloaded_content = std::fs::read(&blob_path).unwrap();
        assert_eq!(downloaded_content, large_content);
    }

    #[tokio::test]
    async fn auth_token_refresh_during_chunked_download() {
        let mock_server = MockServer::start().await;

        let large_content = vec![0xCDu8; 15 * 1024 * 1024];
        let actual_sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&large_content);
            zb_core::checksum::sha256_hex(hasher)
        };

        Mock::given(method("HEAD"))
            .and(path("/v2/homebrew/core/test/blobs/sha256:abc"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Accept-Ranges", "bytes")
                    .append_header("Content-Length", large_content.len().to_string()),
            )
            .mount(&mock_server)
            .await;

        let auth_challenges = Arc::new(AtomicUsize::new(0));
        let auth_challenges_clone = auth_challenges.clone();
        let large_content_for_closure = large_content.clone();

        Mock::given(method("GET"))
            .and(path("/v2/homebrew/core/test/blobs/sha256:abc"))
            .respond_with(move |req: &wiremock::Request| {
                if let Some(range_header) = req.headers.get("Range") {
                    if req.headers.get("Authorization").is_none() {
                        let count = auth_challenges_clone.fetch_add(1, Ordering::SeqCst);
                        if count == 0 {
                            return ResponseTemplate::new(401).append_header(
                                "WWW-Authenticate",
                                "Bearer realm=\"https://ghcr.io/token\",service=\"ghcr.io\",scope=\"repository:homebrew/core/test:pull\"",
                            );
                        }
                    }

                    let range_str = range_header.to_str().unwrap();
                    let range_part = range_str.strip_prefix("bytes=").unwrap();
                    let (start_str, end_str) = range_part.split_once('-').unwrap();
                    let start: usize = start_str.parse().unwrap();
                    let end: usize = end_str.parse().unwrap();

                    let chunk = &large_content_for_closure[start..=end];
                    ResponseTemplate::new(206)
                        .append_header("Content-Length", chunk.len().to_string())
                        .append_header(
                            "Content-Range",
                            format!(
                                "bytes {}-{}/{}",
                                start,
                                end,
                                large_content_for_closure.len()
                            ),
                        )
                        .set_body_bytes(chunk.to_vec())
                } else {
                    ResponseTemplate::new(200).set_body_bytes(large_content_for_closure.clone())
                }
            })
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "test-token-12345"
            })))
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = Downloader::with_semaphore(blob_cache, None);

        let url = format!(
            "{}/v2/homebrew/core/test/blobs/sha256:abc",
            mock_server.uri()
        );
        let result = downloader
            .download_with_progress(&url, &actual_sha256, None, None)
            .await;

        assert!(result.is_ok(), "Download should succeed after auth refresh");
        let blob_path = result.unwrap();
        assert!(blob_path.exists());

        let challenges = auth_challenges.load(Ordering::SeqCst);
        assert!(
            challenges > 0,
            "Expected at least one auth challenge (got {})",
            challenges
        );

        let downloaded_content = std::fs::read(&blob_path).unwrap();
        assert_eq!(downloaded_content, large_content);
    }

    #[tokio::test]
    async fn fallback_to_single_connection_on_chunk_failure() {
        let mock_server = MockServer::start().await;

        let large_content = vec![0xEFu8; 15 * 1024 * 1024];
        let actual_sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&large_content);
            zb_core::checksum::sha256_hex(hasher)
        };

        Mock::given(method("HEAD"))
            .and(path("/large.tar.gz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Accept-Ranges", "bytes")
                    .append_header("Content-Length", large_content.len().to_string()),
            )
            .mount(&mock_server)
            .await;

        let range_requests = Arc::new(AtomicUsize::new(0));
        let range_requests_clone = range_requests.clone();
        let large_content_for_closure = large_content.clone();

        Mock::given(method("GET"))
            .and(path("/large.tar.gz"))
            .respond_with(move |req: &wiremock::Request| {
                if let Some(range_header) = req.headers.get("Range") {
                    range_requests_clone.fetch_add(1, Ordering::SeqCst);

                    if range_header.to_str().unwrap() == "bytes=0-0" {
                        return ResponseTemplate::new(206)
                            .append_header("Content-Length", "1")
                            .append_header(
                                "Content-Range",
                                format!("bytes 0-0/{}", large_content_for_closure.len()),
                            )
                            .set_body_bytes(vec![large_content_for_closure[0]]);
                    }

                    ResponseTemplate::new(500)
                } else {
                    ResponseTemplate::new(200).set_body_bytes(large_content_for_closure.clone())
                }
            })
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = Downloader::with_semaphore(blob_cache, None);

        let url = format!("{}/large.tar.gz", mock_server.uri());
        let result = downloader
            .download_with_progress(&url, &actual_sha256, None, None)
            .await;

        assert!(
            result.is_ok(),
            "Download should succeed via fallback: {:?}",
            result.err()
        );
        let blob_path = result.unwrap();
        assert!(blob_path.exists());

        let downloaded_content = std::fs::read(&blob_path).unwrap();
        assert_eq!(downloaded_content, large_content);
    }

    #[tokio::test]
    async fn incorrect_content_range_triggers_fallback() {
        let mock_server = MockServer::start().await;

        let large_content = vec![0x12u8; 15 * 1024 * 1024];
        let actual_sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&large_content);
            zb_core::checksum::sha256_hex(hasher)
        };

        Mock::given(method("HEAD"))
            .and(path("/large.tar.gz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Accept-Ranges", "bytes")
                    .append_header("Content-Length", large_content.len().to_string()),
            )
            .mount(&mock_server)
            .await;

        let large_content_for_closure = large_content.clone();

        Mock::given(method("GET"))
            .and(path("/large.tar.gz"))
            .respond_with(move |req: &wiremock::Request| {
                if let Some(range_header) = req.headers.get("Range") {
                    let range_str = range_header.to_str().unwrap();

                    if range_str == "bytes=0-0" {
                        return ResponseTemplate::new(206)
                            .append_header("Content-Length", "1")
                            .append_header(
                                "Content-Range",
                                format!("bytes 0-0/{}", large_content_for_closure.len()),
                            )
                            .set_body_bytes(vec![large_content_for_closure[0]]);
                    }

                    let range_part = range_str.strip_prefix("bytes=").unwrap();
                    let (start_str, end_str) = range_part.split_once('-').unwrap();
                    let start: usize = start_str.parse().unwrap();
                    let end: usize = end_str.parse().unwrap();

                    let chunk = &large_content_for_closure[start..=end];
                    ResponseTemplate::new(206)
                        .append_header("Content-Length", chunk.len().to_string())
                        .append_header(
                            "Content-Range",
                            format!(
                                "bytes 0-{}/{}",
                                chunk.len() - 1,
                                large_content_for_closure.len()
                            ),
                        )
                        .set_body_bytes(chunk.to_vec())
                } else {
                    ResponseTemplate::new(200).set_body_bytes(large_content_for_closure.clone())
                }
            })
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = Downloader::with_semaphore(blob_cache, None);

        let url = format!("{}/large.tar.gz", mock_server.uri());
        let result = downloader
            .download_with_progress(&url, &actual_sha256, None, None)
            .await;

        assert!(
            result.is_ok(),
            "Download should succeed via fallback after incorrect Content-Range: {:?}",
            result.err()
        );
        let blob_path = result.unwrap();
        assert!(blob_path.exists());

        let downloaded_content = std::fs::read(&blob_path).unwrap();
        assert_eq!(downloaded_content, large_content);
    }

    /// Bottles live on ghcr.io, which answers every unauthenticated request with
    /// a 401 and a token challenge. The size probe has to survive that challenge,
    /// otherwise a large bottle silently falls back to a single whole-file GET
    /// and the chunked path never runs in production even though it passes every
    /// unauthenticated test. This test's shared token cache lets the size probe's
    /// challenge warm the token before any range request goes out, so it does not
    /// exercise the range-retry fix on its own — see
    /// `auth::tests::range_request_keeps_its_range_across_a_token_challenge` for
    /// that.
    #[tokio::test]
    async fn chunked_download_survives_registry_token_challenge() {
        let mock_server = MockServer::start().await;

        let large_content = vec![0x5Au8; 15 * 1024 * 1024];
        let actual_sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&large_content);
            zb_core::checksum::sha256_hex(hasher)
        };

        // `extract_scope_for_url` keys the token cache off the `ghcr.io/v2/`
        // marker, so the mock path mirrors a real bottle URL.
        let blob_path_str = "/ghcr.io/v2/homebrew/core/testpkg/blobs/sha256-abc";
        let challenge = format!(
            r#"Bearer realm="{}/token",service="ghcr.io",scope="repository:homebrew/core/testpkg:pull""#,
            mock_server.uri()
        );

        Mock::given(method("GET"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"token":"test-token"}"#))
            .mount(&mock_server)
            .await;

        let head_challenges = Arc::new(AtomicUsize::new(0));
        let head_challenges_clone = head_challenges.clone();
        let head_challenge = challenge.clone();
        let content_len = large_content.len();

        Mock::given(method("HEAD"))
            .and(path(blob_path_str))
            .respond_with(move |req: &wiremock::Request| {
                if req.headers.get("Authorization").is_none() {
                    head_challenges_clone.fetch_add(1, Ordering::SeqCst);
                    return ResponseTemplate::new(401)
                        .append_header("WWW-Authenticate", head_challenge.as_str());
                }

                ResponseTemplate::new(200)
                    .append_header("Accept-Ranges", "bytes")
                    .append_header("Content-Length", content_len.to_string())
            })
            .mount(&mock_server)
            .await;

        let range_requests = Arc::new(AtomicUsize::new(0));
        let whole_file_requests = Arc::new(AtomicUsize::new(0));
        let range_requests_clone = range_requests.clone();
        let whole_file_requests_clone = whole_file_requests.clone();
        let body = large_content.clone();

        Mock::given(method("GET"))
            .and(path(blob_path_str))
            .respond_with(move |req: &wiremock::Request| {
                if req.headers.get("Authorization").is_none() {
                    return ResponseTemplate::new(401)
                        .append_header("WWW-Authenticate", challenge.as_str());
                }

                let Some(range_header) = req.headers.get("Range") else {
                    whole_file_requests_clone.fetch_add(1, Ordering::SeqCst);
                    return ResponseTemplate::new(200).set_body_bytes(body.clone());
                };

                range_requests_clone.fetch_add(1, Ordering::SeqCst);

                let range_str = range_header.to_str().unwrap();
                let range_part = range_str.strip_prefix("bytes=").unwrap();
                let (start_str, end_str) = range_part.split_once('-').unwrap();
                let start: usize = start_str.parse().unwrap();
                let end: usize = end_str.parse().unwrap();

                let chunk = &body[start..=end];
                ResponseTemplate::new(206)
                    .append_header("Content-Length", chunk.len().to_string())
                    .append_header(
                        "Content-Range",
                        format!("bytes {}-{}/{}", start, end, body.len()),
                    )
                    .set_body_bytes(chunk.to_vec())
            })
            .mount(&mock_server)
            .await;

        let tmp = TempDir::new().unwrap();
        let blob_cache = BlobCache::new(tmp.path()).unwrap();
        let downloader = Downloader::with_semaphore(blob_cache, None);

        let url = format!("{}{blob_path_str}", mock_server.uri());
        let result = downloader
            .download_with_progress(&url, &actual_sha256, None, None)
            .await;

        assert!(result.is_ok(), "download failed: {:?}", result.err());
        assert_eq!(std::fs::read(result.unwrap()).unwrap(), large_content);

        assert!(
            head_challenges.load(Ordering::SeqCst) > 0,
            "the mock registry never issued a token challenge, so this test proves nothing"
        );
        assert!(
            range_requests.load(Ordering::SeqCst) > 0,
            "an authenticated registry served a large file without any Range request, \
             so the chunked path was skipped"
        );
        assert_eq!(
            whole_file_requests.load(Ordering::SeqCst),
            0,
            "the whole file was fetched in one GET despite the server supporting ranges"
        );
    }
}
