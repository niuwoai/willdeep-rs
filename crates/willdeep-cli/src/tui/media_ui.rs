//! Markdown links and images in the terminal UI.
//!
//! Remote images are fetched only after an explicit Enter/click. Downloads
//! share the core public-address guard, enforce redirect/size/decode limits,
//! and never execute a URL through a shell. Terminal-native image protocols
//! are selected once; an encoding failure rebuilds the preview as Unicode
//! halfblocks from the cached decoded image.

use super::*;
use futures_util::StreamExt;
use image::Limits;
use ratatui::Frame;
use ratatui_image::{
    StatefulImage,
    picker::{Picker, ProtocolType},
    thread::{ResizeRequest, ResizeResponse, ThreadProtocol},
};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION};
use std::collections::{HashSet, VecDeque};
use std::process::{Command, Stdio};

const MAX_MEDIA_BYTES: usize = 8 * 1024 * 1024;
const MAX_MEDIA_DIMENSION: u32 = 12_000;
const MAX_MEDIA_ALLOC: u64 = 96 * 1024 * 1024;
const MAX_MEDIA_REDIRECTS: usize = 5;
const MAX_MEDIA_CACHE_ITEMS: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MediaKind {
    Link,
    Image,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MediaItem {
    pub(super) kind: MediaKind,
    pub(super) label: String,
    pub(super) target: String,
}

#[derive(Clone, Debug)]
enum MediaView {
    List,
    Loading {
        target: String,
    },
    Preview {
        target: String,
        width: u32,
        height: u32,
    },
    Error {
        target: String,
        message: String,
    },
}

#[derive(Clone, Debug)]
struct MediaOverlay {
    items: Vec<MediaItem>,
    selected: usize,
    view: MediaView,
}

pub(super) enum MediaAction {
    None,
    OpenUrl(String),
    LoadImage(String),
}

pub(super) struct MediaState {
    overlay: Option<MediaOverlay>,
    picker: Picker,
    protocol: ThreadProtocol,
    resize_tx: mpsc::UnboundedSender<ResizeRequest>,
    decoded: Option<(String, DynamicImage)>,
    cache: VecDeque<(String, DynamicImage)>,
    protocol_name: &'static str,
    fallback_reason: Option<String>,
    pub(super) rect: Rect,
    hits: Vec<(u16, usize)>,
}

impl Default for MediaState {
    fn default() -> Self {
        let (resize_tx, _resize_rx) = mpsc::unbounded_channel();
        Self::with_picker(Picker::halfblocks(), resize_tx)
    }
}

impl MediaState {
    pub(super) fn detect(resize_tx: mpsc::UnboundedSender<ResizeRequest>) -> Self {
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        Self::with_picker(picker, resize_tx)
    }

    fn with_picker(picker: Picker, resize_tx: mpsc::UnboundedSender<ResizeRequest>) -> Self {
        let protocol_name = protocol_name(picker.protocol_type());
        Self {
            overlay: None,
            picker,
            protocol: ThreadProtocol::new(resize_tx.clone(), None),
            resize_tx,
            decoded: None,
            cache: VecDeque::new(),
            protocol_name,
            fallback_reason: None,
            rect: Rect::default(),
            hits: Vec::new(),
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.overlay.is_some()
    }

    pub(super) fn open(&mut self, transcript: &[String]) -> bool {
        let items = extract_media_items(transcript);
        if items.is_empty() {
            return false;
        }
        self.overlay = Some(MediaOverlay {
            items,
            selected: 0,
            view: MediaView::List,
        });
        true
    }

    pub(super) fn close(&mut self) {
        self.overlay = None;
        self.protocol.empty_protocol();
        self.decoded = None;
        self.fallback_reason = None;
        self.rect = Rect::default();
        self.hits.clear();
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> MediaAction {
        let Some(overlay) = self.overlay.as_mut() else {
            return MediaAction::None;
        };
        match &overlay.view {
            MediaView::List => match key.code {
                KeyCode::Esc => self.close(),
                KeyCode::Up => overlay.selected = overlay.selected.saturating_sub(1),
                KeyCode::Down => {
                    overlay.selected =
                        (overlay.selected + 1).min(overlay.items.len().saturating_sub(1))
                }
                KeyCode::Home => overlay.selected = 0,
                KeyCode::End => overlay.selected = overlay.items.len().saturating_sub(1),
                KeyCode::Enter => return self.activate_selected(),
                _ => {}
            },
            MediaView::Loading { .. } => {
                if key.code == KeyCode::Esc {
                    overlay.view = MediaView::List;
                }
            }
            MediaView::Preview { target, .. } => match key.code {
                KeyCode::Esc => {
                    self.protocol.empty_protocol();
                    self.decoded = None;
                    overlay.view = MediaView::List;
                }
                KeyCode::Char('o') | KeyCode::Char('O') => {
                    return MediaAction::OpenUrl(target.clone());
                }
                KeyCode::Char('u') | KeyCode::Char('U') => self.force_halfblocks(None),
                _ => {}
            },
            MediaView::Error { .. } => {
                if key.code == KeyCode::Esc {
                    overlay.view = MediaView::List;
                }
            }
        }
        MediaAction::None
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) -> MediaAction {
        let Some(overlay) = self.overlay.as_mut() else {
            return MediaAction::None;
        };
        if !matches!(overlay.view, MediaView::List) {
            return MediaAction::None;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => overlay.selected = overlay.selected.saturating_sub(1),
            MouseEventKind::ScrollDown => {
                overlay.selected = (overlay.selected + 1).min(overlay.items.len().saturating_sub(1))
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, index)) = self.hits.iter().find(|(row, _)| *row == mouse.row) {
                    overlay.selected = *index;
                    return self.activate_selected();
                }
            }
            _ => {}
        }
        MediaAction::None
    }

    fn activate_selected(&mut self) -> MediaAction {
        let Some(overlay) = self.overlay.as_mut() else {
            return MediaAction::None;
        };
        let Some(item) = overlay.items.get(overlay.selected).cloned() else {
            return MediaAction::None;
        };
        match item.kind {
            MediaKind::Link => MediaAction::OpenUrl(item.target),
            MediaKind::Image => {
                if let Some((_, image)) =
                    self.cache.iter().find(|(target, _)| target == &item.target)
                {
                    self.set_preview(item.target, image.clone());
                    MediaAction::None
                } else {
                    overlay.view = MediaView::Loading {
                        target: item.target.clone(),
                    };
                    MediaAction::LoadImage(item.target)
                }
            }
        }
    }

    pub(super) fn finish_load(&mut self, target: String, result: Result<DynamicImage, String>) {
        let waiting = self.overlay.as_ref().is_some_and(|overlay| {
            matches!(&overlay.view, MediaView::Loading { target: current } if current == &target)
        });
        if !waiting {
            return;
        }
        match result {
            Ok(image) => {
                self.cache.retain(|(cached, _)| cached != &target);
                self.cache.push_front((target.clone(), image.clone()));
                self.cache.truncate(MAX_MEDIA_CACHE_ITEMS);
                self.set_preview(target, image);
            }
            Err(message) => {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.view = MediaView::Error { target, message };
                }
            }
        }
    }

    fn set_preview(&mut self, target: String, image: DynamicImage) {
        let (width, height) = (image.width(), image.height());
        self.protocol
            .replace_protocol(self.picker.new_resize_protocol(image.clone()));
        self.decoded = Some((target.clone(), image));
        self.fallback_reason = None;
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.view = MediaView::Preview {
                target,
                width,
                height,
            };
        }
    }

    pub(super) fn finish_resize(
        &mut self,
        result: Result<ResizeResponse, String>,
    ) -> Option<String> {
        match result {
            Ok(response) => {
                self.protocol.update_resized_protocol(response);
                None
            }
            Err(error) if self.picker.protocol_type() != ProtocolType::Halfblocks => {
                self.force_halfblocks(Some(error.clone()));
                Some(error)
            }
            Err(error) => {
                if let Some(overlay) = self.overlay.as_mut()
                    && let MediaView::Preview { target, .. } = &overlay.view
                {
                    overlay.view = MediaView::Error {
                        target: target.clone(),
                        message: error.clone(),
                    };
                }
                Some(error)
            }
        }
    }

    fn force_halfblocks(&mut self, reason: Option<String>) {
        let Some((_, image)) = self.decoded.as_ref() else {
            return;
        };
        self.picker = Picker::halfblocks();
        self.protocol_name = protocol_name(ProtocolType::Halfblocks);
        self.fallback_reason = reason;
        self.protocol = ThreadProtocol::new(
            self.resize_tx.clone(),
            Some(self.picker.new_resize_protocol(image.clone())),
        );
    }
}

fn protocol_name(protocol: ProtocolType) -> &'static str {
    match protocol {
        ProtocolType::Kitty => "Kitty",
        ProtocolType::Iterm2 => "iTerm2",
        ProtocolType::Sixel => "Sixel",
        ProtocolType::Halfblocks => "Unicode halfblocks",
    }
}

pub(super) fn render_media_overlay(f: &mut Frame<'_>, app: &mut App) {
    app.media.rect = Rect::default();
    app.media.hits.clear();
    let Some(overlay) = app.media.overlay.as_ref() else {
        return;
    };
    let popup = centered_rect(
        f.area().width.saturating_sub(6).min(104),
        f.area().height.saturating_sub(4).clamp(8, 34),
        f.area(),
    );
    app.media.rect = popup;
    f.render_widget(Clear, popup);
    let title = match &overlay.view {
        MediaView::List => app
            .language
            .text(
                "链接与图片 · ↑/↓ 选择 · Enter 打开/预览 · Esc 关闭",
                "Links & images · ↑/↓ select · Enter open/preview · Esc close",
                "リンクと画像 · ↑/↓ 選択 · Enter 開く/表示 · Esc 閉じる",
            )
            .to_owned(),
        MediaView::Loading { .. } => app
            .language
            .text(
                "正在安全加载图片 · Esc 返回",
                "Loading image safely · Esc back",
                "画像を安全に読込中 · Esc 戻る",
            )
            .to_owned(),
        MediaView::Preview { width, height, .. } => format!(
            "{} · {}×{} · {} · O {} · U {} · Esc {}",
            app.language
                .text("图片预览", "Image preview", "画像プレビュー"),
            width,
            height,
            app.media.protocol_name,
            app.language
                .text("打开原图", "open original", "元画像を開く"),
            app.language.text("字符模式", "Unicode mode", "文字モード"),
            app.language.text("返回", "back", "戻る"),
        ),
        MediaView::Error { .. } => app
            .language
            .text(
                "图片加载失败 · Esc 返回",
                "Image load failed · Esc back",
                "画像読込失敗 · Esc 戻る",
            )
            .to_owned(),
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::LightCyan));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    match &overlay.view {
        MediaView::List => {
            let viewport = inner.height.max(1) as usize;
            let start = overlay
                .selected
                .saturating_sub(viewport.saturating_sub(1))
                .min(overlay.items.len().saturating_sub(viewport));
            let lines = overlay
                .items
                .iter()
                .enumerate()
                .skip(start)
                .take(viewport)
                .map(|(index, item)| {
                    let icon = if item.kind == MediaKind::Image {
                        "▧"
                    } else {
                        "↗"
                    };
                    let marker = if index == overlay.selected {
                        "▶"
                    } else {
                        " "
                    };
                    let target =
                        compact_target(&item.target, inner.width.saturating_sub(8) as usize);
                    let style = if index == overlay.selected {
                        Style::default()
                            .bg(Color::LightCyan)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD)
                    } else if item.kind == MediaKind::Image {
                        Style::default().fg(Color::LightGreen)
                    } else {
                        Style::default().fg(Color::LightBlue)
                    };
                    Line::styled(format!("{marker} {icon} {} · {target}", item.label), style)
                })
                .collect::<Vec<_>>();
            app.media.hits = (0..lines.len())
                .map(|index| (inner.y + index as u16, start + index))
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
        MediaView::Loading { target } => {
            f.render_widget(
                Paragraph::new(format!(
                    "{}\n\n{}",
                    app.language.text(
                        "正在验证地址、下载并解码…",
                        "Validating, downloading, and decoding…",
                        "アドレス検証・ダウンロード・デコード中…"
                    ),
                    compact_target(target, inner.width as usize)
                ))
                .wrap(Wrap { trim: false }),
                inner,
            );
        }
        MediaView::Preview { .. } => {
            f.render_stateful_widget(StatefulImage::default(), inner, &mut app.media.protocol);
            if let Some(reason) = app.media.fallback_reason.as_ref() {
                let row = Rect::new(inner.x, inner.y, inner.width, 1.min(inner.height));
                f.render_widget(
                    Paragraph::new(format!(
                        "{}: {}",
                        app.language.text(
                            "原生协议失败，已降级",
                            "Native protocol failed; downgraded",
                            "ネイティブ方式に失敗、フォールバック"
                        ),
                        compact_target(reason, inner.width.saturating_sub(2) as usize)
                    ))
                    .style(Style::default().fg(Color::Yellow)),
                    row,
                );
            }
        }
        MediaView::Error { target, message } => {
            f.render_widget(
                Paragraph::new(format!(
                    "{}\n\n{}",
                    compact_target(target, inner.width as usize),
                    message
                ))
                .style(Style::default().fg(Color::LightRed))
                .wrap(Wrap { trim: false }),
                inner,
            );
        }
    }
}

fn compact_target(value: &str, width: usize) -> String {
    let width = width.max(8);
    if value.chars().count() <= width {
        return value.to_owned();
    }
    let mut output = value
        .chars()
        .take(width.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

pub(super) fn extract_media_items(transcript: &[String]) -> Vec<MediaItem> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for entry in transcript {
        parse_markdown_media(entry, &mut output, &mut seen);
        parse_bare_urls(entry, &mut output, &mut seen);
    }
    output
}

fn parse_markdown_media(
    value: &str,
    output: &mut Vec<MediaItem>,
    seen: &mut HashSet<(bool, String)>,
) {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let image = bytes[index] == b'!' && bytes.get(index + 1) == Some(&b'[');
        let link = bytes[index] == b'[';
        if !image && !link {
            index += 1;
            continue;
        }
        let label_start = index + if image { 2 } else { 1 };
        let Some(label_tail) = value[label_start..].find("](") else {
            index += 1;
            continue;
        };
        let label_end = label_start + label_tail;
        let target_start = label_end + 2;
        let Some(target_tail) = value[target_start..].find(')') else {
            index += 1;
            continue;
        };
        let target_end = target_start + target_tail;
        let target = value[target_start..target_end].trim();
        if !target.is_empty() {
            let key = (image, target.to_owned());
            if seen.insert(key) {
                output.push(MediaItem {
                    kind: if image {
                        MediaKind::Image
                    } else {
                        MediaKind::Link
                    },
                    label: value[label_start..label_end].trim().to_owned(),
                    target: target.to_owned(),
                });
            }
        }
        index = target_end + 1;
    }
}

fn parse_bare_urls(value: &str, output: &mut Vec<MediaItem>, seen: &mut HashSet<(bool, String)>) {
    for word in value.split_whitespace() {
        let target = word.trim_matches(|character: char| {
            matches!(
                character,
                '<' | '>' | '(' | ')' | '[' | ']' | ',' | ';' | '.' | '!' | '?' | '\'' | '"'
            )
        });
        if !target.starts_with("https://") && !target.starts_with("http://") {
            continue;
        }
        if seen.insert((false, target.to_owned())) {
            output.push(MediaItem {
                kind: MediaKind::Link,
                label: target.to_owned(),
                target: target.to_owned(),
            });
        }
    }
}

pub(super) async fn load_image(
    target: &str,
    workspace: &std::path::Path,
) -> Result<DynamicImage, String> {
    let bytes = if target.starts_with("data:image/") {
        decode_data_image(target)?
    } else if target.starts_with("http://") || target.starts_with("https://") {
        download_image(target).await?
    } else {
        read_workspace_image(target, workspace).await?
    };
    decode_image(&bytes)
}

fn decode_data_image(target: &str) -> Result<Vec<u8>, String> {
    let (metadata, payload) = target
        .split_once(',')
        .ok_or_else(|| "invalid data image".to_owned())?;
    if !metadata.ends_with(";base64") {
        return Err("only base64 data images are supported".to_owned());
    }
    let estimated = payload.len().saturating_mul(3) / 4;
    if estimated > MAX_MEDIA_BYTES {
        return Err("image exceeds the 8 MiB limit".to_owned());
    }
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|error| format!("invalid base64 image: {error}"))
}

async fn read_workspace_image(
    target: &str,
    workspace: &std::path::Path,
) -> Result<Vec<u8>, String> {
    if target.starts_with("file:") {
        return Err("file: URLs are not supported; use a workspace-relative path".to_owned());
    }
    let root = tokio::fs::canonicalize(workspace)
        .await
        .map_err(|error| format!("cannot resolve workspace: {error}"))?;
    let requested = std::path::Path::new(target);
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let canonical = tokio::fs::canonicalize(&joined)
        .await
        .map_err(|error| format!("cannot resolve image path: {error}"))?;
    if !canonical.starts_with(&root) {
        return Err("local image path escapes the workspace".to_owned());
    }
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|error| format!("cannot inspect image: {error}"))?;
    if !metadata.is_file() {
        return Err("local image is not a regular file".to_owned());
    }
    if metadata.len() > MAX_MEDIA_BYTES as u64 {
        return Err("image exceeds the 8 MiB limit".to_owned());
    }
    tokio::fs::read(canonical)
        .await
        .map_err(|error| format!("cannot read image: {error}"))
}

async fn download_image(target: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| error.to_string())?;
    let mut url =
        reqwest::Url::parse(target).map_err(|error| format!("invalid image URL: {error}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs containing credentials are refused".to_owned());
    }
    let mut visited = HashSet::new();
    for redirect in 0..=MAX_MEDIA_REDIRECTS {
        willdeep_core::tools::validate_public_url(&url)
            .await
            .map_err(|error| error.to_string())?;
        let mut normalized = url.clone();
        normalized.set_fragment(None);
        if !visited.insert(normalized.to_string()) {
            return Err("redirect loop detected".to_owned());
        }
        let response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|error| format!("image request failed: {error}"))?;
        if response.status().is_redirection() {
            if redirect == MAX_MEDIA_REDIRECTS {
                return Err("image redirect limit exceeded".to_owned());
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "redirect has no valid Location".to_owned())?;
            let next = url
                .join(location)
                .map_err(|error| format!("invalid redirect: {error}"))?;
            if url.scheme() == "https" && next.scheme() == "http" {
                return Err("HTTPS to HTTP redirect downgrade is refused".to_owned());
            }
            url = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!("image server returned HTTP {}", response.status()));
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|size| size > MAX_MEDIA_BYTES)
        {
            return Err("image exceeds the 8 MiB limit".to_owned());
        }
        if response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                !value.starts_with("image/") && value != "application/octet-stream"
            })
        {
            return Err("response is not an image".to_owned());
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| format!("image download failed: {error}"))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_MEDIA_BYTES {
                return Err("image exceeds the 8 MiB limit".to_owned());
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(bytes);
    }
    Err("image redirect limit exceeded".to_owned())
}

fn decode_image(bytes: &[u8]) -> Result<DynamicImage, String> {
    let cursor = Cursor::new(bytes);
    let mut reader = image::ImageReader::new(cursor)
        .with_guessed_format()
        .map_err(|error| format!("unknown image format: {error}"))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_MEDIA_DIMENSION);
    limits.max_image_height = Some(MAX_MEDIA_DIMENSION);
    limits.max_alloc = Some(MAX_MEDIA_ALLOC);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|error| format!("cannot decode image: {error}"))
}

pub(super) fn open_external_url(target: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(target).map_err(|error| format!("invalid URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("only credential-free HTTP(S) URLs can be opened".to_owned());
    }
    let mut command = platform_open_command(url.as_str());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot open system browser: {error}"))
}

fn platform_open_command(target: &str) -> Command {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(target);
        command
    }
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new("xdg-open");
        command.arg(target);
        command
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", target]);
        command
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let mut command = Command::new("false");
        command.arg(target);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_images_links_and_bare_urls_without_duplicates() {
        let items = extract_media_items(&["WillDeep: ![chart](assets/chart.png) [docs](https://example.com/docs) https://example.com/docs".to_owned()]);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, MediaKind::Image);
        assert_eq!(items[0].target, "assets/chart.png");
        assert_eq!(items[1].kind, MediaKind::Link);
    }

    #[test]
    fn parser_keeps_same_target_when_one_is_an_image() {
        let items = extract_media_items(&[
            "![logo](https://example.com/a.png) [download](https://example.com/a.png)".to_owned(),
        ]);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn data_images_must_be_base64_and_bounded() {
        assert!(decode_data_image("data:image/png,abc").is_err());
        assert_eq!(
            decode_data_image("data:image/png;base64,aGk=").unwrap(),
            b"hi"
        );
    }

    #[test]
    fn external_open_rejects_non_http_and_credentials_before_spawning() {
        assert!(open_external_url("file:///tmp/a").is_err());
        assert!(open_external_url("https://user:secret@example.com/").is_err());
    }

    #[test]
    fn native_encoding_failure_rebuilds_unicode_preview() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let mut state = MediaState::with_picker(picker, tx);
        state.overlay = Some(MediaOverlay {
            items: vec![MediaItem {
                kind: MediaKind::Image,
                label: "pixel".to_owned(),
                target: "pixel.png".to_owned(),
            }],
            selected: 0,
            view: MediaView::List,
        });
        state.set_preview("pixel.png".to_owned(), DynamicImage::new_rgba8(1, 1));

        let warning = state.finish_resize(Err("kitty encode failed".to_owned()));

        assert_eq!(warning.as_deref(), Some("kitty encode failed"));
        assert_eq!(state.picker.protocol_type(), ProtocolType::Halfblocks);
        assert_eq!(state.protocol_name, "Unicode halfblocks");
        assert_eq!(
            state.fallback_reason.as_deref(),
            Some("kitty encode failed")
        );
    }

    #[test]
    fn markdown_image_is_rendered_as_a_visible_card() {
        let rendered = super::super::rendering::render_assistant_markdown(
            "See ![release chart](https://example.com/chart.png)",
            80,
        );
        let text = rendered
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("▧ release chart · Ctrl+L"));
        assert!(!text.contains("https://example.com/chart.png"));
    }
}
