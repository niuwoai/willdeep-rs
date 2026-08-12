use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use http::header::HeaderValue;
use qrcode::{EcLevel, QrCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

const DEFAULT_RELAY_BASE_URL: &str = "https://j.niuwoai.com";
const PROTOCOL_VERSION: &str = "mobile-gateway.v1";
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const ROOM_PREFIX: &str = "wd-";
/// 128 位随机 token 的十六进制长度；配对 JSON 里出现两次，是二维码尺寸的大头。
const TOKEN_HEX_LEN: usize = 32;
const ROOM_ID_HEX_LEN: usize = 32;
/// 桌面名只是给手机端展示，超长主机名会白白把二维码撑大一个版本。
const MAX_DESKTOP_NAME_LEN: usize = 16;
/// 配对二维码在终端里的尺寸（含 4 模块静区）：65 模块 + 静区 = 73 列，
/// Dense1x2 一个字符格装两行模块，所以是 37 行。再大弹窗就开始吞掉整屏。
/// 仅作为回归测试的断言基准。
#[cfg(test)]
const MAX_QR_WIDTH: usize = 73;
#[cfg(test)]
const MAX_QR_HEIGHT: usize = 37;

#[derive(Clone, Debug)]
pub struct MobilePrompt {
    pub text: String,
}

#[derive(Clone)]
pub struct RelayBridge {
    events: broadcast::Sender<String>,
    session_id: Arc<RwLock<Option<String>>>,
}

impl RelayBridge {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            events,
            session_id: Arc::new(RwLock::new(None)),
        }
    }

    pub fn set_session(&self, session_id: impl Into<String>) {
        if let Ok(mut value) = self.session_id.write() {
            *value = Some(session_id.into());
        }
    }

    pub fn publish_assistant(&self, content: &str) {
        let message_id = Uuid::new_v4().to_string();
        let session_id = self.session_id.read().ok().and_then(|value| value.clone());
        self.publish(json!({
            "id": Uuid::new_v4(),
            "type": "message.append",
            "session_id": session_id,
            "payload": {
                "id": message_id,
                "role": "assistant",
                "content": content,
                "created_at": unix_timestamp().to_string(),
                "is_streaming": false,
            },
            "ts": unix_timestamp(),
        }));
        self.publish(json!({
            "id": Uuid::new_v4(),
            "type": "message.done",
            "session_id": session_id,
            "payload": {"message_id": message_id},
            "ts": unix_timestamp(),
        }));
    }

    fn publish(&self, value: Value) {
        let _ = self.events.send(value.to_string());
    }
}

pub struct RelayGateway {
    task: JoinHandle<()>,
    pub qr: String,
    pub room: String,
}

impl RelayGateway {
    pub fn start(
        home: &Path,
        bridge: RelayBridge,
        prompts: mpsc::UnboundedSender<MobilePrompt>,
        snapshot: Value,
    ) -> Result<Self> {
        let credentials = RelayCredentials::load_or_create(home)?;
        let pairing = PairingPayload::new(&credentials);
        let qr = render_qr(&serde_json::to_string(&pairing)?)?;
        let room = credentials.room.clone();
        let task = tokio::spawn(run_relay(credentials, bridge, prompts, snapshot));
        Ok(Self { task, qr, room })
    }
}

impl Drop for RelayGateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct RelayCredentials {
    relay_base_url: String,
    room: String,
    token: String,
}

impl RelayCredentials {
    fn load_or_create(home: &Path) -> Result<Self> {
        let path = home.join("mobile-relay.toml");
        if path.exists() {
            validate_secret_permissions(&path)?;
            let contents = std::fs::read_to_string(&path)
                .with_context(|| format!("read relay credentials: {}", path.display()))?;
            let existing: Self =
                toml::from_str(&contents).context("parse mobile relay credentials")?;
            if existing.is_compact() {
                return Ok(existing);
            }
        }
        std::fs::create_dir_all(home)?;
        let credentials = Self::generate();
        let temporary = home.join(format!(".mobile-relay-{}.tmp", Uuid::new_v4()));
        std::fs::write(&temporary, toml::to_string_pretty(&credentials)?)?;
        set_secret_permissions(&temporary)?;
        std::fs::rename(&temporary, &path)?;
        Ok(credentials)
    }

    fn generate() -> Self {
        Self {
            relay_base_url: DEFAULT_RELAY_BASE_URL.to_owned(),
            room: format!("{ROOM_PREFIX}{}", Uuid::new_v4().simple()),
            token: random_token(),
        }
    }

    /// 旧版凭据用 128 位 token ×2 + 带连字符的 UUID room，配对 JSON 会撑到 437 字节，
    /// 二维码要 81×81 模块，几乎铺满终端。命中旧格式就换成紧凑格式重新落盘（手机重新扫码即可）。
    fn is_compact(&self) -> bool {
        self.token.len() <= TOKEN_HEX_LEN
            && self
                .room
                .strip_prefix(ROOM_PREFIX)
                .is_some_and(|id| id.len() <= ROOM_ID_HEX_LEN && !id.contains('-'))
    }

    fn websocket_url(&self) -> String {
        let base = self
            .relay_base_url
            .trim_end_matches('/')
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        format!("{base}/ws/broadcast/{}", self.room.trim_matches('/'))
    }
}

#[derive(Serialize)]
struct PairingPayload<'a> {
    base_url: &'a str,
    pairing_token: &'a str,
    protocol_version: &'static str,
    desktop_name: String,
    expires_at: &'static str,
    relay_base_url: &'a str,
    relay_room: &'a str,
    relay_token: &'a str,
}

impl<'a> PairingPayload<'a> {
    fn new(credentials: &'a RelayCredentials) -> Self {
        Self {
            base_url: &credentials.relay_base_url,
            pairing_token: &credentials.token,
            protocol_version: PROTOCOL_VERSION,
            desktop_name: desktop_name(),
            expires_at: "2099-12-31T23:59:59Z",
            relay_base_url: &credentials.relay_base_url,
            relay_room: &credentials.room,
            relay_token: &credentials.token,
        }
    }
}

async fn run_relay(
    credentials: RelayCredentials,
    bridge: RelayBridge,
    prompts: mpsc::UnboundedSender<MobilePrompt>,
    snapshot: Value,
) {
    loop {
        let Ok(request) = relay_request(&credentials) else {
            tokio::time::sleep(RECONNECT_DELAY).await;
            continue;
        };
        if let Ok((socket, _)) = tokio_tungstenite::connect_async(request).await {
            let (mut output, mut input) = socket.split();
            let mut events = bridge.events.subscribe();
            let _ = output
                .send(WebSocketMessage::Text(snapshot.to_string().into()))
                .await;
            loop {
                tokio::select! {
                    event = events.recv() => match event {
                        Ok(value) => {
                            if output.send(WebSocketMessage::Text(value.into())).await.is_err() { break; }
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                        _ => {}
                    },
                    incoming = input.next() => match incoming {
                        Some(Ok(WebSocketMessage::Text(value))) => {
                            if let Some(response) = handle_command(&prompts, &snapshot, &value)
                                && output.send(WebSocketMessage::Text(response.into())).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(WebSocketMessage::Ping(value))) => {
                            if output.send(WebSocketMessage::Pong(value)).await.is_err() { break; }
                        }
                        Some(Ok(WebSocketMessage::Close(_))) | None | Some(Err(_)) => break,
                        _ => {}
                    }
                }
            }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

fn relay_request(
    credentials: &RelayCredentials,
) -> Result<http::Request<()>, tokio_tungstenite::tungstenite::Error> {
    let mut request = credentials.websocket_url().into_client_request()?;
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", credentials.token))?,
    );
    request.headers_mut().insert(
        "X-App-Version",
        HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
    Ok(request)
}

fn handle_command(
    prompts: &mpsc::UnboundedSender<MobilePrompt>,
    snapshot: &Value,
    input: &str,
) -> Option<String> {
    let envelope: Value = serde_json::from_str(input).ok()?;
    let kind = envelope.get("type")?.as_str()?;
    let id = envelope.get("id").and_then(Value::as_str);
    match kind {
        "message.send" => {
            let text = envelope
                .pointer("/payload/text")
                .or_else(|| envelope.pointer("/payload/content"))
                .and_then(Value::as_str)?
                .trim();
            if text.is_empty() {
                return Some(error_envelope(id, "message text is empty"));
            }
            if prompts
                .send(MobilePrompt {
                    text: text.to_owned(),
                })
                .is_err()
            {
                return Some(error_envelope(id, "CLI session is unavailable"));
            }
            Some(ack_envelope(id, "message.send"))
        }
        "session.list" | "session.select" => Some(snapshot.to_string()),
        value if value.starts_with("state.") || value.starts_with("message.") => None,
        _ => Some(error_envelope(id, &format!("unsupported command: {kind}"))),
    }
}

fn ack_envelope(id: Option<&str>, command: &str) -> String {
    json!({"id": id, "type": "ack", "payload": {"command": command}, "ts": unix_timestamp()})
        .to_string()
}

fn error_envelope(id: Option<&str>, message: &str) -> String {
    json!({"id": id, "type": "error", "payload": {"message": message}, "ts": unix_timestamp()})
        .to_string()
}

/// 终端里一个模块占一个字符格，纠错等级越高模块越多。屏幕上的二维码不会被印污或折损，
/// L 级（7% 冗余）足够，比默认的 M 级少一到两个版本，宽度直接省掉十几列。
fn render_qr(payload: &str) -> Result<String> {
    let code = QrCode::with_error_correction_level(payload.as_bytes(), EcLevel::L)
        .context("encode mobile pairing QR")?;
    Ok(code
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build())
}

fn desktop_name() -> String {
    let name = std::env::var("HOSTNAME")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "WillDeep CLI".to_owned());
    match name.char_indices().nth(MAX_DESKTOP_NAME_LEN) {
        Some((index, _)) => name[..index].to_owned(),
        None => name,
    }
}

fn random_token() -> String {
    Uuid::new_v4().simple().to_string()
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(unix)]
fn set_secret_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(unix)]
fn validate_secret_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
    anyhow::ensure!(
        mode & 0o077 == 0,
        "{} contains a relay token but permissions are {:o}; run `chmod 600 {}`",
        path.display(),
        mode,
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn set_secret_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn validate_secret_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_send_is_forwarded() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let response = handle_command(
            &tx,
            &json!({"type": "state.snapshot"}),
            r#"{"id":"1","type":"message.send","payload":{"text":"hello"}}"#,
        )
        .unwrap();
        assert!(response.contains("ack"));
        assert_eq!(rx.try_recv().unwrap().text, "hello");
    }

    #[test]
    fn pairing_payload_matches_android_relay_contract() {
        let credentials = RelayCredentials {
            relay_base_url: DEFAULT_RELAY_BASE_URL.to_owned(),
            room: "willdeep-cli-test".to_owned(),
            token: "secret".to_owned(),
        };
        let payload = serde_json::to_value(PairingPayload::new(&credentials)).unwrap();
        assert_eq!(payload["protocol_version"], PROTOCOL_VERSION);
        assert_eq!(payload["relay_room"], "willdeep-cli-test");
        assert_eq!(payload["relay_token"], "secret");
    }

    #[test]
    fn pairing_qr_fits_the_terminal_popup() {
        let credentials = RelayCredentials::generate();
        let payload = serde_json::to_string(&PairingPayload::new(&credentials)).unwrap();
        let qr = render_qr(&payload).unwrap();
        let width = qr
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or_default();
        let height = qr.lines().count();
        assert_eq!(
            (width, height),
            (MAX_QR_WIDTH, MAX_QR_HEIGHT),
            "配对二维码尺寸变了（载荷 {} 字节）",
            payload.len()
        );
    }

    #[test]
    fn legacy_credentials_are_recompacted_on_load() {
        let home = std::env::temp_dir().join(format!("willdeep-relay-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let legacy = RelayCredentials {
            relay_base_url: DEFAULT_RELAY_BASE_URL.to_owned(),
            room: format!("willdeep-cli-{}", Uuid::new_v4()),
            token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
        };
        assert!(!legacy.is_compact());
        let path = home.join("mobile-relay.toml");
        std::fs::write(&path, toml::to_string_pretty(&legacy).unwrap()).unwrap();
        set_secret_permissions(&path).unwrap();

        let loaded = RelayCredentials::load_or_create(&home).unwrap();
        assert!(loaded.is_compact());
        assert_ne!(loaded.token, legacy.token);
        let reloaded = RelayCredentials::load_or_create(&home).unwrap();
        assert_eq!(reloaded.token, loaded.token, "紧凑凭据不应被反复重置");
        std::fs::remove_dir_all(&home).ok();
    }

    #[tokio::test]
    #[ignore = "requires the public j.niuwoai.com relay"]
    async fn live_relay_broadcasts_between_two_authenticated_peers() {
        let credentials = RelayCredentials {
            relay_base_url: DEFAULT_RELAY_BASE_URL.to_owned(),
            room: format!("willdeep-cli-smoke-{}", Uuid::new_v4()),
            token: random_token(),
        };
        let (left, _) = tokio_tungstenite::connect_async(relay_request(&credentials).unwrap())
            .await
            .unwrap();
        let (right, _) = tokio_tungstenite::connect_async(relay_request(&credentials).unwrap())
            .await
            .unwrap();
        let (mut left_output, _) = left.split();
        let (_, mut right_input) = right.split();
        let marker = format!("relay-smoke-{}", Uuid::new_v4());
        left_output
            .send(WebSocketMessage::Text(marker.clone().into()))
            .await
            .unwrap();
        let received = tokio::time::timeout(Duration::from_secs(10), right_input.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(received.into_text().unwrap(), marker);
    }
}
