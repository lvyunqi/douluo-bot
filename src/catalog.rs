use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

use serde::Deserialize;

use crate::config::{is_safe_asset_key, validate_public_https_url};

const EMBEDDED_MANIFEST: &str = include_str!("../assets/illustrations.json");
const MANIFEST_SCHEMA_VERSION: u16 = 1;
const EXPECTED_BINDING_COUNT: usize = 19;

const EXPECTED_ASSET_KEYS: [&str; EXPECTED_BINDING_COUNT] = [
    "maps/holy-soul-village/cover.webp",
    "maps/tiandou-imperial-city/cover.webp",
    "maps/novice-village/cover.webp",
    "maps/sunset-forest/cover.webp",
    "maps/silves/cover.webp",
    "wuhun/lone-wolf/portrait.webp",
    "wuhun/carrot/portrait.webp",
    "wuhun/sickle/portrait.webp",
    "wuhun/bread/portrait.webp",
    "wuhun/dragon-god-bloodline/portrait.webp",
    "soul-beasts/slime/battle.webp",
    "soul-beasts/goblin/battle.webp",
    "soul-rings/white/icon.webp",
    "soul-rings/yellow/icon.webp",
    "soul-rings/purple/icon.webp",
    "soul-rings/black/icon.webp",
    "soul-rings/red/icon.webp",
    "soul-rings/orange/icon.webp",
    "soul-rings/gold/icon.webp",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    schema_version: u16,
    bindings: Vec<IllustrationBinding>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IllustrationBinding {
    pub entity_type: String,
    pub entity_key: String,
    pub media_role: String,
    pub asset_key: String,
    pub alt: String,
    pub display: DisplaySize,
    pub distribution: String,
    pub direct_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DisplaySize {
    pub width: u16,
    pub height: u16,
}

static MANIFEST: OnceLock<Result<ManifestFile, String>> = OnceLock::new();

fn manifest() -> &'static Result<ManifestFile, String> {
    MANIFEST.get_or_init(|| {
        let parsed = serde_json::from_str::<ManifestFile>(EMBEDDED_MANIFEST)
            .map_err(|error| format!("内置插图 manifest 无效：{error}"))?;
        validate_manifest(&parsed)?;
        Ok(parsed)
    })
}

pub(crate) fn validate_embedded_manifest() -> Result<(), String> {
    manifest().as_ref().map(|_| ()).map_err(Clone::clone)
}

pub(crate) fn binding(
    entity_type: &str,
    entity_key: &str,
    media_role: &str,
) -> Option<&'static IllustrationBinding> {
    manifest().as_ref().ok()?.bindings.iter().find(|binding| {
        binding.entity_type == entity_type
            && binding.entity_key == entity_key
            && binding.media_role == media_role
    })
}

/// 返回已经过启动校验的只读插图绑定；调用方必须自行限制可返回的字段。
pub(crate) fn bindings() -> Result<&'static [IllustrationBinding], String> {
    manifest()
        .as_ref()
        .map(|manifest| manifest.bindings.as_slice())
        .map_err(Clone::clone)
}

pub(crate) fn asset_keys() -> impl Iterator<Item = &'static str> {
    manifest().as_ref().ok().into_iter().flat_map(|manifest| {
        manifest
            .bindings
            .iter()
            .map(|binding| binding.asset_key.as_str())
    })
}

fn validate_manifest(manifest: &ManifestFile) -> Result<(), String> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(format!(
            "内置插图 manifest schema_version 必须为 {MANIFEST_SCHEMA_VERSION}"
        ));
    }
    if manifest.bindings.len() != EXPECTED_BINDING_COUNT {
        return Err(format!(
            "内置插图 manifest 必须包含 {EXPECTED_BINDING_COUNT} 条绑定"
        ));
    }

    let expected_keys = EXPECTED_ASSET_KEYS.into_iter().collect::<HashSet<_>>();
    let mut actual_keys = HashSet::with_capacity(manifest.bindings.len());
    let mut binding_ids = HashSet::with_capacity(manifest.bindings.len());
    let mut category_counts = HashMap::<&str, usize>::new();

    for binding in &manifest.bindings {
        if !matches!(
            binding.entity_type.as_str(),
            "map" | "wuhun" | "soul_beast" | "soul_ring"
        ) {
            return Err("内置插图 manifest 包含未知 entity_type".to_string());
        }
        let expected_role = match binding.entity_type.as_str() {
            "map" => "cover",
            "wuhun" => "portrait",
            "soul_beast" => "battle",
            "soul_ring" => "icon",
            _ => unreachable!("entity_type 已由上方校验"),
        };
        if binding.media_role != expected_role {
            return Err("内置插图 manifest 的 media_role 与 entity_type 不匹配".to_string());
        }
        if binding.entity_key.trim().is_empty()
            || binding.entity_key.chars().count() > 80
            || binding.entity_key.chars().any(char::is_control)
        {
            return Err("内置插图 manifest 的 entity_key 无效".to_string());
        }
        if binding.alt.trim().is_empty()
            || binding.alt.chars().count() > 80
            || binding.alt.chars().any(char::is_control)
        {
            return Err("内置插图 manifest 的 alt 无效".to_string());
        }
        if !is_safe_asset_key(&binding.asset_key) {
            return Err("内置插图 manifest 包含不安全 asset_key".to_string());
        }
        if !binding_ids.insert((
            binding.entity_type.as_str(),
            binding.entity_key.as_str(),
            binding.media_role.as_str(),
        )) {
            return Err("内置插图 manifest 包含重复实体绑定".to_string());
        }
        if !actual_keys.insert(binding.asset_key.as_str()) {
            return Err("内置插图 manifest 包含重复 asset_key".to_string());
        }
        if binding.distribution != "not_bundled" {
            return Err("公开插图 manifest 只能声明 not_bundled 资源".to_string());
        }
        if binding.display.width == 0
            || binding.display.height == 0
            || binding.display.width > 4096
            || binding.display.height > 4096
        {
            return Err("内置插图 manifest 的 display 尺寸无效".to_string());
        }
        if let Some(url) = &binding.direct_url {
            validate_public_https_url(url, true)
                .map_err(|_| "内置插图 manifest 的 direct_url 必须是公网 HTTPS".to_string())?;
        }
        *category_counts
            .entry(binding.entity_type.as_str())
            .or_default() += 1;
    }

    if actual_keys != expected_keys {
        return Err("内置插图 manifest 的 asset_key 集合不符合首批资源契约".to_string());
    }
    for (category, expected_count) in [
        ("map", 5),
        ("wuhun", 5),
        ("soul_beast", 2),
        ("soul_ring", 7),
    ] {
        if category_counts.get(category).copied().unwrap_or_default() != expected_count {
            return Err(format!("内置插图 manifest 的 {category} 数量不正确"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_manifest_has_expected_shape() {
        validate_embedded_manifest().expect("公开 manifest 应有效");
        assert_eq!(asset_keys().count(), EXPECTED_BINDING_COUNT);
        let holy_soul = binding("map", "圣魂村", "cover").expect("应有圣魂村绑定");
        assert_eq!(holy_soul.asset_key, "maps/holy-soul-village/cover.webp");
        assert_eq!(
            (holy_soul.display.width, holy_soul.display.height),
            (640, 360)
        );
        assert!(holy_soul.direct_url.is_none());
    }

    #[test]
    fn embedded_manifest_keeps_category_counts_and_unique_keys() {
        let parsed = serde_json::from_str::<ManifestFile>(EMBEDDED_MANIFEST).expect("JSON 应有效");
        let mut counts = HashMap::<&str, usize>::new();
        let mut keys = HashSet::new();
        for binding in &parsed.bindings {
            *counts.entry(binding.entity_type.as_str()).or_default() += 1;
            assert!(keys.insert(binding.asset_key.as_str()));
        }
        assert_eq!(counts.get("map"), Some(&5));
        assert_eq!(counts.get("wuhun"), Some(&5));
        assert_eq!(counts.get("soul_beast"), Some(&2));
        assert_eq!(counts.get("soul_ring"), Some(&7));
        assert_eq!(keys.len(), EXPECTED_BINDING_COUNT);
    }
}
