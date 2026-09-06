//! Incremental model SSE framing. Reconnecting a partial generation is a
//! transport decision; this decoder never requests replay.
use super::ProviderError;

const DEFAULT_EVENT_LIMIT: usize = 1024 * 1024;
const DEFAULT_STREAM_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub kind: String,
    pub data: String,
}

/// Accepts byte boundaries within UTF-8 and CRLF. Rejects malformed UTF-8
/// instead of silently changing model tool arguments.
pub struct SseDecoder {
    line: Vec<u8>,
    data: String,
    kind: String,
    has_data: bool,
    first_line: bool,
    after_cr: bool,
    event_bytes: usize,
    stream_bytes: usize,
    event_limit: usize,
    stream_limit: usize,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_LIMIT, DEFAULT_STREAM_LIMIT)
    }
}

impl SseDecoder {
    pub fn new(event_limit: usize, stream_limit: usize) -> Self {
        Self {
            line: Vec::new(),
            data: String::new(),
            kind: String::new(),
            has_data: false,
            first_line: true,
            after_cr: false,
            event_bytes: 0,
            stream_bytes: 0,
            event_limit,
            stream_limit,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, ProviderError> {
        self.stream_bytes = self
            .stream_bytes
            .checked_add(bytes.len())
            .filter(|size| *size <= self.stream_limit)
            .ok_or_else(|| invalid("SSE stream exceeds byte limit"))?;
        let mut events = Vec::new();
        for &byte in bytes {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            self.event_bytes += 1;
            if self.event_bytes > self.event_limit {
                return Err(invalid("SSE event exceeds byte limit"));
            }
            match byte {
                b'\r' | b'\n' => {
                    self.finish_line(&mut events)?;
                    self.after_cr = byte == b'\r';
                }
                _ => self.line.push(byte),
            }
        }
        Ok(events)
    }

    fn finish_line(&mut self, events: &mut Vec<SseEvent>) -> Result<(), ProviderError> {
        let line = std::mem::take(&mut self.line);
        let line = std::str::from_utf8(&line).map_err(|_| invalid("SSE contains invalid UTF-8"))?;
        let line = if std::mem::replace(&mut self.first_line, false) {
            line.strip_prefix('\u{feff}').unwrap_or(line)
        } else {
            line
        };
        if line.is_empty() {
            if self.has_data {
                self.data.pop();
                events.push(SseEvent {
                    kind: if self.kind.is_empty() {
                        "message".to_owned()
                    } else {
                        std::mem::take(&mut self.kind)
                    },
                    data: std::mem::take(&mut self.data),
                });
            }
            self.kind.clear();
            self.has_data = false;
            self.event_bytes = 0;
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                self.has_data = true;
                self.data.push_str(value);
                self.data.push('\n');
            }
            "event" => {
                self.kind.clear();
                self.kind.push_str(value);
            }
            _ => {}
        }
        Ok(())
    }

    /// An unfinished event is never dispatched on EOF.
    pub fn finish(self) -> Result<(), ProviderError> {
        if !self.line.is_empty() || self.has_data || !self.kind.is_empty() {
            return Err(invalid("SSE ended inside an event"));
        }
        Ok(())
    }
}

fn invalid(message: &str) -> ProviderError {
    ProviderError::InvalidResponse(message.to_owned())
}

/// Deliver events as they arrive. The protocol callback returns true only for
/// its explicit completion marker. EOF alone never means a completed model run.
/// No reconnect/retry is performed after response delivery begins.
pub async fn read_stream<F, Fut>(
    mut response: reqwest::Response,
    deadline: tokio::time::Instant,
    mut on_event: F,
) -> Result<(), ProviderError>
where
    F: FnMut(SseEvent) -> Fut,
    Fut: std::future::Future<Output = Result<bool, ProviderError>>,
{
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if !response.status().is_success()
        || !content_type.is_some_and(|value| value.eq_ignore_ascii_case("text/event-stream"))
    {
        return Err(invalid("expected a successful text/event-stream response"));
    }
    let operation = async {
        let mut decoder = SseDecoder::default();
        while let Some(chunk) = response.chunk().await? {
            for event in decoder.push(&chunk)? {
                if on_event(event).await? {
                    return Ok(());
                }
            }
        }
        decoder.finish()?;
        Err(invalid("model stream ended without a completion event"))
    };
    tokio::time::timeout_at(deadline, operation)
        .await
        .map_err(|_| ProviderError::DeadlineExceeded)?
}

pub(crate) fn is_event_stream(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn network_events_arrive_before_eof_and_completion_closes_the_reader() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (release, released) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: first\n\n").await.unwrap();
            stream.flush().await.unwrap();
            released.await.unwrap();
            stream.write_all(b"data: [DONE]\n\n").await.unwrap();
            stream.flush().await.unwrap();
            std::future::pending::<()>().await;
        });
        let response = reqwest::get(url).await.unwrap();
        let mut release = Some(release);
        let mut count = 0;
        let result = read_stream(
            response,
            tokio::time::Instant::now() + std::time::Duration::from_secs(3),
            |event| {
                count += 1;
                if event.data == "first" {
                    release.take().unwrap().send(()).unwrap();
                }
                std::future::ready(Ok(event.data == "[DONE]"))
            },
        )
        .await;
        server.abort();
        result.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn eof_after_a_valid_event_is_still_incomplete() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: partial\n\n").await.unwrap();
        });
        let response = reqwest::get(url).await.unwrap();
        let mut delivered = false;
        let result = read_stream(
            response,
            tokio::time::Instant::now() + std::time::Duration::from_secs(3),
            |event| {
                delivered = event.data == "partial";
                std::future::ready(Ok(false))
            },
        )
        .await;
        server.await.unwrap();
        assert!(delivered);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("without a completion event")
        );
    }

    #[test]
    fn every_split_preserves_utf8_crlf_and_multiline_data() {
        let input =
            "\u{feff}: ping\r\nevent: delta\r\ndata: 你好\r\ndata: world\r\n\r\ndata: [DONE]\n\n"
                .as_bytes();
        for split in 0..=input.len() {
            let mut decoder = SseDecoder::default();
            let mut events = decoder.push(&input[..split]).unwrap();
            events.extend(decoder.push(&input[split..]).unwrap());
            decoder.finish().unwrap();
            assert_eq!(
                events,
                vec![
                    SseEvent {
                        kind: "delta".into(),
                        data: "你好\nworld".into()
                    },
                    SseEvent {
                        kind: "message".into(),
                        data: "[DONE]".into()
                    }
                ]
            );
        }
    }

    #[test]
    fn bytewise_cr_preserves_empty_data_and_resets_kind() {
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for byte in b"event: empty\rdata:\r\rdata: next\r\r" {
            events.extend(decoder.push(&[*byte]).unwrap());
        }
        decoder.finish().unwrap();
        assert_eq!(
            events,
            vec![
                SseEvent {
                    kind: "empty".into(),
                    data: "".into()
                },
                SseEvent {
                    kind: "message".into(),
                    data: "next".into()
                }
            ]
        );
    }

    #[test]
    fn comments_and_reconnect_fields_do_not_emit_content() {
        let mut decoder = SseDecoder::default();
        assert!(
            decoder
                .push(b": ping\nid: 42\nretry: 0\n\n")
                .unwrap()
                .is_empty()
        );
        decoder.finish().unwrap();
    }

    #[test]
    fn incomplete_or_invalid_event_never_becomes_a_reply() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: unfinished\n").unwrap().is_empty());
        assert!(decoder.finish().is_err());
        assert!(SseDecoder::default().push(b"data: \xff\n\n").is_err());
    }

    #[test]
    fn limits_cover_unterminated_lines_and_accumulated_data() {
        assert!(SseDecoder::new(8, 100).push(b"data: too long").is_err());
        assert!(
            SseDecoder::new(20, 100)
                .push(b"data: abc\ndata: def\ndata: ghi\n")
                .is_err()
        );
        let mut decoder = SseDecoder::new(10, 10);
        decoder.push(b"data: a\n\n").unwrap();
        assert!(decoder.push(b"data: b\n\n").is_err());
    }
}
