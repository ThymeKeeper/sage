use std::io::Read;

use flate2::bufread::GzDecoder;
use reqwest::header::HeaderMap;

use super::{Error, Result};

const HEADER_SSE_C_ALGORITHM: &str = "x-amz-server-side-encryption-customer-algorithm";
const HEADER_SSE_C_KEY: &str = "x-amz-server-side-encryption-customer-key";
const AES256: &str = "AES256";

/// Attempts per chunk. The pre-signed URL serves immutable content, so every
/// failure mode short of URL expiry (dropped connection, truncated body,
/// gateway 5xx) is worth retrying.
const CHUNK_DOWNLOAD_ATTEMPTS: u32 = 3;

pub(crate) async fn download_chunk(
    client: reqwest::Client,
    chunk_url: String,
    mut headers: HeaderMap,
    qrmk: String,
) -> Result<Vec<Vec<Option<String>>>> {
    if headers.is_empty() {
        headers.append(HEADER_SSE_C_ALGORITHM, AES256.parse()?);
        headers.append(HEADER_SSE_C_KEY, qrmk.parse()?);
    }

    let mut last_err = None;
    for attempt in 0..CHUNK_DOWNLOAD_ATTEMPTS {
        if attempt > 0 {
            // 2s then 4s before the second and third attempts.
            tokio::time::sleep(std::time::Duration::from_secs(2u64 << (attempt - 1))).await;
        }
        match download_chunk_once(&client, &chunk_url, headers.clone()).await {
            Ok(rows) => return Ok(rows),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("CHUNK_DOWNLOAD_ATTEMPTS > 0 guarantees at least one attempt"))
}

async fn download_chunk_once(
    client: &reqwest::Client,
    chunk_url: &str,
    headers: HeaderMap,
) -> Result<Vec<Vec<Option<String>>>> {
    let response = client.get(chunk_url).headers(headers).send().await?;
    if !response.status().is_success() {
        let body = response.text().await?;
        return Err(Error::ChunkDownload(body));
    }

    let body = response.bytes().await?;
    if body.len() < 2 {
        return Err(Error::ChunkDownload("invalid chunk format".into()));
    }

    let bytes = if body[0] == 0x1f && body[1] == 0x8b {
        let mut d = GzDecoder::new(&body[..]);
        let mut buf = vec![];
        d.read_to_end(&mut buf)?;
        buf
    } else {
        body.to_vec()
    };

    let mut buf = vec![b'['];
    buf.extend(bytes);
    buf.push(b']');
    let rows: Vec<Vec<Option<String>>> = match serde_json::from_slice(&buf) {
        Ok(rows) => rows,
        Err(e) => {
            return Err(Error::Json(e, String::from_utf8_lossy(&buf).into_owned()));
        }
    };
    Ok(rows)
}
