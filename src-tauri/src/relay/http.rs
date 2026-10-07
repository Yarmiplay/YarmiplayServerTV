//! The relay's HTTP side: readers `GET`/`HEAD /yarmiplay/files/{id}` (with
//! `Range`), seeders `PUT /yarmiplay/upload/{id}`. Served on the Syncplay
//! port by [`crate::syncplay::mux`], which checks the session token first.

use super::cache::{chunk_len, CHUNK};
use super::scheduler::Mode;
use super::Relay;
use crate::syncplay::room::ConnId;
use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, HeaderValue, Method, Response, StatusCode};
use futures_util::StreamExt;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::watch;

/// Bytes handed to the HTTP body per read.
const PIECE: u64 = 256 * 1024;

fn status(code: StatusCode, msg: &str) -> Response<Body> {
    let mut r = Response::new(Body::from(msg.to_string()));
    *r.status_mut() = code;
    r
}

#[derive(Debug, PartialEq, Eq)]
pub struct Unsatisfiable;

/// `Range: bytes=a-b` (one range only) as an inclusive `(start, end)`.
/// `Ok(None)` means the whole file.
pub fn parse_range(value: Option<&str>, size: u64) -> Result<Option<(u64, u64)>, Unsatisfiable> {
    let Some(v) = value else { return Ok(None) };
    let Some(spec) = v.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') || size == 0 {
        return Err(Unsatisfiable);
    }
    let (a, b) = spec.split_once('-').ok_or(Unsatisfiable)?;
    let (a, b) = (a.trim(), b.trim());
    let num = |s: &str| s.parse::<u64>().map_err(|_| Unsatisfiable);
    let (start, end) = if a.is_empty() {
        let n = num(b)?;
        if n == 0 {
            return Err(Unsatisfiable);
        }
        (size.saturating_sub(n), size - 1)
    } else {
        let end = if b.is_empty() {
            size - 1
        } else {
            num(b)?.min(size - 1)
        };
        (num(a)?, end)
    };
    if start > end || start >= size {
        return Err(Unsatisfiable);
    }
    Ok(Some((start, end)))
}

pub fn content_type(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mkv" => "video/x-matroska",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "ts" | "m2ts" => "video/mp2t",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

struct ReadStream {
    relay: Relay,
    id: String,
    reader: u64,
    rx: watch::Receiver<u64>,
    offset: u64,
    end: u64,
}

impl Drop for ReadStream {
    fn drop(&mut self) {
        self.relay.close_reader(&self.id, self.reader);
    }
}

pub async fn get_file(
    relay: &Relay,
    room: &str,
    id: &str,
    method: &Method,
    headers: &HeaderMap,
    mode: Mode,
) -> Response<Body> {
    let Some(info) = relay.file_info(room, id) else {
        return status(StatusCode::NOT_FOUND, "No such file in your room");
    };
    let size = info.size;
    let range = match parse_range(
        headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
        size,
    ) {
        Ok(r) => r,
        Err(Unsatisfiable) => {
            let mut r = status(StatusCode::RANGE_NOT_SATISFIABLE, "");
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{size}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return r;
        }
    };
    let (start, end) = range.unwrap_or((0, size.saturating_sub(1)));
    let len = if size == 0 { 0 } else { end - start + 1 };

    let body = if *method == Method::HEAD || len == 0 {
        Body::empty()
    } else {
        let Some((reader, rx)) = relay.open_reader(id, start, mode) else {
            return status(StatusCode::NOT_FOUND, "No such file in your room");
        };
        let rs = ReadStream {
            relay: relay.clone(),
            id: id.to_string(),
            reader,
            rx,
            offset: start,
            end: end + 1,
        };
        let stream = futures_util::stream::unfold(rs, |mut rs| async move {
            if rs.offset >= rs.end {
                return None;
            }
            let max = (rs.end - rs.offset).min(PIECE);
            let relay = rs.relay.clone();
            match relay
                .read_at(&rs.id, rs.reader, &mut rs.rx, rs.offset, max)
                .await
            {
                Some(d) => {
                    rs.offset += d.len() as u64;
                    Some((Ok::<Bytes, std::io::Error>(Bytes::from(d)), rs))
                }
                None => {
                    rs.offset = rs.end;
                    Some((
                        Err(std::io::Error::other("the file's sources went away")),
                        rs,
                    ))
                }
            }
        });
        Body::from_stream(stream)
    };

    let mut r = Response::new(body);
    *r.status_mut() = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let h = r.headers_mut();
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(&info.name)),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if range.is_some() {
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")) {
            h.insert(header::CONTENT_RANGE, v);
        }
    }
    r
}

pub async fn put_upload(relay: &Relay, conn: ConnId, upload: &str, body: Body) -> Response<Body> {
    let Some((file, size, offset, length)) = relay.upload_target(conn, upload) else {
        return status(StatusCode::NOT_FOUND, "No such upload for this session");
    };
    let end = offset + length;
    let mut pos = offset;
    let mut out: Option<(u64, tokio::fs::File)> = None;
    let mut stream = body.into_data_stream();
    let mut result: Result<(), (StatusCode, &str)> = Ok(());

    'body: while let Some(frame) = stream.next().await {
        let Ok(data) = frame else {
            result = Err((StatusCode::BAD_REQUEST, "upload interrupted"));
            break;
        };
        let mut data = &data[..];
        while !data.is_empty() {
            if pos >= end {
                result = Err((StatusCode::BAD_REQUEST, "more bytes than requested"));
                break 'body;
            }
            let index = pos / CHUNK;
            let within = pos % CHUNK;
            let n = (chunk_len(size, index) - within)
                .min(data.len() as u64)
                .min(end - pos) as usize;
            if !relay.upload_still_on(upload) {
                result = Err((StatusCode::CONFLICT, "upload was cancelled"));
                break 'body;
            }
            if out.as_ref().is_none_or(|(i, _)| *i != index) {
                let path = relay.inner.store.chunk_path(&file, index);
                let opened = if within == 0 {
                    tokio::fs::File::create(&path).await
                } else {
                    match tokio::fs::OpenOptions::new().write(true).open(&path).await {
                        Ok(mut f) => f.seek(std::io::SeekFrom::Start(within)).await.map(|_| f),
                        Err(e) => Err(e),
                    }
                };
                match opened {
                    Ok(f) => out = Some((index, f)),
                    Err(_) => {
                        result = Err((StatusCode::INTERNAL_SERVER_ERROR, "cache write failed"));
                        break 'body;
                    }
                }
            }
            let (_, f) = out.as_mut().unwrap();
            if f.write_all(&data[..n]).await.is_err() || f.flush().await.is_err() {
                result = Err((StatusCode::INTERNAL_SERVER_ERROR, "cache write failed"));
                break 'body;
            }
            if !relay.upload_progress(upload, pos, n as u64) {
                result = Err((StatusCode::CONFLICT, "upload was cancelled"));
                break 'body;
            }
            pos += n as u64;
            data = &data[n..];
        }
    }
    drop(out);
    let complete = result.is_ok() && pos == end;
    relay.finish_upload(upload, complete);
    match result {
        Ok(()) if complete => status(StatusCode::NO_CONTENT, ""),
        Ok(()) => status(StatusCode::BAD_REQUEST, "upload ended early"),
        Err((code, msg)) => status(code, msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 100), Ok(None));
        assert_eq!(parse_range(Some("bytes=0-9"), 100), Ok(Some((0, 9))));
        assert_eq!(parse_range(Some("bytes=90-"), 100), Ok(Some((90, 99))));
        assert_eq!(parse_range(Some("bytes=-10"), 100), Ok(Some((90, 99))));
        assert_eq!(parse_range(Some("bytes=50-1000"), 100), Ok(Some((50, 99))));
        assert_eq!(parse_range(Some("bytes=100-"), 100), Err(Unsatisfiable));
        assert_eq!(parse_range(Some("bytes=5-1"), 100), Err(Unsatisfiable));
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), Err(Unsatisfiable));
        assert_eq!(parse_range(Some("items=0-1"), 100), Ok(None));
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type("A.MKV"), "video/x-matroska");
        assert_eq!(content_type("noext"), "application/octet-stream");
    }
}
