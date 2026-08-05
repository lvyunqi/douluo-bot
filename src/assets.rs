use std::{
    collections::HashMap,
    fmt,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use crate::{
    catalog,
    config::{IllustrationConfig, IllustrationMode},
    message::MAX_INLINE_IMAGE_BYTES,
};

const MAX_DIRECT_ASSET_COUNT: usize = 256;
const MAX_DIRECT_ASSET_TOTAL_BYTES: usize = 64 * 1024 * 1024;

/// Immutable direct-mode image snapshot loaded before the plugin starts serving commands.
#[derive(Clone, Default)]
pub struct IllustrationAssets {
    entries: Arc<HashMap<String, Arc<[u8]>>>,
    total_bytes: usize,
}

impl fmt::Debug for IllustrationAssets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IllustrationAssets")
            .field("entry_count", &self.entries.len())
            .field("total_bytes", &self.total_bytes)
            .finish()
    }
}

impl IllustrationAssets {
    pub fn load(data_dir: &Path, config: &IllustrationConfig) -> Result<Self, String> {
        if !config.enabled
            || config.mode != IllustrationMode::Direct
            || config.direct_asset_root.is_empty()
        {
            return Ok(Self::default());
        }

        let requested_root = data_dir.join(&config.direct_asset_root);
        match requested_root.try_exists() {
            Ok(false) => return Ok(Self::default()),
            Err(_) => return Err("本地插图根目录无法访问".to_string()),
            Ok(true) => {}
        }
        let root_metadata = fs::symlink_metadata(&requested_root)
            .map_err(|_| "本地插图根目录无法访问".to_string())?;
        if root_metadata.file_type().is_symlink() {
            return Err("本地插图根目录不能是符号链接".to_string());
        }

        let canonical_data_dir = fs::canonicalize(data_dir)
            .map_err(|_| "QimenBot data_dir 无法访问，不能加载本地插图".to_string())?;
        let canonical_root =
            fs::canonicalize(&requested_root).map_err(|_| "本地插图根目录无法访问".to_string())?;
        if !canonical_root.starts_with(&canonical_data_dir) {
            return Err("本地插图根目录不能离开 data_dir".to_string());
        }
        if !fs::metadata(&canonical_root)
            .map_err(|_| "本地插图根目录无法访问".to_string())?
            .is_dir()
        {
            return Err("本地插图根目录必须是目录".to_string());
        }

        let mut entries = HashMap::new();
        let mut total_bytes = 0_usize;
        for asset_key in catalog::asset_keys() {
            let Some(bytes) = load_one(&canonical_root, asset_key) else {
                continue;
            };
            let next_total = total_bytes
                .checked_add(bytes.len())
                .ok_or_else(|| "本地插图总大小溢出".to_string())?;
            if entries.len() >= MAX_DIRECT_ASSET_COUNT || next_total > MAX_DIRECT_ASSET_TOTAL_BYTES
            {
                return Err("本地插图最多 256 个且总大小不能超过 64 MiB".to_string());
            }
            total_bytes = next_total;
            entries.insert(asset_key.to_string(), bytes);
        }

        Ok(Self {
            entries: Arc::new(entries),
            total_bytes,
        })
    }

    pub fn get(&self, asset_key: &str) -> Option<Arc<[u8]>> {
        self.entries.get(asset_key).cloned()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

fn load_one(root: &Path, asset_key: &str) -> Option<Arc<[u8]>> {
    let requested_path = root.join(asset_key);
    if path_contains_symlink(root, &requested_path) {
        return None;
    }
    let canonical_path = fs::canonicalize(&requested_path).ok()?;
    if !canonical_path.starts_with(root) {
        return None;
    }

    // Open the canonical path, then re-check the original path. This keeps the
    // snapshot bounded and rejects the common replace-with-symlink race; the
    // data_dir is still expected to be writable only by the host administrator.
    let mut file = File::open(&canonical_path).ok()?;
    if fs::canonicalize(&requested_path).ok()?.as_path() != canonical_path {
        return None;
    }
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_INLINE_IMAGE_BYTES as u64
    {
        return None;
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if file
        .by_ref()
        .take(MAX_INLINE_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 != metadata.len()
        || file.metadata().ok()?.len() != metadata.len()
    {
        return None;
    }

    let extension = allowed_extension(Path::new(asset_key))?;
    extension_matches_magic(&extension, &bytes).then(|| Arc::from(bytes))
}

fn path_contains_symlink(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    let mut cursor = PathBuf::from(root);
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return true;
        };
        cursor.push(segment);
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

fn allowed_extension(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
    )
    .then_some(extension)
}

fn extension_matches_magic(extension: &str, bytes: &[u8]) -> bool {
    match extension {
        "png" => bytes.len() >= 8 && bytes[..8] == [137, 80, 78, 71, 13, 10, 26, 10],
        "jpg" | "jpeg" => bytes.len() >= 3 && bytes[..3] == [0xff, 0xd8, 0xff],
        "webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "gif" => bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a"),
        "bmp" => bytes.len() >= 2 && &bytes[..2] == b"BM",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_only_manifest_assets_with_matching_magic() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let root = directory.path().join("douluo-game/assets");
        let valid = root.join("maps/holy-soul-village/cover.webp");
        fs::create_dir_all(valid.parent().expect("父目录")).expect("目录应创建");
        fs::write(&valid, b"RIFF\x04\x00\x00\x00WEBP").expect("测试资源应写入");
        fs::write(root.join("unknown.webp"), b"RIFF\x04\x00\x00\x00WEBP").expect("未知资源应写入");
        fs::write(
            root.join("maps/holy-soul-village/wrong.png"),
            b"not an image",
        )
        .expect("错误资源应写入");

        let assets = IllustrationAssets::load(directory.path(), &IllustrationConfig::default())
            .expect("本地资源应加载");
        assert_eq!(assets.len(), 1);
        assert_eq!(
            assets.get("maps/holy-soul-village/cover.webp").as_deref(),
            Some(b"RIFF\x04\x00\x00\x00WEBP".as_slice())
        );
        let debug = format!("{assets:?}");
        assert!(debug.contains("entry_count"));
        assert!(!debug.contains("RIFF"));
    }

    #[test]
    fn missing_or_remote_root_degrades_to_empty_snapshot() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let missing = IllustrationAssets::load(directory.path(), &IllustrationConfig::default())
            .expect("缺少目录时应降级");
        assert_eq!(missing.len(), 0);

        let remote = IllustrationConfig {
            mode: IllustrationMode::Remote,
            remote_base_url: "https://media.example.com".to_string(),
            ..IllustrationConfig::default()
        };
        let assets =
            IllustrationAssets::load(directory.path(), &remote).expect("远程模式不应读取本地目录");
        assert_eq!(assets.len(), 0);
    }

    #[test]
    fn supported_extensions_must_match_their_magic() {
        let cases: [(&str, &[u8]); 6] = [
            ("png", b"\x89PNG\r\n\x1a\n"),
            ("jpg", b"\xff\xd8\xff"),
            ("jpeg", b"\xff\xd8\xff"),
            ("webp", b"RIFF\x04\x00\x00\x00WEBP"),
            ("gif", b"GIF89a"),
            ("bmp", b"BM"),
        ];

        for (extension, bytes) in cases {
            assert!(
                extension_matches_magic(extension, bytes),
                "{extension} 签名应被接受"
            );
        }
        assert!(!extension_matches_magic("png", b"RIFF\x04\x00\x00\x00WEBP"));
        assert!(!extension_matches_magic("svg", b"<svg></svg>"));
    }

    #[test]
    fn skips_symlinked_manifest_files() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let root = directory.path().join("douluo-game/assets");
        let outside = directory.path().join("outside.webp");
        let link = root.join("maps/holy-soul-village/cover.webp");
        fs::create_dir_all(link.parent().expect("父目录")).expect("目录应创建");
        fs::write(&outside, b"RIFF\x04\x00\x00\x00WEBP").expect("外部资源应写入");

        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, &link);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&outside, &link);
        if linked.is_err() {
            // Windows CI may not grant symlink creation to the test account.
            return;
        }

        let assets = IllustrationAssets::load(directory.path(), &IllustrationConfig::default())
            .expect("符号链接不应让加载失败");
        assert!(assets.get("maps/holy-soul-village/cover.webp").is_none());
    }
}
