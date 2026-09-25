use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, HeaderValue, WWW_AUTHENTICATE};
use serde::Deserialize;
use tokio::sync::RwLock;

use zb_core::Error;

use super::MAX_CHUNK_RETRIES;

pub(crate) fn bearer_header(token: &str) -> Result<HeaderValue, Error> {
    HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| Error::NetworkFailure {
        message: "auth token contains invalid header characters".into(),
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    token: String,
}

pub(crate) struct CachedToken {
    pub(crate) token: String,
    pub(crate) expires_at: Instant,
}

pub(crate) type TokenCache = Arc<RwLock<HashMap<String, CachedToken>>>;

/// Send a request, transparently answering a registry token challenge.
///
/// The retry replays the request the caller built rather than a bare GET, so
/// headers that change what the request *means* survive the 401. A `Range`
/// probe that came back as a whole-file 200 would otherwise read as "this
/// server does not support ranges".
async fn send_with_auth<F>(
    client: &reqwest::Client,
    token_cache: &TokenCache,
    url: &str,
    build: F,
) -> Result<reqwest::Response, Error>
where
    F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
{
    let cached_token = get_cached_token_for_url_internal(token_cache, url).await;

    let mut request = build(client);
    if let Some(token) = &cached_token {
        request = request.header(AUTHORIZATION, bearer_header(token)?);
    }

    let response = request.send().await.map_err(|e| Error::NetworkFailure {
        message: e.to_string(),
    })?;

    if response.status() != StatusCode::UNAUTHORIZED {
        return Ok(response);
    }

    let token = {
        let www_auth = match response.headers().get(WWW_AUTHENTICATE) {
            Some(value) => value.to_str().map_err(|_| Error::NetworkFailure {
                message: "WWW-Authenticate header contains invalid characters".to_string(),
            })?,
            None => {
                return Err(Error::NetworkFailure {
                    message:
                        "server returned 401 without WWW-Authenticate header (may be rate limited)"
                            .to_string(),
                });
            }
        };

        fetch_bearer_token_internal(client, token_cache, url, www_auth).await?
    };

    let response = build(client)
        .header(AUTHORIZATION, bearer_header(&token)?)
        .send()
        .await
        .map_err(|e| Error::NetworkFailure {
            message: e.to_string(),
        })?;

    if response.status() == StatusCode::UNAUTHORIZED {
        return Err(Error::NetworkFailure {
            message: "authentication failed: token was rejected by server".to_string(),
        });
    }

    Ok(response)
}

pub(crate) async fn fetch_download_response_internal(
    client: &reqwest::Client,
    token_cache: &TokenCache,
    url: &str,
) -> Result<reqwest::Response, Error> {
    let response = send_with_auth(client, token_cache, url, |client| client.get(url)).await?;

    if !response.status().is_success() {
        return Err(Error::NetworkFailure {
            message: format!("HTTP {}", response.status()),
        });
    }

    Ok(response)
}

/// Probe a download target for its size and range support.
///
/// Answering the token challenge here is what decides whether a large bottle
/// takes the chunked path at all, and it warms the token cache so the transfer
/// that follows does not spend a round trip rediscovering the same challenge.
pub(crate) async fn fetch_head_response_internal(
    client: &reqwest::Client,
    token_cache: &TokenCache,
    url: &str,
) -> Result<reqwest::Response, Error> {
    send_with_auth(client, token_cache, url, |client| client.head(url)).await
}

pub(crate) async fn fetch_range_response_internal(
    client: &reqwest::Client,
    token_cache: &TokenCache,
    url: &str,
    range: &str,
) -> Result<reqwest::Response, Error> {
    let mut last_error = None;

    for attempt in 0..=MAX_CHUNK_RETRIES {
        match send_with_auth(client, token_cache, url, |client| {
            client.get(url).header("Range", range)
        })
        .await
        {
            Ok(response) => {
                if !response.status().is_success() {
                    let err = Error::NetworkFailure {
                        message: format!("HTTP {}", response.status()),
                    };

                    if response.status().is_server_error() && attempt < MAX_CHUNK_RETRIES {
                        last_error = Some(err);
                        tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
                        continue;
                    }
                    return Err(err);
                }

                return Ok(response);
            }
            Err(e) => {
                last_error = Some(Error::NetworkFailure {
                    message: e.to_string(),
                });

                if attempt < MAX_CHUNK_RETRIES {
                    tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
                    continue;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| Error::NetworkFailure {
        message: "range request failed after retries".into(),
    }))
}

pub(crate) async fn get_cached_token_for_url_internal(
    token_cache: &TokenCache,
    url: &str,
) -> Option<String> {
    let scope = token_cache_key(url, &extract_scope_for_url(url)?);
    let cache = token_cache.read().await;
    let now = Instant::now();

    cache
        .get(&scope)
        .filter(|cached| cached.expires_at > now)
        .map(|cached| cached.token.clone())
}

pub(crate) async fn fetch_bearer_token_internal(
    client: &reqwest::Client,
    token_cache: &TokenCache,
    url: &str,
    www_authenticate: &str,
) -> Result<String, Error> {
    let (realm, service, scope) = parse_www_authenticate(www_authenticate)?;
    let cache_key = token_cache_key(url, &scope);

    {
        let cache = token_cache.read().await;
        if let Some(cached) = cache.get(&cache_key)
            && cached.expires_at > Instant::now()
        {
            return Ok(cached.token.clone());
        }
    }

    let token_url =
        reqwest::Url::parse_with_params(&realm, &[("service", &service), ("scope", &scope)])
            .map_err(Error::network("failed to construct token URL"))?;

    let response = client
        .get(token_url)
        .send()
        .await
        .map_err(Error::network("token request failed"))?;

    if !response.status().is_success() {
        return Err(Error::NetworkFailure {
            message: format!("token request returned HTTP {}", response.status()),
        });
    }

    let token_response: TokenResponse = response
        .json()
        .await
        .map_err(Error::network("failed to parse token response"))?;

    {
        let mut cache = token_cache.write().await;
        cache.insert(
            cache_key,
            CachedToken {
                token: token_response.token.clone(),
                expires_at: Instant::now() + Duration::from_secs(240),
            },
        );
    }

    Ok(token_response.token)
}

/// Cache key for a registry token: the host that issued it, plus the scope.
///
/// A token is only valid at the registry that minted it, and mirrors made
/// these URLs multi-host — before that every URL reaching this module was
/// literally ghcr.io, so the scope alone was incidentally host-unique.
/// Keying on the scope alone now lets a mirror's token be attached to an
/// upstream request, and the retry re-reads the same entry, so the fallback
/// dies with "token was rejected by server" instead of succeeding.
fn token_cache_key(url: &str, scope: &str) -> String {
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .unwrap_or_default();
    format!("{host}\u{1f}{scope}")
}

pub(crate) fn extract_scope_for_url(url: &str) -> Option<String> {
    let marker = "ghcr.io/v2/";
    let start = url.find(marker)? + marker.len();
    let remainder = &url[start..];
    let mut parts = remainder.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    let formula = parts.next()?;
    if owner.is_empty() || repo.is_empty() || formula.is_empty() {
        return None;
    }
    Some(format!("repository:{owner}/{repo}/{formula}:pull"))
}

fn parse_www_authenticate(header: &str) -> Result<(String, String, String), Error> {
    let header = header
        .strip_prefix("Bearer ")
        .ok_or_else(|| Error::NetworkFailure {
            message: "unsupported auth scheme".to_string(),
        })?;

    let mut realm = None;
    let mut service = None;
    let mut scope = None;

    for part in header.split(',') {
        let part = part.trim();
        if let Some((key, value)) = part.split_once('=') {
            let value = value.trim_matches('"');
            match key {
                "realm" => realm = Some(value.to_string()),
                "service" => service = Some(value.to_string()),
                "scope" => scope = Some(value.to_string()),
                _ => {}
            }
        }
    }

    let realm = realm.ok_or_else(|| Error::NetworkFailure {
        message: "missing realm in WWW-Authenticate".to_string(),
    })?;
    let service = service.ok_or_else(|| Error::NetworkFailure {
        message: "missing service in WWW-Authenticate".to_string(),
    })?;
    let scope = scope.ok_or_else(|| Error::NetworkFailure {
        message: "missing scope in WWW-Authenticate".to_string(),
    })?;

    Ok((realm, service, scope))
}

#[cfg(test)]
mod tests {
    use super::*;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A token can expire mid-download, so a range request has to be able to
    /// answer a challenge and still be a range request afterwards. Replaying it
    /// as a plain GET returns the whole file with a 200, which the caller reads
    /// as "this server does not support ranges".
    #[tokio::test]
    async fn range_request_keeps_its_range_across_a_token_challenge() {
        let mock_server = MockServer::start().await;

        let body = b"0123456789abcdef".to_vec();
        let blob_path = "/ghcr.io/v2/homebrew/core/rangepkg/blobs/sha256-abc";
        let challenge = format!(
            r#"Bearer realm="{}/token",service="ghcr.io",scope="repository:homebrew/core/rangepkg:pull""#,
            mock_server.uri()
        );

        Mock::given(method("GET"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"token":"test-token"}"#))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(blob_path))
            .respond_with(move |req: &wiremock::Request| {
                if req.headers.get("Authorization").is_none() {
                    return ResponseTemplate::new(401)
                        .append_header("WWW-Authenticate", challenge.as_str());
                }

                let Some(range_header) = req.headers.get("Range") else {
                    return ResponseTemplate::new(200).set_body_bytes(body.clone());
                };

                let range_part = range_header
                    .to_str()
                    .unwrap()
                    .strip_prefix("bytes=")
                    .unwrap();
                let (start_str, end_str) = range_part.split_once('-').unwrap();
                let start: usize = start_str.parse().unwrap();
                let end: usize = end_str.parse().unwrap();

                ResponseTemplate::new(206)
                    .append_header(
                        "Content-Range",
                        format!("bytes {}-{}/{}", start, end, body.len()),
                    )
                    .set_body_bytes(body[start..=end].to_vec())
            })
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let token_cache: TokenCache = Arc::new(RwLock::new(HashMap::new()));
        let url = format!("{}{blob_path}", mock_server.uri());

        let response = fetch_range_response_internal(&client, &token_cache, &url, "bytes=0-3")
            .await
            .expect("range request should succeed after the token challenge");

        assert_eq!(
            response.status(),
            StatusCode::PARTIAL_CONTENT,
            "the retry after the 401 dropped the Range header"
        );
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"0123");
    }

    #[test]
    fn extract_scope_for_url_supports_core_packages() {
        let scope =
            extract_scope_for_url("https://ghcr.io/v2/homebrew/core/lz4/blobs/sha256:abc").unwrap();
        assert_eq!(scope, "repository:homebrew/core/lz4:pull");
    }

    /// A token minted by one registry must never be attached to a request to
    /// another. Mirrors made these URLs multi-host; before that the scope was
    /// incidentally host-unique, which is why this could go unnoticed.
    #[test]
    fn token_cache_keys_separate_hosts_sharing_a_scope() {
        let scope = "repository:homebrew/core/lz4:pull";
        let upstream =
            token_cache_key("https://ghcr.io/v2/homebrew/core/lz4/blobs/sha256:a", scope);
        let mirror = token_cache_key(
            "https://mirror.example.com/v2/ghcr-io/homebrew/core/lz4/blobs/sha256:a",
            scope,
        );

        assert_ne!(upstream, mirror);
        assert_eq!(
            upstream,
            token_cache_key(
                "https://ghcr.io/v2/homebrew/core/other/blobs/sha256:b",
                scope
            ),
            "the same host and scope must still share one entry"
        );
    }

    #[test]
    fn extract_scope_for_url_supports_tapped_packages() {
        let scope =
            extract_scope_for_url("https://ghcr.io/v2/hashicorp/tap/terraform/blobs/sha256:abc")
                .unwrap();
        assert_eq!(scope, "repository:hashicorp/tap/terraform:pull");
    }
}
