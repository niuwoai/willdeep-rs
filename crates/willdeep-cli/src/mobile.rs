//! 手机中继的共享件：凭据、配对 URL、二维码、中继连接请求。
//!
//! 连接本身归 Runtime Daemon（`daemon/mobile_gateway.rs`）：一台机器一个 Daemon、
//! 一个 room。TUI 的 `/mobile` 与 `willdeep daemon mobile` 只是遥控器，用这里的
//! 函数出二维码，不持有任何连接。

use std::path::Path;

use anyhow::{Context, Result};
use http::header::HeaderValue;
use qrcode::{EcLevel, QrCode};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

const DEFAULT_RELAY_BASE_URL: &str = "https://j.niuwoai.com";
const CREDENTIALS_FILE: &str = "mobile-relay.toml";
const ROOM_PREFIX: &str = "wd-";
/// 128 位随机 token 的十六进制长度；配对 JSON 里出现两次，是二维码尺寸的大头。
const TOKEN_HEX_LEN: usize = 32;
const ROOM_ID_HEX_LEN: usize = 32;
/// 桌面名只是给手机端展示，超长主机名会白白把二维码撑大一个版本。按 UTF-8 字节数截断。
const MAX_DESKTOP_NAME_LEN: usize = 16;
/// 配对二维码在终端里的尺寸上界（含 4 模块静区）：桌面名顶满且全需转义时 45 模块 + 静区 = 53 列，
/// Dense1x2 一个字符格装两行模块，所以是 27 行。ASCII 主机名的常见情况是 41 模块 / 49×25。
/// 再大弹窗就开始吞掉整屏。仅作为回归测试的断言基准。
#[cfg(test)]
const MAX_QR_WIDTH: usize = 53;
#[cfg(test)]
const MAX_QR_HEIGHT: usize = 27;

/// 凭据文件对同组或其他用户可读。单独成类型，Runtime 据此给出「chmod 600」的可操作提示，
/// 而不是一句笼统的内部错误。
#[derive(Debug)]
pub(crate) struct UnsafeCredentialPermissions {
    mode: u32,
}

impl std::fmt::Display for UnsafeCredentialPermissions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "mobile relay credentials have permissions {:o}; run `chmod 600 $WILLDEEP_HOME/{CREDENTIALS_FILE}`",
            self.mode
        )
    }
}

impl std::error::Error for UnsafeCredentialPermissions {}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RelayCredentials {
    relay_base_url: String,
    room: String,
    token: String,
    /// 用户是否打开了中继。Daemon 启动时据此自动重连；旧版文件没有这个字段，按关闭处理。
    #[serde(default)]
    enabled: bool,
}

impl RelayCredentials {
    /// 只读不建：Daemon 启动时判断要不要自动连，文件不存在就是「从没开过」。
    pub(crate) fn load(home: &Path) -> Result<Option<Self>> {
        let path = home.join(CREDENTIALS_FILE);
        if !path.exists() {
            return Ok(None);
        }
        validate_secret_permissions(&path)?;
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("read relay credentials: {}", path.display()))?;
        let existing: Self = toml::from_str(&contents).context("parse mobile relay credentials")?;
        Ok(existing.is_compact().then_some(existing))
    }

    pub(crate) fn load_or_create(home: &Path) -> Result<Self> {
        if let Some(existing) = Self::load(home)? {
            return Ok(existing);
        }
        let credentials = Self::generate();
        credentials.save(home)?;
        Ok(credentials)
    }

    /// 打开或关闭中继并落盘。room 与 token 不变，已配对的手机不用重新扫码。
    pub(crate) fn save_enabled(home: &Path, enabled: bool) -> Result<Self> {
        let mut credentials = Self::load_or_create(home)?;
        if credentials.enabled != enabled {
            credentials.enabled = enabled;
            credentials.save(home)?;
        }
        Ok(credentials)
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// 先写临时文件、设好权限再 rename，不存在权限窗口。
    fn save(&self, home: &Path) -> Result<()> {
        std::fs::create_dir_all(home)?;
        let temporary = home.join(format!(".mobile-relay-{}.tmp", Uuid::new_v4()));
        std::fs::write(&temporary, toml::to_string_pretty(self)?)?;
        set_secret_permissions(&temporary)?;
        std::fs::rename(&temporary, home.join(CREDENTIALS_FILE))?;
        Ok(())
    }

    fn generate() -> Self {
        Self {
            relay_base_url: DEFAULT_RELAY_BASE_URL.to_owned(),
            room: format!("{ROOM_PREFIX}{}", Uuid::new_v4().simple()),
            token: random_token(),
            enabled: false,
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

    /// 中继服务的主机名，给状态展示用；不含 room 与 token。
    pub(crate) fn relay_host(&self) -> Option<String> {
        Url::parse(&self.relay_base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
    }

    fn websocket_url(&self) -> String {
        let base = self
            .relay_base_url
            .trim_end_matches('/')
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        format!("{base}/ws/broadcast/{}", self.room.trim_matches('/'))
    }

    pub(crate) fn websocket_request(
        &self,
    ) -> Result<http::Request<()>, tokio_tungstenite::tungstenite::Error> {
        let mut request = self.websocket_url().into_client_request()?;
        request.headers_mut().insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", self.token))?,
        );
        request.headers_mut().insert(
            "X-App-Version",
            HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
        );
        Ok(request)
    }

    /// `mobile-gateway.v1` 的紧凑配对 URL（手机端 `compactPairingPayloadJSON` 的输入）：
    /// `r` = relay room，`t` = relay token，`u` = relay base url（等于默认值时省略），
    /// `d` = 桌面名。协议版本不进二维码：`v` 缺省时手机按 `mobile-gateway.v1` 处理，
    /// 协议真升版时再补 `v`。手机会把这几个参数补全成完整的配对 JSON，
    /// `base_url`/`pairing_token` 由 `u`/`t` 推出，`expires_at`/`protocol_version` 取默认值——
    /// 所以这些字段没必要再进二维码。
    ///
    /// 相比原先直接编码完整 JSON（437 字节、81×81 模块），这里最多 118 字节、41×41 模块。
    pub(crate) fn pairing_url(&self) -> Result<String> {
        self.pairing_url_named(&desktop_name())
    }

    /// 桌面名由调用方给出，`pairing_url` 之外只有测试会用——桌面名长度取决于 `HOSTNAME`，
    /// 尺寸断言不能跟着环境走。
    fn pairing_url_named(&self, desktop_name: &str) -> Result<String> {
        let base = self.relay_base_url.trim_end_matches('/');
        let mut url = Url::parse(&format!("{base}/pair")).context("build mobile pairing URL")?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("r", &self.room);
            query.append_pair("t", &self.token);
            query.append_pair("d", desktop_name);
            // 手机端 `u` 缺省时按 DEFAULT_RELAY_BASE_URL 处理，自建中继才需要多带这一段。
            if base != DEFAULT_RELAY_BASE_URL {
                query.append_pair("u", base);
            }
        }
        Ok(url.to_string())
    }
}

/// 终端里一个模块占一个字符格，纠错等级越高模块越多。屏幕上的二维码不会被印污或折损，
/// L 级（7% 冗余）足够，比默认的 M 级少一到两个版本，宽度直接省掉十几列。
pub(crate) fn render_qr(payload: &str) -> Result<String> {
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
    // 按字节截断（在字符边界上切）：URL 里每个非 ASCII 字节要百分号转义成三个字符，
    // 按字符数截断的话一个中文主机名就能把二维码顶大一圈。
    if name.len() <= MAX_DESKTOP_NAME_LEN {
        return name;
    }
    let mut end = MAX_DESKTOP_NAME_LEN;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

fn random_token() -> String {
    Uuid::new_v4().simple().to_string()
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
    if mode & 0o077 != 0 {
        return Err(UnsafeCredentialPermissions { mode }.into());
    }
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
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message as WebSocketMessage;

    fn temp_home() -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!("willdeep-relay-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn credentials(base: &str) -> RelayCredentials {
        RelayCredentials {
            relay_base_url: base.to_owned(),
            room: "wd-test".to_owned(),
            token: "secret".to_owned(),
            enabled: false,
        }
    }

    #[test]
    fn pairing_url_matches_android_compact_contract() {
        let url = credentials(DEFAULT_RELAY_BASE_URL).pairing_url().unwrap();
        assert!(url.starts_with("https://j.niuwoai.com/pair?"), "{url}");
        assert!(url.contains("r=wd-test"), "{url}");
        assert!(url.contains("t=secret"), "{url}");
        // 默认中继地址由手机端补全，不进二维码。
        assert!(!url.contains("u="), "{url}");
    }

    #[test]
    fn self_hosted_relay_keeps_its_base_url_in_the_pairing_url() {
        let url = credentials("https://relay.example.com")
            .pairing_url()
            .unwrap();
        assert!(
            url.contains("u=https%3A%2F%2Frelay.example.com"),
            "自建中继地址必须随二维码下发：{url}"
        );
    }

    #[test]
    fn relay_host_names_the_server_without_room_or_token() {
        let host = credentials("https://relay.example.com/").relay_host();
        assert_eq!(host.as_deref(), Some("relay.example.com"));
    }

    /// 最坏情况：桌面名顶满 `MAX_DESKTOP_NAME_LEN` 字节，且每个字节都要百分号转义成三个字符。
    /// 真实主机名只会比这短，所以这就是二维码尺寸的上界。
    #[test]
    fn pairing_qr_fits_the_terminal_popup() {
        let credentials = RelayCredentials::generate();
        let worst_case_name = "中".repeat(MAX_DESKTOP_NAME_LEN / "中".len());
        let payload = credentials.pairing_url_named(&worst_case_name).unwrap();
        let (width, height) = qr_size(&payload);
        assert_eq!(
            (width, height),
            (MAX_QR_WIDTH, MAX_QR_HEIGHT),
            "配对二维码尺寸变了（载荷 {} 字节）",
            payload.len()
        );

        // 当前环境下的真实二维码不得超过这个上界。
        let (actual_width, actual_height) = qr_size(&credentials.pairing_url().unwrap());
        assert!(
            actual_width <= MAX_QR_WIDTH && actual_height <= MAX_QR_HEIGHT,
            "实际二维码 {actual_width}×{actual_height} 超出上界 {MAX_QR_WIDTH}×{MAX_QR_HEIGHT}"
        );
    }

    fn qr_size(payload: &str) -> (usize, usize) {
        let qr = render_qr(payload).unwrap();
        let width = qr
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or_default();
        (width, qr.lines().count())
    }

    #[test]
    fn legacy_credentials_are_recompacted_on_load() {
        let home = temp_home();
        let legacy = RelayCredentials {
            relay_base_url: DEFAULT_RELAY_BASE_URL.to_owned(),
            room: format!("willdeep-cli-{}", Uuid::new_v4()),
            token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            enabled: false,
        };
        assert!(!legacy.is_compact());
        let path = home.join(CREDENTIALS_FILE);
        std::fs::write(&path, toml::to_string_pretty(&legacy).unwrap()).unwrap();
        set_secret_permissions(&path).unwrap();

        let loaded = RelayCredentials::load_or_create(&home).unwrap();
        assert!(loaded.is_compact());
        assert_ne!(loaded.token, legacy.token);
        let reloaded = RelayCredentials::load_or_create(&home).unwrap();
        assert_eq!(reloaded.token, loaded.token, "紧凑凭据不应被反复重置");
        std::fs::remove_dir_all(&home).ok();
    }

    /// 0.81 及更早写出的文件没有 `enabled`：读出来是关闭，打开后 room 与 token 不变，
    /// 已配对的手机不用重新扫码。
    #[test]
    fn enabling_keeps_the_pairing_and_survives_a_reload() {
        let home = temp_home();
        let path = home.join(CREDENTIALS_FILE);
        std::fs::write(
            &path,
            "relay_base_url = \"https://j.niuwoai.com\"\nroom = \"wd-0123456789abcdef0123456789abcdef\"\ntoken = \"0123456789abcdef0123456789abcdef\"\n",
        )
        .unwrap();
        set_secret_permissions(&path).unwrap();

        let before = RelayCredentials::load(&home).unwrap().unwrap();
        assert!(!before.enabled(), "旧文件没有 enabled 字段，应按关闭处理");

        let enabled = RelayCredentials::save_enabled(&home, true).unwrap();
        assert!(enabled.enabled());
        assert_eq!(enabled.token, before.token);
        assert_eq!(enabled.room, before.room);
        assert!(RelayCredentials::load(&home).unwrap().unwrap().enabled());

        RelayCredentials::save_enabled(&home, false).unwrap();
        assert!(!RelayCredentials::load(&home).unwrap().unwrap().enabled());
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn missing_credentials_mean_the_relay_was_never_enabled() {
        let home = temp_home();
        assert!(RelayCredentials::load(&home).unwrap().is_none());
        std::fs::remove_dir_all(&home).ok();
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_permissions_are_reported_as_a_typed_error() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_home();
        let credentials = RelayCredentials::load_or_create(&home).unwrap();
        let path = home.join(CREDENTIALS_FILE);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = match RelayCredentials::load(&home) {
            Err(error) => error,
            Ok(_) => panic!("0644 的凭据文件必须被拒绝"),
        };
        let typed = error
            .downcast_ref::<UnsafeCredentialPermissions>()
            .expect("权限错误要能被识别出来");
        assert!(typed.to_string().contains("chmod 600"));
        assert!(!typed.to_string().contains(&credentials.token));
        std::fs::remove_dir_all(&home).ok();
    }

    #[tokio::test]
    #[ignore = "requires the public j.niuwoai.com relay"]
    async fn live_relay_broadcasts_between_two_authenticated_peers() {
        let credentials = RelayCredentials {
            relay_base_url: DEFAULT_RELAY_BASE_URL.to_owned(),
            room: format!("willdeep-cli-smoke-{}", Uuid::new_v4()),
            token: random_token(),
            enabled: true,
        };
        let (left, _) = tokio_tungstenite::connect_async(credentials.websocket_request().unwrap())
            .await
            .unwrap();
        let (right, _) = tokio_tungstenite::connect_async(credentials.websocket_request().unwrap())
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
