//! 插件页面问模型时附带的图片（桥 2.6.0 `ai.images`）。
//!
//! 与 macOS 宿主 `AgentPluginAIMedia` 同一套边界：
//! 1. 页面递的是**路径**。路径必须落在本插件自己的媒体目录里，拒绝符号链接——
//!    少了这道钳制，插件页就能让宿主读任意文件再发给第三方模型。
//! 2. 图片在宿主侧解码、限边、重编码成 JPEG，不把 8K PNG 原样塞进请求。
//!
//! 视频本宿主不支持：抽帧需要视频解码器，而这里没有。所以 `ai.videos`
//! 不进能力清单，请求里带了 `videoPaths` 就当场拒，不假装审过。

use std::io::Cursor;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use willdeep_core::MessageAttachment;

/// 一次请求最多附几张图，与 macOS 宿主同值。
pub(crate) const MAX_IMAGES: usize = 12;
/// 送进模型前把长边限到这里。
pub(crate) const MAX_IMAGE_PIXELS: u32 = 1_568;
const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_PATH_CHARS: usize = 4_096;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MediaError {
    OutsidePluginData(String),
    Unreadable(String),
}

impl MediaError {
    /// 与 `ai_complete` 其余错误同一种写法：机器可读的前缀加具体对象。
    pub(crate) fn code(&self) -> String {
        match self {
            Self::OutsidePluginData(path) => format!("mediaOutsidePluginData: {path}"),
            Self::Unreadable(name) => format!("unreadableMedia: {name}"),
        }
    }
}

/// 去空、去重、丢掉超长路径。数量上限由调用方按整个请求算。
pub(crate) fn clean_paths(paths: &[String]) -> Vec<String> {
    let mut cleaned: Vec<String> = Vec::new();
    for raw in paths {
        let path = raw.trim();
        if path.is_empty()
            || path.chars().count() > MAX_PATH_CHARS
            || cleaned.iter().any(|item| item == path)
        {
            continue;
        }
        cleaned.push(path.to_owned());
    }
    cleaned
}

/// 钳制到插件媒体目录：绝对路径、普通文件、不是符号链接、规范化后父目录恰好是媒体根。
pub(crate) fn clamp(raw: &str, media_root: &Path) -> Result<PathBuf, MediaError> {
    let outside = || MediaError::OutsidePluginData(raw.to_owned());
    let candidate = Path::new(raw);
    if !candidate.is_absolute() {
        return Err(outside());
    }
    let metadata = std::fs::symlink_metadata(candidate).map_err(|_| outside())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(outside());
    }
    let resolved = candidate.canonicalize().map_err(|_| outside())?;
    let root = media_root.canonicalize().map_err(|_| outside())?;
    if resolved.parent() != Some(root.as_path()) {
        return Err(outside());
    }
    Ok(resolved)
}

/// 读一张图，限边后编码成 JPEG 附件。
pub(crate) fn image_attachment(path: &Path) -> Result<MessageAttachment, MediaError> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("image")
        .to_owned();
    let unreadable = || MediaError::Unreadable(name.clone());
    let size = std::fs::metadata(path).map_err(|_| unreadable())?.len();
    if size == 0 || size > MAX_IMAGE_BYTES {
        return Err(unreadable());
    }
    let bytes = std::fs::read(path).map_err(|_| unreadable())?;
    let decoded = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|_| unreadable())?
        .decode()
        .map_err(|_| unreadable())?;
    let fitted = if decoded.width().max(decoded.height()) > MAX_IMAGE_PIXELS {
        decoded.resize(
            MAX_IMAGE_PIXELS,
            MAX_IMAGE_PIXELS,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    // JPEG 不带透明通道，先压成 RGB，否则编码器直接报错。
    let rgb = fitted.to_rgb8();
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 85)
        .encode_image(&rgb)
        .map_err(|_| unreadable())?;
    Ok(MessageAttachment::Image {
        name,
        media_type: "image/jpeg".to_owned(),
        data: base64::engine::general_purpose::STANDARD.encode(&encoded),
        width: rgb.width(),
        height: rgb.height(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("plugin-ai-media-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("media")).unwrap();
        root.canonicalize().unwrap()
    }

    fn write_png(path: &Path, width: u32, height: u32) {
        image::RgbaImage::from_pixel(width, height, image::Rgba([200, 40, 40, 128]))
            .save(path)
            .unwrap();
    }

    #[test]
    fn clean_paths_trims_dedupes_and_drops_empty() {
        let raw = vec![
            " /a.png ".to_owned(),
            "/a.png".to_owned(),
            String::new(),
            "/b.png".to_owned(),
        ];
        assert_eq!(
            clean_paths(&raw),
            vec!["/a.png".to_owned(), "/b.png".to_owned()]
        );
    }

    #[test]
    fn clamp_accepts_only_regular_files_directly_inside_the_media_root() {
        let base = temp_root("clamp");
        let media = base.join("media");
        let inside = media.join("a.png");
        write_png(&inside, 4, 4);
        let outside = base.join("secret.png");
        write_png(&outside, 4, 4);

        assert_eq!(clamp(inside.to_str().unwrap(), &media), Ok(inside.clone()));
        for raw in [
            outside.to_str().unwrap().to_owned(),
            format!("{}/../secret.png", media.display()),
            "a.png".to_owned(),
            media.to_str().unwrap().to_owned(),
        ] {
            assert_eq!(
                clamp(&raw, &media),
                Err(MediaError::OutsidePluginData(raw.clone())),
                "{raw}"
            );
        }
        #[cfg(unix)]
        {
            let link = media.join("link.png");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            let raw = link.to_str().unwrap().to_owned();
            assert_eq!(
                clamp(&raw, &media),
                Err(MediaError::OutsidePluginData(raw.clone()))
            );
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn large_images_are_downscaled_and_reencoded_as_jpeg() {
        let base = temp_root("resize");
        let path = base.join("media").join("big.png");
        write_png(&path, 3_000, 2_000);
        let MessageAttachment::Image {
            media_type,
            data,
            width,
            height,
            ..
        } = image_attachment(&path).unwrap()
        else {
            panic!("expected an image attachment");
        };
        assert_eq!(media_type, "image/jpeg");
        assert!(width.max(height) <= MAX_IMAGE_PIXELS);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Jpeg
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn garbage_is_reported_as_unreadable() {
        let base = temp_root("garbage");
        let path = base.join("media").join("fake.png");
        std::fs::write(&path, b"not an image").unwrap();
        assert_eq!(
            image_attachment(&path).err(),
            Some(MediaError::Unreadable("fake.png".to_owned()))
        );
        let _ = std::fs::remove_dir_all(base);
    }
}
