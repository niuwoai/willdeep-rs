//! 测试用的手工 HTTP/1.1 服务端：一个连接一个请求，`Connection: close`。
//! 不引入 axum 之类的依赖进 core 的测试；要的只是让 reqwest 有个能说话的对端。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
pub(super) struct Request {
    pub method: String,
    pub path: String,
    /// 头名字统一小写。
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

#[derive(Clone, Debug)]
pub(super) struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub(super) async fn spawn_http_server<F>(handler: F) -> (SocketAddr, JoinHandle<()>)
where
    F: Fn(Request) -> Response + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handler = Arc::new(handler);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    if let Some(position) = find_header_end(&buffer) {
                        break position;
                    }
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or_default();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_owned();
                let path = parts.next().unwrap_or_default().to_owned();
                let mut headers = BTreeMap::new();
                for line in lines {
                    if let Some((name, value)) = line.split_once(':') {
                        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
                    }
                }
                let length: usize = headers
                    .get("content-length")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                let mut body = buffer[header_end + 4..].to_vec();
                while body.len() < length {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => body.extend_from_slice(&chunk[..n]),
                    }
                }
                body.truncate(length);
                let response = handler(Request {
                    method,
                    path,
                    headers,
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let mut wire = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    response.status,
                    reason(response.status),
                    response.body.len()
                )
                .into_bytes();
                for (name, value) in &response.headers {
                    wire.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
                }
                wire.extend_from_slice(b"\r\n");
                wire.extend_from_slice(&response.body);
                let _ = socket.write_all(&wire).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (address, task)
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Status",
    }
}
