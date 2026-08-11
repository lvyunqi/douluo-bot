use std::{
    fs::{self, File, OpenOptions},
    io::{Cursor, ErrorKind, Write},
    path::{Component, Path, PathBuf},
};

use getrandom::fill;
use image::{GenericImageView, ImageFormat, ImageReader, Limits};

use crate::{
    catalog,
    config::{IllustrationConfig, IllustrationMode, is_safe_asset_key, is_safe_data_relative_path},
    message::MAX_INLINE_IMAGE_BYTES,
};

/// 管理端单次本地素材上传的输入和输出上限，与运行时内联图片上限保持一致。
pub(crate) const MAX_DIRECT_ASSET_UPLOAD_BYTES: usize = MAX_INLINE_IMAGE_BYTES;
const MAX_DIRECT_ASSET_WIDTH: u32 = 4_096;
const MAX_DIRECT_ASSET_HEIGHT: u32 = 4_096;
const MAX_DIRECT_ASSET_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_DIRECT_ASSET_DECODE_BYTES: u64 = MAX_DIRECT_ASSET_PIXELS * 4;

/// 已安全写入 direct 根目录的插图元数据，不包含磁盘路径或原始字节。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectAssetUploadReceipt {
    pub asset_key: String,
    pub byte_size: usize,
    pub height: u32,
    pub mime_type: &'static str,
    pub width: u32,
}

/// 上传失败分类用于让 HTTP 层保持稳定、脱敏的响应语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirectAssetUploadError {
    DirectModeUnavailable,
    InvalidAsset,
    InvalidImage,
    StorageUnavailable,
}

/// 将受限图片写入 direct 根目录。此操作不修改 catalog、游戏数据库或运行时预加载快照。
pub(crate) fn store_direct_asset(
    data_dir: &Path,
    config: &IllustrationConfig,
    asset_key: &str,
    input: &[u8],
) -> Result<DirectAssetUploadReceipt, DirectAssetUploadError> {
    validate_asset_key(asset_key)?;
    if input.is_empty() || input.len() > MAX_DIRECT_ASSET_UPLOAD_BYTES {
        return Err(DirectAssetUploadError::InvalidImage);
    }

    let (bytes, width, height) = decode_and_encode_webp(input)?;
    let root = direct_asset_root(data_dir, config)?;
    let destination = prepare_destination(&root, asset_key)?;
    replace_file(&root, &destination, &bytes)?;

    Ok(DirectAssetUploadReceipt {
        asset_key: asset_key.to_string(),
        byte_size: bytes.len(),
        height,
        mime_type: "image/webp",
        width,
    })
}

fn validate_asset_key(asset_key: &str) -> Result<(), DirectAssetUploadError> {
    if !is_safe_asset_key(asset_key) || !asset_key.ends_with(".webp") {
        return Err(DirectAssetUploadError::InvalidAsset);
    }
    let bindings = catalog::bindings().map_err(|_| DirectAssetUploadError::StorageUnavailable)?;
    bindings
        .iter()
        .any(|binding| binding.asset_key == asset_key)
        .then_some(())
        .ok_or(DirectAssetUploadError::InvalidAsset)
}

fn decode_and_encode_webp(input: &[u8]) -> Result<(Vec<u8>, u32, u32), DirectAssetUploadError> {
    let mut dimensions_reader = image_reader(input)?;
    let format = dimensions_reader
        .format()
        .ok_or(DirectAssetUploadError::InvalidImage)?;
    if !matches!(
        format,
        ImageFormat::Bmp | ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP
    ) {
        return Err(DirectAssetUploadError::InvalidImage);
    }
    dimensions_reader.limits(image_limits());
    let (width, height) = dimensions_reader
        .into_dimensions()
        .map_err(|_| DirectAssetUploadError::InvalidImage)?;
    if width == 0
        || height == 0
        || u64::from(width)
            .checked_mul(u64::from(height))
            .filter(|pixels| *pixels <= MAX_DIRECT_ASSET_PIXELS)
            .is_none()
    {
        return Err(DirectAssetUploadError::InvalidImage);
    }

    let mut image_reader = image_reader(input)?;
    image_reader.limits(image_limits());
    let image = image_reader
        .decode()
        .map_err(|_| DirectAssetUploadError::InvalidImage)?;
    if image.dimensions() != (width, height) {
        return Err(DirectAssetUploadError::InvalidImage);
    }

    let mut encoded = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut encoded), ImageFormat::WebP)
        .map_err(|_| DirectAssetUploadError::InvalidImage)?;
    if encoded.is_empty() || encoded.len() > MAX_DIRECT_ASSET_UPLOAD_BYTES {
        return Err(DirectAssetUploadError::InvalidImage);
    }
    Ok((encoded, width, height))
}

fn image_reader(input: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, DirectAssetUploadError> {
    ImageReader::new(Cursor::new(input))
        .with_guessed_format()
        .map_err(|_| DirectAssetUploadError::InvalidImage)
}

fn image_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIRECT_ASSET_WIDTH);
    limits.max_image_height = Some(MAX_DIRECT_ASSET_HEIGHT);
    limits.max_alloc = Some(MAX_DIRECT_ASSET_DECODE_BYTES);
    limits
}

fn direct_asset_root(
    data_dir: &Path,
    config: &IllustrationConfig,
) -> Result<PathBuf, DirectAssetUploadError> {
    if !config.enabled
        || config.mode != IllustrationMode::Direct
        || config.direct_asset_root.is_empty()
        || !is_safe_data_relative_path(&config.direct_asset_root)
    {
        return Err(DirectAssetUploadError::DirectModeUnavailable);
    }

    let canonical_data_dir =
        fs::canonicalize(data_dir).map_err(|_| DirectAssetUploadError::StorageUnavailable)?;
    if !fs::metadata(&canonical_data_dir)
        .map_err(|_| DirectAssetUploadError::StorageUnavailable)?
        .is_dir()
    {
        return Err(DirectAssetUploadError::StorageUnavailable);
    }

    create_relative_directory(&canonical_data_dir, Path::new(&config.direct_asset_root)).map_err(
        |error| match error {
            DirectAssetUploadError::InvalidAsset => DirectAssetUploadError::DirectModeUnavailable,
            error => error,
        },
    )
}

fn prepare_destination(root: &Path, asset_key: &str) -> Result<PathBuf, DirectAssetUploadError> {
    let relative = Path::new(asset_key);
    let file_name = relative
        .file_name()
        .ok_or(DirectAssetUploadError::InvalidAsset)?;
    let parent =
        create_relative_directory(root, relative.parent().unwrap_or_else(|| Path::new("")))?;
    let destination = parent.join(file_name);
    if path_contains_symlink(root, &destination)? {
        return Err(DirectAssetUploadError::StorageUnavailable);
    }
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(DirectAssetUploadError::StorageUnavailable)
        }
        Ok(_) => Ok(destination),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(destination),
        Err(_) => Err(DirectAssetUploadError::StorageUnavailable),
    }
}

/// 逐层创建受限目录，先拒绝现有符号链接，避免递归创建跟随根外链接。
fn create_relative_directory(
    root: &Path,
    relative_path: &Path,
) -> Result<PathBuf, DirectAssetUploadError> {
    let mut parent = root.to_path_buf();
    for component in relative_path.components() {
        let Component::Normal(segment) = component else {
            return Err(DirectAssetUploadError::InvalidAsset);
        };
        parent.push(segment);
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(DirectAssetUploadError::StorageUnavailable);
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                match fs::create_dir(&parent) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                    Err(_) => return Err(DirectAssetUploadError::StorageUnavailable),
                }
                let metadata = fs::symlink_metadata(&parent)
                    .map_err(|_| DirectAssetUploadError::StorageUnavailable)?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(DirectAssetUploadError::StorageUnavailable);
                }
            }
            Err(_) => return Err(DirectAssetUploadError::StorageUnavailable),
        }
        let canonical_parent =
            fs::canonicalize(&parent).map_err(|_| DirectAssetUploadError::StorageUnavailable)?;
        if !canonical_parent.starts_with(root) {
            return Err(DirectAssetUploadError::StorageUnavailable);
        }
        parent = canonical_parent;
    }
    Ok(parent)
}

fn path_contains_symlink(root: &Path, path: &Path) -> Result<bool, DirectAssetUploadError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| DirectAssetUploadError::StorageUnavailable)?;
    let mut cursor = PathBuf::from(root);
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(DirectAssetUploadError::StorageUnavailable);
        };
        cursor.push(segment);
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(DirectAssetUploadError::StorageUnavailable),
        }
    }
    Ok(false)
}

fn replace_file(
    root: &Path,
    destination: &Path,
    bytes: &[u8],
) -> Result<(), DirectAssetUploadError> {
    let parent = destination
        .parent()
        .ok_or(DirectAssetUploadError::StorageUnavailable)?;
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(DirectAssetUploadError::StorageUnavailable)?;
    let (temporary, mut file) = create_temporary_file(parent, file_name)?;
    let write_result = (|| {
        file.write_all(bytes)
            .map_err(|_| DirectAssetUploadError::StorageUnavailable)?;
        file.sync_all()
            .map_err(|_| DirectAssetUploadError::StorageUnavailable)?;
        drop(file);

        if path_contains_symlink(root, destination)? {
            return Err(DirectAssetUploadError::StorageUnavailable);
        }
        replace_destination(&temporary, destination)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn create_temporary_file(
    parent: &Path,
    file_name: &str,
) -> Result<(PathBuf, File), DirectAssetUploadError> {
    for _ in 0..8 {
        let mut random = [0_u8; 16];
        fill(&mut random).map_err(|_| DirectAssetUploadError::StorageUnavailable)?;
        let token = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let temporary = parent.join(format!(".{file_name}.{token}.upload"));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(DirectAssetUploadError::StorageUnavailable),
        }
    }
    Err(DirectAssetUploadError::StorageUnavailable)
}

fn replace_destination(temporary: &Path, destination: &Path) -> Result<(), DirectAssetUploadError> {
    #[cfg(windows)]
    if let Ok(metadata) = fs::symlink_metadata(destination) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(DirectAssetUploadError::StorageUnavailable);
        }
        fs::remove_file(destination).map_err(|_| DirectAssetUploadError::StorageUnavailable)?;
    }
    fs::rename(temporary, destination).map_err(|_| DirectAssetUploadError::StorageUnavailable)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use image::{DynamicImage, Rgba, RgbaImage};

    use super::*;

    const ASSET_KEY: &str = "maps/holy-soul-village/cover.webp";

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, Rgba([4, 8, 15, 255])));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("测试 PNG 应编码");
        bytes
    }

    #[test]
    fn stores_a_known_direct_asset_as_normalized_webp() {
        let directory = tempfile::tempdir().expect("临时目录");
        let receipt = store_direct_asset(
            directory.path(),
            &IllustrationConfig::default(),
            ASSET_KEY,
            &png(2, 3),
        )
        .expect("已知资源应写入");
        assert_eq!(receipt.asset_key, ASSET_KEY);
        assert_eq!(receipt.mime_type, "image/webp");
        assert_eq!((receipt.width, receipt.height), (2, 3));

        let stored = fs::read(directory.path().join("douluo-game/assets").join(ASSET_KEY))
            .expect("应读取规范化文件");
        assert!(stored.starts_with(b"RIFF"));
        assert_eq!(&stored[8..12], b"WEBP");
        assert_eq!(stored.len(), receipt.byte_size);
    }

    #[test]
    fn replaces_the_current_direct_asset_without_creating_history() {
        let directory = tempfile::tempdir().expect("临时目录");
        store_direct_asset(
            directory.path(),
            &IllustrationConfig::default(),
            ASSET_KEY,
            &png(1, 1),
        )
        .expect("首个资源应写入");
        let receipt = store_direct_asset(
            directory.path(),
            &IllustrationConfig::default(),
            ASSET_KEY,
            &png(3, 2),
        )
        .expect("同一资源应替换");
        assert_eq!((receipt.width, receipt.height), (3, 2));

        let stored = fs::read(directory.path().join("douluo-game/assets").join(ASSET_KEY))
            .expect("应读取替换后的文件");
        let image = ImageReader::new(Cursor::new(stored))
            .with_guessed_format()
            .expect("应识别 WebP")
            .decode()
            .expect("替换后的 WebP 应可解码");
        assert_eq!(image.dimensions(), (3, 2));
    }

    #[test]
    fn rejects_unknown_assets_disabled_mode_and_invalid_images() {
        let directory = tempfile::tempdir().expect("临时目录");
        assert_eq!(
            store_direct_asset(
                directory.path(),
                &IllustrationConfig::default(),
                "maps/unknown/cover.webp",
                &png(1, 1),
            ),
            Err(DirectAssetUploadError::InvalidAsset)
        );

        let disabled = IllustrationConfig {
            enabled: false,
            ..IllustrationConfig::default()
        };
        assert_eq!(
            store_direct_asset(directory.path(), &disabled, ASSET_KEY, &png(1, 1)),
            Err(DirectAssetUploadError::DirectModeUnavailable)
        );
        let remote = IllustrationConfig {
            mode: IllustrationMode::Remote,
            remote_base_url: "https://media.example.com".to_string(),
            ..IllustrationConfig::default()
        };
        assert_eq!(
            store_direct_asset(directory.path(), &remote, ASSET_KEY, &png(1, 1)),
            Err(DirectAssetUploadError::DirectModeUnavailable)
        );
        assert_eq!(
            store_direct_asset(
                directory.path(),
                &IllustrationConfig::default(),
                ASSET_KEY,
                b"not an image"
            ),
            Err(DirectAssetUploadError::InvalidImage)
        );
    }

    #[test]
    fn rejects_images_beyond_the_direct_pixel_limit() {
        let directory = tempfile::tempdir().expect("临时目录");
        assert_eq!(
            store_direct_asset(
                directory.path(),
                &IllustrationConfig::default(),
                ASSET_KEY,
                &png(MAX_DIRECT_ASSET_WIDTH + 1, 1),
            ),
            Err(DirectAssetUploadError::InvalidImage)
        );
    }

    #[test]
    fn refuses_a_symlinked_asset_directory_before_creating_any_external_path() {
        let directory = tempfile::tempdir().expect("临时目录");
        let root = directory.path().join("douluo-game/assets");
        let outside = directory.path().join("outside");
        fs::create_dir_all(&root).expect("本地插图根应创建");
        fs::create_dir_all(&outside).expect("外部目录应创建");

        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, root.join("maps"));
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&outside, root.join("maps"));
        if linked.is_err() {
            // Windows CI 可能不授予创建符号链接的权限。
            return;
        }

        assert_eq!(
            store_direct_asset(
                directory.path(),
                &IllustrationConfig::default(),
                ASSET_KEY,
                &png(1, 1),
            ),
            Err(DirectAssetUploadError::StorageUnavailable)
        );
        assert!(
            !outside.join("holy-soul-village").exists(),
            "符号链接外不能创建资源目录"
        );
    }

    #[test]
    fn refuses_a_symlinked_direct_root_before_creating_any_external_path() {
        let directory = tempfile::tempdir().expect("临时目录");
        let outside = directory.path().join("outside");
        fs::create_dir_all(&outside).expect("外部目录应创建");

        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, directory.path().join("douluo-game"));
        #[cfg(windows)]
        let linked =
            std::os::windows::fs::symlink_dir(&outside, directory.path().join("douluo-game"));
        if linked.is_err() {
            // Windows CI 可能不授予创建符号链接的权限。
            return;
        }

        assert_eq!(
            store_direct_asset(
                directory.path(),
                &IllustrationConfig::default(),
                ASSET_KEY,
                &png(1, 1),
            ),
            Err(DirectAssetUploadError::StorageUnavailable)
        );
        assert!(!outside.join("assets").exists(), "根外不能创建插图目录");
    }
}
