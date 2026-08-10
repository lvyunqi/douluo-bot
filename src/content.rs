//! 受控内容包的解析、路径边界与结构校验。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 单个内容包允许读取的最大字节数。
pub const MAX_PACKAGE_BYTES: usize = 2 * 1024 * 1024;

/// 首版中毒效果冻结到快照中的规则版本。
pub const POISON_RULE_VERSION: &str = "poison-v1";

/// 首版中毒仅在玩家攻击或释放魂技的直接伤害后结算。
pub const POISON_TICK_PHASE: &str = "after_player_damage";

/// 首版中毒固定每个符合条件的行动序列结算一次。
pub const POISON_TICK_INTERVAL: i64 = 1;

/// 单次中毒 tick 的目录上限，与 effect_definition.value 的数据库边界一致。
pub const MAX_POISON_TICK_DAMAGE: i64 = 1_000_000;

/// 首版眩晕效果冻结到快照中的规则版本。
pub const STUN_RULE_VERSION: &str = "stun-v1";

/// 首版眩晕只在玩家直接伤害后阻止本次魂兽反击。
pub const STUN_APPLY_PHASE: &str = "after_player_damage";

/// 首版眩晕的唯一结算结果是跳过魂兽反击。
pub const STUN_COUNTERATTACK_BEHAVIOR: &str = "skip";

/// 首版眩晕只允许单点控制，持续区间由 duration_rounds 冻结。
pub const STUN_VALUE: i64 = 1;

/// 首版眩晕的持续时间上限，避免内容包声明无法审计的超长控制。
pub const MAX_STUN_DURATION_ROUNDS: i64 = 10;

/// 首版序列护盾冻结到快照中的规则版本。
pub const SHIELD_RULE_VERSION: &str = "shield-v1";

/// 首版护盾在玩家直接伤害后保护本序列的魂兽反击。
pub const SHIELD_APPLY_PHASE: &str = "after_player_damage";

/// 首版护盾吸收本序列的一次魂兽反击。
pub const SHIELD_COUNTERATTACK_BEHAVIOR: &str = "absorb";

/// 首版护盾每个受影响序列吸收一次完整反击。
pub const SHIELD_VALUE: i64 = 1;

/// 首版护盾的持续时间上限，避免内容包声明无法审计的超长保护。
pub const MAX_SHIELD_DURATION_ROUNDS: i64 = 10;

/// 首版即时治疗冻结到快照中的规则版本。
pub const HEAL_RULE_VERSION: &str = "heal-v1";

/// 首版治疗在玩家对魂兽造成直接伤害后、魂兽反击前结算。
pub const HEAL_APPLY_PHASE: &str = "after_player_damage";

/// 首版治疗溢出时只恢复到玩家最大生命。
pub const HEAL_OVERFLOW_BEHAVIOR: &str = "cap_at_max_hp";

/// 首版治疗的单次恢复量上限，与 effect_definition.value 的数据库边界一致。
pub const MAX_HEAL_AMOUNT: i64 = 1_000_000;

/// 首版目标选择冻结到快照中的规则版本。
pub const TARGET_RULE_VERSION: &str = "target-v1";

/// 首版只选择当前战斗已经冻结的魂兽目标。
pub const TARGET_SELECTOR: &str = "current_battle_beast";

/// 首版目标选择节点的标记值。
pub const TARGET_SELECTION_VALUE: i64 = 1;

/// 首版禁技效果冻结到快照中的规则版本。
pub const FORBID_SKILL_RULE_VERSION: &str = "forbid-skill-v1";

/// 首版禁技在玩家行动前检查。
pub const FORBID_SKILL_APPLY_PHASE: &str = "before_player_action";

/// 首版禁技只阻止魂技释放动作。
pub const FORBID_SKILL_BLOCKED_ACTION: &str = "release_skill";

/// 首版禁技的受控标记值。
pub const FORBID_SKILL_VALUE: i64 = 1;

/// 禁技至少覆盖施放后的下一条行动序列。
pub const MIN_FORBID_SKILL_DURATION_ROUNDS: i64 = 2;

/// 首版禁技持续时间上限，避免内容包声明无法审计的超长控制。
pub const MAX_FORBID_SKILL_DURATION_ROUNDS: i64 = 10;

/// 角色等级经验规则的只读展示引用。
pub const PLAYER_LEVEL_EXP_CURVE_REFERENCE: &str = "player-level-exp-v1";

/// 魂技熟练度规则的只读展示引用。
pub const SKILL_PROFICIENCY_CURVE_REFERENCE: &str = "skill-proficiency-v1";

/// 魂技等级伤害倍率规则的只读展示引用。
pub const SKILL_DAMAGE_PERCENT_CURVE_REFERENCE: &str = "skill-damage-percent-v1";

/// 可发布目录数据的文件格式。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContentPackage {
    pub package_key: String,
    pub revision: i64,
    pub author: String,
    #[serde(default)]
    pub minimum_runtime: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maps: Vec<MapPackageEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ItemPackageEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub npcs: Vec<NpcPackageEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quests: Vec<QuestPackageEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub numeric_curves: Vec<NumericCurvePackageEntry>,
    #[serde(default)]
    pub wuhun: Vec<WuhunPackageEntry>,
    #[serde(default)]
    pub skills: Vec<SkillPackageEntry>,
    #[serde(default)]
    pub effects: Vec<EffectPackageEntry>,
    #[serde(default)]
    pub soul_beasts: Vec<SoulBeastPackageEntry>,
    #[serde(default)]
    pub soul_rings: Vec<SoulRingPackageEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<ContentTransitionPackageEntry>,
}

/// 内容包声明的旧 key 弃用或替换关系。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContentTransitionPackageEntry {
    pub entity_kind: String,
    pub source_key: String,
    #[serde(default)]
    pub target_key: Option<String>,
    pub transition_kind: String,
    pub reason: String,
}

/// 内容包中的静态地图目录行，不承载出口或玩家位置。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MapPackageEntry {
    pub map_key: String,
    pub name: String,
    pub description: String,
    pub level_required: i64,
    pub safe: bool,
    pub pvp_enabled: bool,
    pub teleport_enabled: bool,
    pub sort_order: i64,
}

/// 内容包中的静态物品目录行；只允许追加新键，不承载玩家背包数量。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ItemPackageEntry {
    pub item_key: String,
    pub name: String,
    pub category: String,
    pub quality: i64,
    pub stackable: bool,
    pub max_stack: i64,
    pub buy_price: i64,
    pub sell_price: i64,
    pub level_required: i64,
    pub effect_kind: String,
    pub effect_amount: i64,
    pub revive_hp_percent: i64,
    pub purchasable: bool,
    pub sellable: bool,
    pub usable: bool,
    pub description: String,
}

/// 内容包中的静态 NPC 目录行；不承载商店库存或玩家对话绑定。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NpcPackageEntry {
    pub npc_key: String,
    pub map_key: String,
    pub name: String,
    pub npc_kind: String,
    pub dialogue: String,
    pub description: String,
    pub enabled: bool,
    pub sort_order: i64,
}

/// 内容包中的静态任务目录行；不承载玩家任务、进度或奖励发放状态。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestPackageEntry {
    pub quest_key: String,
    pub name: String,
    pub description: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_key: Option<String>,
    pub level_required: i64,
    pub repeatable: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub requirements: Vec<QuestRequirementPackageEntry>,
    pub rewards: Vec<QuestRewardPackageEntry>,
}

/// 静态任务的受控完成条件。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestRequirementPackageEntry {
    pub requirement_kind: String,
    pub target_key: String,
    pub required_quantity: i64,
    pub sort_order: i64,
    pub description: String,
}

/// 静态任务的受控奖励目录行。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestRewardPackageEntry {
    pub reward_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_key: Option<String>,
    pub amount: i64,
    pub sort_order: i64,
    pub description: String,
}

/// 内容包中的静态数值曲线展示目录；不承载公式、阈值或玩家状态。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NumericCurvePackageEntry {
    pub curve_key: String,
    pub name: String,
    pub unit: String,
    pub range_min: i64,
    pub range_max: i64,
    pub reference_key: String,
    pub description: String,
    pub sort_order: i64,
}

/// 内容包中的武魂及其属性模板。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WuhunPackageEntry {
    pub name: String,
    pub category: String,
    pub form: String,
    pub description: String,
    pub weight: i64,
    pub stats: WuhunStatsPackageEntry,
}

/// 武魂属性模板的百分比字段。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WuhunStatsPackageEntry {
    pub attack_percent: i64,
    pub defense_percent: i64,
    pub strength_percent: i64,
    pub agility_percent: i64,
    pub spirit_percent: i64,
    pub endurance_percent: i64,
    pub perception_percent: i64,
    pub luck_percent: i64,
}

/// 内容包中的魂技目录行。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SkillPackageEntry {
    pub skill_key: String,
    pub name: String,
    pub skill_type: String,
    pub wuhun_category: String,
    pub ring_index: i64,
    pub soul_power_cost: i64,
    pub cooldown_rounds: i64,
    pub base_damage: i64,
    pub spirit_ratio_percent: i64,
    pub strength_ratio_percent: i64,
    pub description: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub starter: bool,
}

/// 内容包中的受控效果 DSL 定义。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectPackageEntry {
    pub effect_key: String,
    pub skill_key: String,
    pub trigger_kind: String,
    pub target_kind: String,
    pub operation: String,
    pub attribute_key: String,
    pub value_mode: String,
    pub value: i64,
    pub duration_rounds: i64,
    #[serde(default = "default_chance")]
    pub chance_percent: i64,
    pub stack_policy: String,
    #[serde(default)]
    pub parameters: BTreeMap<String, Value>,
    pub description: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// 内容包中的可挑战魂兽目录行。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SoulBeastPackageEntry {
    pub beast_key: String,
    pub name: String,
    pub description: String,
    pub map_key: String,
    pub age: i64,
    pub level_required: i64,
    pub max_hp: i64,
    pub attack: i64,
    pub defense: i64,
    pub speed: i64,
    pub exp_reward: i64,
    pub drop_item_key: String,
    pub drop_quantity: i64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// 内容包中的魂环目录行。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SoulRingPackageEntry {
    pub ring_key: String,
    pub name: String,
    pub soul_beast_key: String,
    pub skill_key: String,
    pub ring_index: i64,
    pub age: i64,
    pub color: String,
    pub description: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// 已完成解析、结构校验与哈希计算的内容包。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedContentPackage {
    pub package: ContentPackage,
    pub content_hash: String,
    pub source_format: String,
}

fn default_enabled() -> bool {
    true
}

fn default_chance() -> i64 {
    100
}

/// 判断内容包参数是否精确声明首版中毒规则，拒绝任意脚本或附加开关。
pub fn is_poison_v1_parameters(parameters: &BTreeMap<String, Value>) -> bool {
    parameters.len() == 3
        && parameters.get("rule_version").and_then(Value::as_str) == Some(POISON_RULE_VERSION)
        && parameters.get("tick_phase").and_then(Value::as_str) == Some(POISON_TICK_PHASE)
        && parameters.get("tick_interval").and_then(Value::as_i64) == Some(POISON_TICK_INTERVAL)
}

/// 判断内容包参数是否精确声明首版眩晕规则，拒绝任意脚本或附加开关。
pub fn is_stun_v1_parameters(parameters: &BTreeMap<String, Value>) -> bool {
    parameters.len() == 3
        && parameters.get("rule_version").and_then(Value::as_str) == Some(STUN_RULE_VERSION)
        && parameters.get("apply_phase").and_then(Value::as_str) == Some(STUN_APPLY_PHASE)
        && parameters.get("counterattack").and_then(Value::as_str)
            == Some(STUN_COUNTERATTACK_BEHAVIOR)
}

/// 判断内容包参数是否精确声明首版护盾规则，拒绝任意脚本或附加开关。
pub fn is_shield_v1_parameters(parameters: &BTreeMap<String, Value>) -> bool {
    parameters.len() == 3
        && parameters.get("rule_version").and_then(Value::as_str) == Some(SHIELD_RULE_VERSION)
        && parameters.get("apply_phase").and_then(Value::as_str) == Some(SHIELD_APPLY_PHASE)
        && parameters.get("counterattack").and_then(Value::as_str)
            == Some(SHIELD_COUNTERATTACK_BEHAVIOR)
}

/// 判断内容包参数是否精确声明首版治疗规则，拒绝任意脚本或附加开关。
pub fn is_heal_v1_parameters(parameters: &BTreeMap<String, Value>) -> bool {
    parameters.len() == 3
        && parameters.get("rule_version").and_then(Value::as_str) == Some(HEAL_RULE_VERSION)
        && parameters.get("apply_phase").and_then(Value::as_str) == Some(HEAL_APPLY_PHASE)
        && parameters.get("overflow").and_then(Value::as_str) == Some(HEAL_OVERFLOW_BEHAVIOR)
}

/// 判断内容包参数是否精确声明首版当前战斗目标选择规则。
pub fn is_target_v1_parameters(parameters: &BTreeMap<String, Value>) -> bool {
    parameters.len() == 2
        && parameters.get("rule_version").and_then(Value::as_str) == Some(TARGET_RULE_VERSION)
        && parameters.get("selector").and_then(Value::as_str) == Some(TARGET_SELECTOR)
}

/// 判断内容包参数是否精确声明首版玩家禁技规则。
pub fn is_forbid_skill_v1_parameters(parameters: &BTreeMap<String, Value>) -> bool {
    parameters.len() == 3
        && parameters.get("rule_version").and_then(Value::as_str) == Some(FORBID_SKILL_RULE_VERSION)
        && parameters.get("apply_phase").and_then(Value::as_str) == Some(FORBID_SKILL_APPLY_PHASE)
        && parameters.get("blocked_action").and_then(Value::as_str)
            == Some(FORBID_SKILL_BLOCKED_ACTION)
}

/// 将内容包序列化为用于持久化和哈希的稳定 JSON 表示。
pub fn canonical_json(package: &ContentPackage) -> Result<String, String> {
    serde_json::to_string(package).map_err(|error| format!("序列化内容包失败：{error}"))
}

/// 计算规范 JSON 的 SHA-256 内容指纹。
pub fn content_hash(package: &ContentPackage) -> Result<String, String> {
    let canonical = canonical_json(package)?;
    let digest = Sha256::digest(canonical.as_bytes());
    Ok(format!("{digest:x}"))
}

/// 按来源格式解析文本内容包，并在返回前完成字段结构校验。
pub fn parse_package_text(text: &str, source_format: &str) -> Result<LoadedContentPackage, String> {
    if text.len() > MAX_PACKAGE_BYTES {
        return Err(format!(
            "内容包超过 {} MiB 大小上限",
            MAX_PACKAGE_BYTES / 1024 / 1024
        ));
    }
    let package = match source_format {
        "json" => serde_json::from_str::<ContentPackage>(text)
            .map_err(|error| format!("JSON 内容包无效：{error}"))?,
        "toml" => toml::from_str::<ContentPackage>(text)
            .map_err(|error| format!("TOML 内容包无效：{error}"))?,
        _ => return Err("内容包扩展名必须是 .json 或 .toml".to_string()),
    };
    let errors = validate_shape(&package);
    if !errors.is_empty() {
        return Err(format!("内容包字段校验失败：{}", errors.join("；")));
    }
    Ok(LoadedContentPackage {
        content_hash: content_hash(&package)?,
        package,
        source_format: source_format.to_string(),
    })
}

/// 从 data_dir 内的普通文件有界读取并解析内容包。
pub fn load_package_file(
    data_dir: &Path,
    relative_path: &str,
) -> Result<LoadedContentPackage, String> {
    let root =
        fs::canonicalize(data_dir).map_err(|error| format!("解析内容包根目录失败：{error}"))?;
    let candidate = data_dir.join(relative_path);
    let metadata =
        fs::symlink_metadata(&candidate).map_err(|error| format!("读取内容包文件失败：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("内容包文件不能是符号链接".to_string());
    }
    if !metadata.is_file() {
        return Err("内容包必须是常规文件".to_string());
    }
    if metadata.len() > MAX_PACKAGE_BYTES as u64 {
        return Err(format!(
            "内容包超过 {} MiB 大小上限",
            MAX_PACKAGE_BYTES / 1024 / 1024
        ));
    }
    let path =
        fs::canonicalize(&candidate).map_err(|error| format!("解析内容包文件路径失败：{error}"))?;
    if !path.starts_with(&root) {
        return Err("内容包文件必须位于 QimenBot data_dir 内".to_string());
    }
    let mut file = fs::File::open(&path).map_err(|error| format!("读取内容包文件失败：{error}"))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_PACKAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取内容包文件失败：{error}"))?;
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(format!(
            "内容包超过 {} MiB 大小上限",
            MAX_PACKAGE_BYTES / 1024 / 1024
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| "内容包必须是 UTF-8 文本".to_string())?;
    let source_format = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .ok_or_else(|| "内容包文件必须使用 .json 或 .toml 扩展名".to_string())?;
    parse_package_text(&text, &source_format)
}

/// 返回内容包的全部结构错误，不访问数据库目录。
pub fn validate_shape(package: &ContentPackage) -> Vec<String> {
    let mut errors = Vec::new();
    text_field(&mut errors, "package_key", &package.package_key, 96, true);
    if !is_content_key(&package.package_key) {
        errors.push("package_key 必须是小写字母、数字、点、下划线或横线组成的键".to_string());
    }
    if package.revision <= 0 {
        errors.push("revision 必须大于 0".to_string());
    }
    text_field(&mut errors, "author", &package.author, 200, true);
    text_field(
        &mut errors,
        "minimum_runtime",
        &package.minimum_runtime,
        64,
        false,
    );
    let total = package.maps.len()
        + package.items.len()
        + package.npcs.len()
        + package.quests.len()
        + package.numeric_curves.len()
        + package.wuhun.len()
        + package.skills.len()
        + package.effects.len()
        + package.soul_beasts.len()
        + package.soul_rings.len()
        + package.transitions.len();
    if total == 0 {
        errors.push("内容包至少要包含一条目录数据".to_string());
    }
    if total > 10_000 {
        errors.push("内容包目录行数不能超过 10000".to_string());
    }

    let mut keys = BTreeSet::new();
    let mut map_names = BTreeSet::new();
    let mut map_sort_orders = BTreeSet::new();
    for entry in &package.maps {
        if !keys.insert(format!("map:{}", entry.map_key)) {
            errors.push(format!("地图键重复：{}", entry.map_key));
        }
        if !map_names.insert(entry.name.as_str()) {
            errors.push(format!("地图名称重复：{}", entry.name));
        }
        if !map_sort_orders.insert(entry.sort_order) {
            errors.push(format!("地图排序重复：{}", entry.sort_order));
        }
        validate_key(&mut errors, "map.map_key", &entry.map_key);
        text_field(&mut errors, "map.name", &entry.name, 128, true);
        text_field(
            &mut errors,
            "map.description",
            &entry.description,
            2000,
            true,
        );
        range_field(
            &mut errors,
            "map.level_required",
            &entry.map_key,
            entry.level_required,
            1,
            120,
        );
        if entry.sort_order < 0 {
            errors.push(format!("地图 {} 的 sort_order 不能为负数", entry.map_key));
        }
    }

    keys.clear();
    let mut item_names = BTreeSet::new();
    for entry in &package.items {
        if !keys.insert(format!("item:{}", entry.item_key)) {
            errors.push(format!("物品键重复：{}", entry.item_key));
        }
        if !item_names.insert(entry.name.as_str()) {
            errors.push(format!("物品名称重复：{}", entry.name));
        }
        validate_key(&mut errors, "item.item_key", &entry.item_key);
        text_field(&mut errors, "item.name", &entry.name, 128, true);
        if !matches!(entry.category.as_str(), "revival" | "consumable") {
            errors.push(format!("物品 {} 的 category 不受支持", entry.item_key));
        }
        range_field(
            &mut errors,
            "item.quality",
            &entry.item_key,
            entry.quality,
            1,
            5,
        );
        range_field(
            &mut errors,
            "item.max_stack",
            &entry.item_key,
            entry.max_stack,
            1,
            9999,
        );
        if !entry.stackable && entry.max_stack != 1 {
            errors.push(format!(
                "物品 {} 不可堆叠时 max_stack 必须为 1",
                entry.item_key
            ));
        }
        if entry.buy_price < 0 {
            errors.push(format!("物品 {} 的 buy_price 不能为负数", entry.item_key));
        }
        if entry.sell_price < 0 || entry.sell_price > entry.buy_price {
            errors.push(format!(
                "物品 {} 的 sell_price 必须在 0 到 buy_price 之间",
                entry.item_key
            ));
        }
        range_field(
            &mut errors,
            "item.level_required",
            &entry.item_key,
            entry.level_required,
            1,
            120,
        );
        if !matches!(
            entry.effect_kind.as_str(),
            "revive" | "restore_hp" | "restore_soul"
        ) {
            errors.push(format!("物品 {} 的 effect_kind 不受支持", entry.item_key));
        }
        match entry.effect_kind.as_str() {
            "revive" => {
                if entry.effect_amount != 0 {
                    errors.push(format!(
                        "复活物品 {} 的 effect_amount 必须为 0",
                        entry.item_key
                    ));
                }
                range_field(
                    &mut errors,
                    "item.revive_hp_percent",
                    &entry.item_key,
                    entry.revive_hp_percent,
                    1,
                    100,
                );
            }
            "restore_hp" | "restore_soul" => {
                range_field(
                    &mut errors,
                    "item.effect_amount",
                    &entry.item_key,
                    entry.effect_amount,
                    1,
                    1_000_000,
                );
                if entry.revive_hp_percent != 0 {
                    errors.push(format!(
                        "恢复物品 {} 的 revive_hp_percent 必须为 0",
                        entry.item_key
                    ));
                }
            }
            _ => {}
        }
        text_field(
            &mut errors,
            "item.description",
            &entry.description,
            2000,
            false,
        );
    }

    keys.clear();
    let mut npc_names = BTreeSet::new();
    for entry in &package.npcs {
        if !keys.insert(format!("npc:{}", entry.npc_key)) {
            errors.push(format!("NPC 键重复：{}", entry.npc_key));
        }
        if !npc_names.insert((entry.map_key.as_str(), entry.name.as_str())) {
            errors.push(format!(
                "地图 {} 的 NPC 名称重复：{}",
                entry.map_key, entry.name
            ));
        }
        validate_key(&mut errors, "npc.npc_key", &entry.npc_key);
        validate_key(&mut errors, "npc.map_key", &entry.map_key);
        text_field(&mut errors, "npc.name", &entry.name, 128, true);
        if !matches!(entry.npc_kind.as_str(), "elder" | "merchant") {
            errors.push(format!("NPC {} 的 npc_kind 不受支持", entry.npc_key));
        }
        text_field(&mut errors, "npc.dialogue", &entry.dialogue, 2000, false);
        text_field(
            &mut errors,
            "npc.description",
            &entry.description,
            2000,
            false,
        );
        if !entry.enabled {
            errors.push(format!("当前发布切片不允许禁用新 NPC：{}", entry.npc_key));
        }
        if entry.sort_order < 0 {
            errors.push(format!("NPC {} 的 sort_order 不能为负数", entry.npc_key));
        }
    }

    keys.clear();
    let mut quest_names = BTreeSet::new();
    for entry in &package.quests {
        if !keys.insert(format!("quest:{}", entry.quest_key)) {
            errors.push(format!("任务键重复：{}", entry.quest_key));
        }
        if !quest_names.insert(entry.name.as_str()) {
            errors.push(format!("任务名称重复：{}", entry.name));
        }
        validate_key(&mut errors, "quest.quest_key", &entry.quest_key);
        text_field(&mut errors, "quest.name", &entry.name, 128, true);
        text_field(
            &mut errors,
            "quest.description",
            &entry.description,
            2000,
            true,
        );
        if !matches!(entry.category.as_str(), "main" | "side" | "daily") {
            errors.push(format!("任务 {} 的 category 不受支持", entry.quest_key));
        }
        if let Some(map_key) = entry.map_key.as_deref() {
            validate_key(&mut errors, "quest.map_key", map_key);
        }
        range_field(
            &mut errors,
            "quest.level_required",
            &entry.quest_key,
            entry.level_required,
            1,
            120,
        );
        if entry.repeatable {
            errors.push(format!(
                "当前发布切片不允许可重复新任务：{}",
                entry.quest_key
            ));
        }
        if !entry.enabled {
            errors.push(format!("当前发布切片不允许禁用新任务：{}", entry.quest_key));
        }
        if entry.requirements.is_empty() {
            errors.push(format!("任务 {} 至少需要一条完成条件", entry.quest_key));
        }
        if entry.requirements.len() > 100 {
            errors.push(format!(
                "任务 {} 的完成条件不能超过 100 条",
                entry.quest_key
            ));
        }
        let mut requirement_sort_orders = BTreeSet::new();
        for requirement in &entry.requirements {
            if !requirement_sort_orders.insert(requirement.sort_order) {
                errors.push(format!(
                    "任务 {} 的完成条件排序重复：{}",
                    entry.quest_key, requirement.sort_order
                ));
            }
            if !matches!(
                requirement.requirement_kind.as_str(),
                "item" | "visit" | "level"
            ) {
                errors.push(format!(
                    "任务 {} 的 requirement_kind 不受支持：{}",
                    entry.quest_key, requirement.requirement_kind
                ));
            }
            match requirement.requirement_kind.as_str() {
                "item" | "visit" => validate_key(
                    &mut errors,
                    "quest.requirement.target_key",
                    &requirement.target_key,
                ),
                "level" if requirement.target_key != "level" => errors.push(format!(
                    "任务 {} 的等级条件 target_key 必须是 level",
                    entry.quest_key
                )),
                _ => {}
            }
            range_field(
                &mut errors,
                "quest.requirement.required_quantity",
                &entry.quest_key,
                requirement.required_quantity,
                1,
                9999,
            );
            if requirement.sort_order < 0 {
                errors.push(format!(
                    "任务 {} 的完成条件 sort_order 不能为负数",
                    entry.quest_key
                ));
            }
            text_field(
                &mut errors,
                "quest.requirement.description",
                &requirement.description,
                500,
                false,
            );
        }
        if entry.rewards.is_empty() {
            errors.push(format!("任务 {} 至少需要一条奖励", entry.quest_key));
        }
        if entry.rewards.len() > 100 {
            errors.push(format!("任务 {} 的奖励不能超过 100 条", entry.quest_key));
        }
        let mut reward_sort_orders = BTreeSet::new();
        for reward in &entry.rewards {
            if !reward_sort_orders.insert(reward.sort_order) {
                errors.push(format!(
                    "任务 {} 的奖励排序重复：{}",
                    entry.quest_key, reward.sort_order
                ));
            }
            if !matches!(reward.reward_kind.as_str(), "exp" | "currency" | "item") {
                errors.push(format!(
                    "任务 {} 的 reward_kind 不受支持：{}",
                    entry.quest_key, reward.reward_kind
                ));
            }
            match reward.reward_kind.as_str() {
                "exp" if reward.currency_code.is_some() || reward.item_key.is_some() => {
                    errors.push(format!(
                        "任务 {} 的经验奖励不能声明货币或物品",
                        entry.quest_key
                    ));
                }
                "currency"
                    if reward.currency_code.as_deref() != Some("gold_soul_coin")
                        || reward.item_key.is_some() =>
                {
                    errors.push(format!(
                        "任务 {} 的货币奖励只能使用 gold_soul_coin",
                        entry.quest_key
                    ));
                }
                "item" => {
                    if reward.currency_code.is_some() || reward.item_key.is_none() {
                        errors.push(format!(
                            "任务 {} 的物品奖励必须只声明 item_key",
                            entry.quest_key
                        ));
                    }
                    if let Some(item_key) = reward.item_key.as_deref() {
                        validate_key(&mut errors, "quest.reward.item_key", item_key);
                    }
                }
                _ => {}
            }
            range_field(
                &mut errors,
                "quest.reward.amount",
                &entry.quest_key,
                reward.amount,
                1,
                999_999_999,
            );
            if reward.sort_order < 0 {
                errors.push(format!(
                    "任务 {} 的奖励 sort_order 不能为负数",
                    entry.quest_key
                ));
            }
            text_field(
                &mut errors,
                "quest.reward.description",
                &reward.description,
                500,
                false,
            );
        }
    }

    keys.clear();
    let mut curve_names = BTreeSet::new();
    let mut curve_sort_orders = BTreeSet::new();
    for entry in &package.numeric_curves {
        if !keys.insert(format!("curve:{}", entry.curve_key)) {
            errors.push(format!("数值曲线键重复：{}", entry.curve_key));
        }
        if !curve_names.insert(entry.name.as_str()) {
            errors.push(format!("数值曲线名称重复：{}", entry.name));
        }
        if !curve_sort_orders.insert(entry.sort_order) {
            errors.push(format!("数值曲线排序重复：{}", entry.sort_order));
        }
        validate_key(&mut errors, "numeric_curve.curve_key", &entry.curve_key);
        text_field(&mut errors, "numeric_curve.name", &entry.name, 128, true);
        text_field(&mut errors, "numeric_curve.unit", &entry.unit, 32, true);
        validate_key(
            &mut errors,
            "numeric_curve.reference_key",
            &entry.reference_key,
        );
        if !matches!(
            entry.reference_key.as_str(),
            PLAYER_LEVEL_EXP_CURVE_REFERENCE
                | SKILL_PROFICIENCY_CURVE_REFERENCE
                | SKILL_DAMAGE_PERCENT_CURVE_REFERENCE
        ) {
            errors.push(format!(
                "数值曲线 {} 的 reference_key 不受支持：{}",
                entry.curve_key, entry.reference_key
            ));
        }
        if entry.range_min < 1 {
            errors.push(format!(
                "数值曲线 {} 的 range_min 必须大于或等于 1",
                entry.curve_key
            ));
        }
        if entry.range_max < entry.range_min {
            errors.push(format!(
                "数值曲线 {} 的 range_max 不能小于 range_min",
                entry.curve_key
            ));
        }
        if entry.sort_order < 0 {
            errors.push(format!(
                "数值曲线 {} 的 sort_order 不能为负数",
                entry.curve_key
            ));
        }
        text_field(
            &mut errors,
            "numeric_curve.description",
            &entry.description,
            2000,
            true,
        );
    }

    keys.clear();
    for entry in &package.wuhun {
        if !keys.insert(format!("wuhun:{}", entry.name)) {
            errors.push(format!("武魂名称重复：{}", entry.name));
        }
        text_field(&mut errors, "wuhun.name", &entry.name, 128, true);
        text_field(&mut errors, "wuhun.category", &entry.category, 32, true);
        if !matches!(
            entry.category.as_str(),
            "强攻系" | "控制系" | "敏攻系" | "辅助系" | "防御系" | "食物系"
        ) {
            errors.push(format!("武魂 {} 的 category 不受支持", entry.name));
        }
        text_field(&mut errors, "wuhun.form", &entry.form, 128, true);
        text_field(
            &mut errors,
            "wuhun.description",
            &entry.description,
            2000,
            true,
        );
        if entry.weight <= 0 {
            errors.push(format!("武魂 {} 的 weight 必须大于 0", entry.name));
        }
        validate_percent_fields(
            &mut errors,
            &format!("武魂 {} 的属性", entry.name),
            &entry.stats,
        );
    }

    keys.clear();
    for entry in &package.skills {
        if !keys.insert(format!("skill:{}", entry.skill_key)) {
            errors.push(format!("魂技键重复：{}", entry.skill_key));
        }
        validate_key(&mut errors, "skill.skill_key", &entry.skill_key);
        text_field(&mut errors, "skill.name", &entry.name, 128, true);
        if !matches!(
            entry.skill_type.as_str(),
            "active" | "passive" | "domain" | "ultimate"
        ) {
            errors.push(format!("魂技 {} 的 skill_type 不受支持", entry.skill_key));
        }
        if !matches!(
            entry.wuhun_category.as_str(),
            "all" | "强攻系" | "控制系" | "敏攻系" | "辅助系" | "防御系" | "食物系"
        ) {
            errors.push(format!(
                "魂技 {} 的 wuhun_category 不受支持",
                entry.skill_key
            ));
        }
        range_field(
            &mut errors,
            "skill.ring_index",
            &entry.skill_key,
            entry.ring_index,
            1,
            9,
        );
        range_field(
            &mut errors,
            "skill.soul_power_cost",
            &entry.skill_key,
            entry.soul_power_cost,
            1,
            1000,
        );
        range_field(
            &mut errors,
            "skill.cooldown_rounds",
            &entry.skill_key,
            entry.cooldown_rounds,
            0,
            100,
        );
        range_field(
            &mut errors,
            "skill.base_damage",
            &entry.skill_key,
            entry.base_damage,
            1,
            1_000_000,
        );
        range_field(
            &mut errors,
            "skill.spirit_ratio_percent",
            &entry.skill_key,
            entry.spirit_ratio_percent,
            0,
            1000,
        );
        range_field(
            &mut errors,
            "skill.strength_ratio_percent",
            &entry.skill_key,
            entry.strength_ratio_percent,
            0,
            1000,
        );
        text_field(
            &mut errors,
            "skill.description",
            &entry.description,
            2000,
            true,
        );
        if entry.starter && entry.ring_index != 1 {
            errors.push(format!(
                "skill {} starter requires ring_index = 1",
                entry.skill_key
            ));
        }
    }

    keys.clear();
    for entry in &package.effects {
        if !keys.insert(format!("effect:{}", entry.effect_key)) {
            errors.push(format!("效果键重复：{}", entry.effect_key));
        }
        validate_key(&mut errors, "effect.effect_key", &entry.effect_key);
        validate_key(&mut errors, "effect.skill_key", &entry.skill_key);
        if entry.trigger_kind != "on_release" {
            errors.push(format!("效果 {} 当前只支持 on_release", entry.effect_key));
        }
        if !matches!(entry.target_kind.as_str(), "self" | "enemy" | "beast") {
            errors.push(format!(
                "效果 {} 当前只支持 self、enemy 或 beast",
                entry.effect_key
            ));
        }
        let beast_attack_reduction = entry.operation == "modify_stat"
            && entry.attribute_key == "beast_attack"
            && entry.value_mode == "percent_delta";
        let poison_damage = entry.target_kind == "beast"
            && entry.operation == "deal_damage"
            && entry.attribute_key == "beast_hp"
            && entry.value_mode == "absolute";
        let stun_control = entry.target_kind == "beast"
            && entry.operation == "control"
            && entry.attribute_key == "stunned"
            && entry.value_mode == "absolute";
        let shield_protection = entry.target_kind == "self"
            && entry.operation == "absorb_damage"
            && entry.attribute_key == "beast_counterattack"
            && entry.value_mode == "absolute";
        let heal_restore = entry.target_kind == "self"
            && entry.operation == "restore"
            && entry.attribute_key == "player_hp"
            && entry.value_mode == "absolute";
        let target_selection = entry.target_kind == "beast"
            && entry.operation == "select_target"
            && entry.attribute_key == "battle_target"
            && entry.value_mode == "absolute";
        let forbid_skill = entry.target_kind == "self"
            && entry.operation == "control"
            && entry.attribute_key == "skill_usage"
            && entry.value_mode == "absolute";
        if !beast_attack_reduction
            && !poison_damage
            && !stun_control
            && !shield_protection
            && !heal_restore
            && !target_selection
            && !forbid_skill
        {
            errors.push(format!(
                "效果 {} 当前只支持减攻、poison-v1 中毒伤害、stun-v1 眩晕、shield-v1 护盾、heal-v1 治疗、target-v1 目标或 forbid-skill-v1 禁技节点",
                entry.effect_key
            ));
        }
        if beast_attack_reduction && !(-90..=-1).contains(&entry.value) {
            errors.push(format!(
                "效果 {} 的 value 必须是 -1 到 -90 的 percent_delta",
                entry.effect_key
            ));
        }
        if poison_damage && !(1..=MAX_POISON_TICK_DAMAGE).contains(&entry.value) {
            errors.push(format!(
                "效果 {} 的 poison-v1 value 必须是 1 到 {} 的 absolute",
                entry.effect_key, MAX_POISON_TICK_DAMAGE
            ));
        }
        if stun_control && entry.value != STUN_VALUE {
            errors.push(format!(
                "效果 {} 的 stun-v1 value 必须固定为 {} 的 absolute",
                entry.effect_key, STUN_VALUE
            ));
        }
        range_field(
            &mut errors,
            "effect.duration_rounds",
            &entry.effect_key,
            entry.duration_rounds,
            1,
            100,
        );
        if entry.chance_percent != 100 {
            errors.push(format!(
                "效果 {} 当前 chance_percent 必须为 100",
                entry.effect_key
            ));
        }
        if !matches!(
            entry.stack_policy.as_str(),
            "strongest" | "add" | "refresh" | "replace"
        ) {
            errors.push(format!(
                "效果 {} 的 stack_policy 不受支持",
                entry.effect_key
            ));
        }
        if beast_attack_reduction && !entry.parameters.is_empty() {
            errors.push(format!(
                "效果 {} 当前不允许非空 parameters",
                entry.effect_key
            ));
        }
        if poison_damage && !is_poison_v1_parameters(&entry.parameters) {
            errors.push(format!(
                "效果 {} 的 poison-v1 parameters 必须固定声明规则版本、结算时机和间隔",
                entry.effect_key
            ));
        }
        if stun_control && entry.stack_policy != "refresh" {
            errors.push(format!(
                "效果 {} 的 stun-v1 stack_policy 必须是 refresh",
                entry.effect_key
            ));
        }
        if stun_control && !(1..=MAX_STUN_DURATION_ROUNDS).contains(&entry.duration_rounds) {
            errors.push(format!(
                "效果 {} 的 stun-v1 duration_rounds 必须在 1 到 {} 之间",
                entry.effect_key, MAX_STUN_DURATION_ROUNDS
            ));
        }
        if stun_control && !is_stun_v1_parameters(&entry.parameters) {
            errors.push(format!(
                "效果 {} 的 stun-v1 parameters 必须固定声明规则版本、结算时机和反击行为",
                entry.effect_key
            ));
        }
        if shield_protection && entry.value != SHIELD_VALUE {
            errors.push(format!(
                "效果 {} 的 shield-v1 value 必须固定为 {} 的 absolute",
                entry.effect_key, SHIELD_VALUE
            ));
        }
        if shield_protection && entry.stack_policy != "refresh" {
            errors.push(format!(
                "效果 {} 的 shield-v1 stack_policy 必须是 refresh",
                entry.effect_key
            ));
        }
        if shield_protection && !(1..=MAX_SHIELD_DURATION_ROUNDS).contains(&entry.duration_rounds) {
            errors.push(format!(
                "效果 {} 的 shield-v1 duration_rounds 必须在 1 到 {} 之间",
                entry.effect_key, MAX_SHIELD_DURATION_ROUNDS
            ));
        }
        if shield_protection && !is_shield_v1_parameters(&entry.parameters) {
            errors.push(format!(
                "效果 {} 的 shield-v1 parameters 必须固定声明规则版本、结算时机和反击行为",
                entry.effect_key
            ));
        }
        if heal_restore && !(1..=MAX_HEAL_AMOUNT).contains(&entry.value) {
            errors.push(format!(
                "效果 {} 的 heal-v1 value 必须在 1 到 {} 之间",
                entry.effect_key, MAX_HEAL_AMOUNT
            ));
        }
        if heal_restore && entry.duration_rounds != 1 {
            errors.push(format!(
                "效果 {} 的 heal-v1 duration_rounds 必须固定为 1",
                entry.effect_key
            ));
        }
        if heal_restore && entry.stack_policy != "add" {
            errors.push(format!(
                "效果 {} 的 heal-v1 stack_policy 必须是 add",
                entry.effect_key
            ));
        }
        if heal_restore && !is_heal_v1_parameters(&entry.parameters) {
            errors.push(format!(
                "效果 {} 的 heal-v1 parameters 必须固定声明规则版本、结算时机和溢出行为",
                entry.effect_key
            ));
        }
        if forbid_skill && entry.value != FORBID_SKILL_VALUE {
            errors.push(format!(
                "效果 {} 的 forbid-skill-v1 value 必须固定为 {} 的 absolute",
                entry.effect_key, FORBID_SKILL_VALUE
            ));
        }
        if forbid_skill
            && !(MIN_FORBID_SKILL_DURATION_ROUNDS..=MAX_FORBID_SKILL_DURATION_ROUNDS)
                .contains(&entry.duration_rounds)
        {
            errors.push(format!(
                "效果 {} 的 forbid-skill-v1 duration_rounds 必须在 {} 到 {} 之间",
                entry.effect_key,
                MIN_FORBID_SKILL_DURATION_ROUNDS,
                MAX_FORBID_SKILL_DURATION_ROUNDS
            ));
        }
        if forbid_skill && entry.stack_policy != "refresh" {
            errors.push(format!(
                "效果 {} 的 forbid-skill-v1 stack_policy 必须是 refresh",
                entry.effect_key
            ));
        }
        if forbid_skill && !is_forbid_skill_v1_parameters(&entry.parameters) {
            errors.push(format!(
                "效果 {} 的 forbid-skill-v1 parameters 必须固定声明规则版本、结算时机和阻断动作",
                entry.effect_key
            ));
        }
        if target_selection {
            if entry.value != TARGET_SELECTION_VALUE {
                errors.push(format!(
                    "效果 {} 的 target-v1 value 必须固定为 {}",
                    entry.effect_key, TARGET_SELECTION_VALUE
                ));
            }
            if entry.duration_rounds != 1 {
                errors.push(format!(
                    "效果 {} 的 target-v1 duration_rounds 必须固定为 1",
                    entry.effect_key
                ));
            }
            if entry.stack_policy != "replace" {
                errors.push(format!(
                    "效果 {} 的 target-v1 stack_policy 必须是 replace",
                    entry.effect_key
                ));
            }
            if !is_target_v1_parameters(&entry.parameters) {
                errors.push(format!(
                    "效果 {} 的 target-v1 parameters 必须固定声明当前战斗魂兽选择器",
                    entry.effect_key
                ));
            }
        }
        text_field(
            &mut errors,
            "effect.description",
            &entry.description,
            2000,
            true,
        );
    }

    keys.clear();
    for entry in &package.soul_beasts {
        if !keys.insert(format!("beast:{}", entry.beast_key)) {
            errors.push(format!("魂兽键重复：{}", entry.beast_key));
        }
        validate_key(&mut errors, "soul_beast.beast_key", &entry.beast_key);
        text_field(&mut errors, "soul_beast.name", &entry.name, 128, true);
        text_field(
            &mut errors,
            "soul_beast.description",
            &entry.description,
            2000,
            true,
        );
        validate_key(&mut errors, "soul_beast.map_key", &entry.map_key);
        range_field(
            &mut errors,
            "soul_beast.age",
            &entry.beast_key,
            entry.age,
            1,
            999_999,
        );
        range_field(
            &mut errors,
            "soul_beast.level_required",
            &entry.beast_key,
            entry.level_required,
            1,
            120,
        );
        range_field(
            &mut errors,
            "soul_beast.max_hp",
            &entry.beast_key,
            entry.max_hp,
            1,
            1_000_000_000,
        );
        range_field(
            &mut errors,
            "soul_beast.attack",
            &entry.beast_key,
            entry.attack,
            1,
            1_000_000_000,
        );
        range_field(
            &mut errors,
            "soul_beast.defense",
            &entry.beast_key,
            entry.defense,
            0,
            1_000_000_000,
        );
        range_field(
            &mut errors,
            "soul_beast.speed",
            &entry.beast_key,
            entry.speed,
            0,
            1_000_000_000,
        );
        range_field(
            &mut errors,
            "soul_beast.exp_reward",
            &entry.beast_key,
            entry.exp_reward,
            1,
            999_999_999,
        );
        validate_key(
            &mut errors,
            "soul_beast.drop_item_key",
            &entry.drop_item_key,
        );
        range_field(
            &mut errors,
            "soul_beast.drop_quantity",
            &entry.beast_key,
            entry.drop_quantity,
            1,
            99,
        );
    }

    keys.clear();
    for entry in &package.soul_rings {
        if !keys.insert(format!("ring:{}", entry.ring_key)) {
            errors.push(format!("魂环键重复：{}", entry.ring_key));
        }
        validate_key(&mut errors, "soul_ring.ring_key", &entry.ring_key);
        text_field(&mut errors, "soul_ring.name", &entry.name, 128, true);
        validate_key(
            &mut errors,
            "soul_ring.soul_beast_key",
            &entry.soul_beast_key,
        );
        validate_key(&mut errors, "soul_ring.skill_key", &entry.skill_key);
        range_field(
            &mut errors,
            "soul_ring.ring_index",
            &entry.ring_key,
            entry.ring_index,
            1,
            9,
        );
        range_field(
            &mut errors,
            "soul_ring.age",
            &entry.ring_key,
            entry.age,
            1,
            1_000_000_000,
        );
        if !matches!(
            entry.color.as_str(),
            "white" | "yellow" | "purple" | "black" | "red"
        ) {
            errors.push(format!("魂环 {} 的 color 不受支持", entry.ring_key));
        }
        text_field(
            &mut errors,
            "soul_ring.description",
            &entry.description,
            2000,
            true,
        );
    }

    let mut transition_sources = BTreeSet::new();
    let mut transition_targets = BTreeSet::new();
    for entry in &package.transitions {
        if !matches!(
            entry.entity_kind.as_str(),
            "wuhun" | "skill" | "effect" | "beast" | "ring"
        ) {
            errors.push(format!(
                "transition {} 的 entity_kind 不受支持",
                entry.source_key
            ));
        }
        if !matches!(entry.transition_kind.as_str(), "deprecated" | "replaced") {
            errors.push(format!(
                "transition {} 的 transition_kind 不受支持",
                entry.source_key
            ));
        }
        validate_transition_key(
            &mut errors,
            "transition.source_key",
            &entry.entity_kind,
            &entry.source_key,
        );
        if let Some(target_key) = &entry.target_key {
            validate_transition_key(
                &mut errors,
                "transition.target_key",
                &entry.entity_kind,
                target_key,
            );
        }
        text_field(&mut errors, "transition.reason", &entry.reason, 512, true);
        if !transition_sources.insert(format!("{}:{}", entry.entity_kind, entry.source_key)) {
            errors.push(format!(
                "transition source 重复：{} / {}",
                entry.entity_kind, entry.source_key
            ));
        }
        if let Some(target_key) = &entry.target_key {
            if entry.source_key == *target_key {
                errors.push(format!(
                    "transition source 与 target 不能相同：{}",
                    entry.source_key
                ));
            }
            if !transition_targets.insert(format!("{}:{}", entry.entity_kind, target_key)) {
                errors.push(format!(
                    "transition target 重复：{} / {}",
                    entry.entity_kind, target_key
                ));
            }
        }
        match (entry.transition_kind.as_str(), entry.target_key.as_ref()) {
            ("deprecated", Some(_)) => errors.push(format!(
                "deprecated transition 不允许 target：{}",
                entry.source_key
            )),
            ("replaced", None) => errors.push(format!(
                "replaced transition 必须提供 target：{}",
                entry.source_key
            )),
            _ => {}
        }
    }
    errors
}

fn validate_transition_key(errors: &mut Vec<String>, field: &str, entity_kind: &str, value: &str) {
    if entity_kind == "wuhun" {
        text_field(errors, field, value, 200, true);
    } else {
        validate_key(errors, field, value);
    }
}

fn validate_percent_fields(errors: &mut Vec<String>, prefix: &str, stats: &WuhunStatsPackageEntry) {
    for (name, value) in [
        ("attack_percent", stats.attack_percent),
        ("defense_percent", stats.defense_percent),
        ("strength_percent", stats.strength_percent),
        ("agility_percent", stats.agility_percent),
        ("spirit_percent", stats.spirit_percent),
        ("endurance_percent", stats.endurance_percent),
        ("perception_percent", stats.perception_percent),
        ("luck_percent", stats.luck_percent),
    ] {
        if !(1..=500).contains(&value) {
            errors.push(format!("{prefix}.{name} 必须在 1 到 500 之间"));
        }
    }
}

fn text_field(
    errors: &mut Vec<String>,
    field: &str,
    value: &str,
    max_chars: usize,
    required: bool,
) {
    let length = value.chars().count();
    if (required && value.trim().is_empty())
        || length > max_chars
        || value.chars().any(char::is_control)
    {
        errors.push(format!("{field} 为空、过长或包含控制字符"));
    }
}

fn validate_key(errors: &mut Vec<String>, field: &str, value: &str) {
    if !is_content_key(value) {
        errors.push(format!("{field} 不是合法小写内容键"));
    }
}

fn range_field(errors: &mut Vec<String>, field: &str, key: &str, value: i64, min: i64, max: i64) {
    if !(min..=max).contains(&value) {
        errors.push(format!("{field}({key}) 必须在 {min} 到 {max} 之间"));
    }
}

/// 判断 HTTP 路由与内容解析共用的稳定内容键格式。
pub(crate) fn is_content_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value == value.trim()
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_package() -> ContentPackage {
        ContentPackage {
            package_key: "example-content".to_string(),
            revision: 1,
            author: "test".to_string(),
            minimum_runtime: String::new(),
            maps: Vec::new(),
            items: Vec::new(),
            npcs: Vec::new(),
            quests: Vec::new(),
            numeric_curves: Vec::new(),
            wuhun: Vec::new(),
            skills: vec![SkillPackageEntry {
                skill_key: "test-skill".to_string(),
                name: "测试魂技".to_string(),
                skill_type: "active".to_string(),
                wuhun_category: "all".to_string(),
                ring_index: 1,
                soul_power_cost: 10,
                cooldown_rounds: 1,
                base_damage: 10,
                spirit_ratio_percent: 100,
                strength_ratio_percent: 0,
                description: "测试技能".to_string(),
                enabled: true,
                starter: false,
            }],
            effects: vec![EffectPackageEntry {
                effect_key: "test-effect".to_string(),
                skill_key: "test-skill".to_string(),
                trigger_kind: "on_release".to_string(),
                target_kind: "beast".to_string(),
                operation: "modify_stat".to_string(),
                attribute_key: "beast_attack".to_string(),
                value_mode: "percent_delta".to_string(),
                value: -20,
                duration_rounds: 2,
                chance_percent: 100,
                stack_policy: "strongest".to_string(),
                parameters: BTreeMap::new(),
                description: "测试效果".to_string(),
                enabled: true,
            }],
            soul_beasts: Vec::new(),
            soul_rings: Vec::new(),
            transitions: Vec::new(),
        }
    }

    fn poison_v1_parameters() -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "rule_version".to_string(),
                Value::String(POISON_RULE_VERSION.to_string()),
            ),
            (
                "tick_phase".to_string(),
                Value::String(POISON_TICK_PHASE.to_string()),
            ),
            (
                "tick_interval".to_string(),
                Value::from(POISON_TICK_INTERVAL),
            ),
        ])
    }

    #[test]
    fn parses_json_and_toml_to_the_same_hash() {
        let package = minimal_package();
        let json = serde_json::to_string(&package).expect("JSON 应可序列化");
        let toml = toml::to_string(&package).expect("TOML 应可序列化");
        let json_loaded = parse_package_text(&json, "json").expect("JSON 应可解析");
        let toml_loaded = parse_package_text(&toml, "toml").expect("TOML 应可解析");
        assert_eq!(json_loaded.package, toml_loaded.package);
        assert_eq!(json_loaded.content_hash, toml_loaded.content_hash);
    }

    #[test]
    fn maps_preserve_legacy_hash_and_enforce_static_shape() {
        let mut package = minimal_package();
        let legacy = canonical_json(&package).expect("旧内容包应可规范化");
        assert!(!legacy.contains("\"maps\""));
        assert!(!legacy.contains("\"items\""));
        assert!(!legacy.contains("\"npcs\""));
        assert!(!legacy.contains("\"quests\""));
        assert!(!legacy.contains("\"numeric_curves\""));

        package.maps = vec![MapPackageEntry {
            map_key: "content-map".to_string(),
            name: "内容测试地图".to_string(),
            description: "仅用于校验静态地图目录。".to_string(),
            level_required: 1,
            safe: true,
            pvp_enabled: false,
            teleport_enabled: true,
            sort_order: 80,
        }];
        assert!(validate_shape(&package).is_empty());

        package.maps.push(MapPackageEntry {
            map_key: "content-map-second".to_string(),
            name: "内容测试地图".to_string(),
            description: "重复名称和排序必须拒绝。".to_string(),
            level_required: 1,
            safe: false,
            pvp_enabled: true,
            teleport_enabled: false,
            sort_order: 80,
        });
        let errors = validate_shape(&package);
        assert!(errors.iter().any(|error| error.contains("地图名称重复")));
        assert!(errors.iter().any(|error| error.contains("地图排序重复")));
    }

    #[test]
    fn items_preserve_legacy_hash_and_enforce_static_shape() {
        let mut package = minimal_package();
        package.items = vec![ItemPackageEntry {
            item_key: "content-potion".to_string(),
            name: "内容测试药".to_string(),
            category: "consumable".to_string(),
            quality: 2,
            stackable: true,
            max_stack: 99,
            buy_price: 50,
            sell_price: 10,
            level_required: 1,
            effect_kind: "restore_hp".to_string(),
            effect_amount: 100,
            revive_hp_percent: 0,
            purchasable: true,
            sellable: true,
            usable: true,
            description: "恢复100点生命值".to_string(),
        }];
        assert!(validate_shape(&package).is_empty());

        package.items.push(ItemPackageEntry {
            item_key: "content-potion".to_string(),
            name: "内容测试药".to_string(),
            category: "consumable".to_string(),
            quality: 2,
            stackable: true,
            max_stack: 99,
            buy_price: 50,
            sell_price: 10,
            level_required: 1,
            effect_kind: "restore_hp".to_string(),
            effect_amount: 100,
            revive_hp_percent: 0,
            purchasable: true,
            sellable: true,
            usable: true,
            description: "重复名称".to_string(),
        });
        let errors = validate_shape(&package);
        assert!(errors.iter().any(|error| error.contains("物品键重复")));
        assert!(errors.iter().any(|error| error.contains("物品名称重复")));

        package.items[0].quality = 6;
        package.items[1].name = "另一种测试药".to_string();
        let errors = validate_shape(&package);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("item.quality(content-potion)"))
        );
    }

    #[test]
    fn npcs_preserve_legacy_hash_and_enforce_static_shape() {
        let mut package = minimal_package();
        let legacy = canonical_json(&package).expect("旧内容包应可规范化");
        assert!(!legacy.contains("\"npcs\""));

        package.npcs = vec![NpcPackageEntry {
            npc_key: "content-merchant".to_string(),
            map_key: "content-map".to_string(),
            name: "内容商人".to_string(),
            npc_kind: "merchant".to_string(),
            dialogue: "这里仅校验静态 NPC 目录。".to_string(),
            description: "不承载商店商品或玩家对话绑定。".to_string(),
            enabled: true,
            sort_order: 10,
        }];
        assert!(validate_shape(&package).is_empty());

        package.npcs.push(NpcPackageEntry {
            npc_key: "content-merchant".to_string(),
            map_key: "content-map".to_string(),
            name: "内容商人".to_string(),
            npc_kind: "elder".to_string(),
            dialogue: String::new(),
            description: String::new(),
            enabled: true,
            sort_order: 20,
        });
        let errors = validate_shape(&package);
        assert!(errors.iter().any(|error| error.contains("NPC 键重复")));
        assert!(errors.iter().any(|error| error.contains("NPC 名称重复")));

        package.npcs[0].enabled = false;
        package.npcs[1].npc_key = "content-elder".to_string();
        package.npcs[1].name = "另一位内容 NPC".to_string();
        package.npcs[1].npc_kind = "unknown".to_string();
        package.npcs[1].sort_order = -1;
        let errors = validate_shape(&package);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("不允许禁用新 NPC"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("npc_kind 不受支持"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("sort_order 不能为负数"))
        );
    }

    #[test]
    fn quests_preserve_legacy_hash_and_enforce_static_shape() {
        let mut package = minimal_package();
        let legacy = canonical_json(&package).expect("旧内容包应可规范化");
        assert!(!legacy.contains("\"quests\""));

        package.quests = vec![QuestPackageEntry {
            quest_key: "content-quest".to_string(),
            name: "内容任务".to_string(),
            description: "仅用于校验静态任务目录。".to_string(),
            category: "side".to_string(),
            map_key: Some("content-map".to_string()),
            level_required: 1,
            repeatable: false,
            enabled: true,
            requirements: vec![QuestRequirementPackageEntry {
                requirement_kind: "item".to_string(),
                target_key: "content-potion".to_string(),
                required_quantity: 2,
                sort_order: 0,
                description: "拥有两瓶内容测试药".to_string(),
            }],
            rewards: vec![
                QuestRewardPackageEntry {
                    reward_kind: "exp".to_string(),
                    currency_code: None,
                    item_key: None,
                    amount: 80,
                    sort_order: 0,
                    description: "经验奖励".to_string(),
                },
                QuestRewardPackageEntry {
                    reward_kind: "currency".to_string(),
                    currency_code: Some("gold_soul_coin".to_string()),
                    item_key: None,
                    amount: 30,
                    sort_order: 1,
                    description: "金魂币奖励".to_string(),
                },
            ],
        }];
        assert!(validate_shape(&package).is_empty());

        package.quests.push(QuestPackageEntry {
            quest_key: "content-quest".to_string(),
            name: "内容任务".to_string(),
            description: "重复任务必须拒绝。".to_string(),
            category: "unknown".to_string(),
            map_key: None,
            level_required: 1,
            repeatable: true,
            enabled: false,
            requirements: Vec::new(),
            rewards: Vec::new(),
        });
        let errors = validate_shape(&package);
        assert!(errors.iter().any(|error| error.contains("任务键重复")));
        assert!(errors.iter().any(|error| error.contains("任务名称重复")));
        assert!(
            errors
                .iter()
                .any(|error| error.contains("category 不受支持"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("不允许可重复新任务"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("不允许禁用新任务"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("至少需要一条完成条件"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("至少需要一条奖励"))
        );

        package.quests.truncate(1);
        package.quests[0].requirements[0].requirement_kind = "unknown".to_string();
        package.quests[0].rewards[1].currency_code = Some("unknown".to_string());
        let errors = validate_shape(&package);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("requirement_kind 不受支持"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("货币奖励只能使用 gold_soul_coin"))
        );
    }

    #[test]
    fn numeric_curves_preserve_legacy_hash_and_enforce_static_shape() {
        let mut package = minimal_package();
        let legacy = canonical_json(&package).expect("旧内容包应可规范化");
        assert!(!legacy.contains("\"numeric_curves\""));

        package.numeric_curves = vec![NumericCurvePackageEntry {
            curve_key: "content-player-level-exp".to_string(),
            name: "内容角色等级经验".to_string(),
            unit: "级".to_string(),
            range_min: 1,
            range_max: 120,
            reference_key: PLAYER_LEVEL_EXP_CURVE_REFERENCE.to_string(),
            description: "只展示既有角色等级经验规则的输入范围。".to_string(),
            sort_order: 0,
        }];
        assert!(validate_shape(&package).is_empty());

        package.numeric_curves.push(NumericCurvePackageEntry {
            curve_key: "content-player-level-exp".to_string(),
            name: "内容角色等级经验".to_string(),
            unit: String::new(),
            range_min: 10,
            range_max: 1,
            reference_key: "unknown-curve-v1".to_string(),
            description: String::new(),
            sort_order: 0,
        });
        let errors = validate_shape(&package);
        assert!(errors.iter().any(|error| error.contains("数值曲线键重复")));
        assert!(
            errors
                .iter()
                .any(|error| error.contains("数值曲线名称重复"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("数值曲线排序重复"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("reference_key 不受支持"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("range_max 不能小于 range_min"))
        );
    }

    #[test]
    fn accepts_only_the_controlled_poison_v1_node() {
        let mut package = minimal_package();
        let effect = &mut package.effects[0];
        effect.target_kind = "beast".to_string();
        effect.operation = "deal_damage".to_string();
        effect.attribute_key = "beast_hp".to_string();
        effect.value_mode = "absolute".to_string();
        effect.value = 5;
        effect.duration_rounds = 3;
        effect.stack_policy = "refresh".to_string();
        effect.parameters = poison_v1_parameters();
        assert!(validate_shape(&package).is_empty());

        package.effects[0]
            .parameters
            .insert("tick_interval".to_string(), Value::from(2));
        assert!(
            validate_shape(&package)
                .iter()
                .any(|error| error.contains("poison-v1 parameters"))
        );
    }

    #[test]
    fn accepts_only_the_controlled_stun_v1_node() {
        let mut package = minimal_package();
        let effect = &mut package.effects[0];
        effect.target_kind = "beast".to_string();
        effect.operation = "control".to_string();
        effect.attribute_key = "stunned".to_string();
        effect.value_mode = "absolute".to_string();
        effect.value = 1;
        effect.duration_rounds = 2;
        effect.stack_policy = "refresh".to_string();
        effect.parameters = BTreeMap::from([
            (
                "rule_version".to_string(),
                Value::String("stun-v1".to_string()),
            ),
            (
                "apply_phase".to_string(),
                Value::String("after_player_damage".to_string()),
            ),
            (
                "counterattack".to_string(),
                Value::String("skip".to_string()),
            ),
        ]);
        assert!(validate_shape(&package).is_empty());

        package.effects[0].parameters.insert(
            "counterattack".to_string(),
            Value::String("block".to_string()),
        );
        assert!(
            validate_shape(&package)
                .iter()
                .any(|error| error.contains("stun-v1 parameters"))
        );
    }

    #[test]
    fn accepts_only_the_controlled_shield_v1_node() {
        let mut package = minimal_package();
        let effect = &mut package.effects[0];
        effect.target_kind = "self".to_string();
        effect.operation = "absorb_damage".to_string();
        effect.attribute_key = "beast_counterattack".to_string();
        effect.value_mode = "absolute".to_string();
        effect.value = 1;
        effect.duration_rounds = 2;
        effect.stack_policy = "refresh".to_string();
        effect.parameters = BTreeMap::from([
            (
                "rule_version".to_string(),
                Value::String("shield-v1".to_string()),
            ),
            (
                "apply_phase".to_string(),
                Value::String("after_player_damage".to_string()),
            ),
            (
                "counterattack".to_string(),
                Value::String("absorb".to_string()),
            ),
        ]);
        assert!(validate_shape(&package).is_empty());

        package.effects[0].parameters.insert(
            "counterattack".to_string(),
            Value::String("skip".to_string()),
        );
        assert!(
            validate_shape(&package)
                .iter()
                .any(|error| error.contains("shield-v1 parameters"))
        );
    }

    #[test]
    fn accepts_only_the_controlled_heal_v1_node() {
        let mut package = minimal_package();
        let effect = &mut package.effects[0];
        effect.target_kind = "self".to_string();
        effect.operation = "restore".to_string();
        effect.attribute_key = "player_hp".to_string();
        effect.value_mode = "absolute".to_string();
        effect.value = 30;
        effect.duration_rounds = 1;
        effect.stack_policy = "add".to_string();
        effect.parameters = BTreeMap::from([
            (
                "rule_version".to_string(),
                Value::String("heal-v1".to_string()),
            ),
            (
                "apply_phase".to_string(),
                Value::String("after_player_damage".to_string()),
            ),
            (
                "overflow".to_string(),
                Value::String("cap_at_max_hp".to_string()),
            ),
        ]);
        assert!(validate_shape(&package).is_empty());

        package.effects[0].parameters.insert(
            "overflow".to_string(),
            Value::String("overflow".to_string()),
        );
        assert!(
            validate_shape(&package)
                .iter()
                .any(|error| error.contains("heal-v1 parameters"))
        );
    }

    #[test]
    fn accepts_only_the_current_battle_target_v1_node() {
        let mut package = minimal_package();
        let effect = &mut package.effects[0];
        effect.target_kind = "beast".to_string();
        effect.operation = "select_target".to_string();
        effect.attribute_key = "battle_target".to_string();
        effect.value_mode = "absolute".to_string();
        effect.value = 1;
        effect.duration_rounds = 1;
        effect.stack_policy = "replace".to_string();
        effect.parameters = BTreeMap::from([
            (
                "rule_version".to_string(),
                Value::String("target-v1".to_string()),
            ),
            (
                "selector".to_string(),
                Value::String("current_battle_beast".to_string()),
            ),
        ]);
        assert!(validate_shape(&package).is_empty());

        package.effects[0].parameters.insert(
            "selector".to_string(),
            Value::String("random_beast".to_string()),
        );
        assert!(
            validate_shape(&package)
                .iter()
                .any(|error| error.contains("target-v1 parameters"))
        );
    }

    #[test]
    fn accepts_only_the_controlled_forbid_skill_v1_node() {
        let mut package = minimal_package();
        let effect = &mut package.effects[0];
        effect.target_kind = "self".to_string();
        effect.operation = "control".to_string();
        effect.attribute_key = "skill_usage".to_string();
        effect.value_mode = "absolute".to_string();
        effect.value = FORBID_SKILL_VALUE;
        effect.duration_rounds = MIN_FORBID_SKILL_DURATION_ROUNDS;
        effect.stack_policy = "refresh".to_string();
        effect.parameters = BTreeMap::from([
            (
                "rule_version".to_string(),
                Value::String(FORBID_SKILL_RULE_VERSION.to_string()),
            ),
            (
                "apply_phase".to_string(),
                Value::String(FORBID_SKILL_APPLY_PHASE.to_string()),
            ),
            (
                "blocked_action".to_string(),
                Value::String(FORBID_SKILL_BLOCKED_ACTION.to_string()),
            ),
        ]);
        assert!(validate_shape(&package).is_empty());

        package.effects[0].parameters.insert(
            "blocked_action".to_string(),
            Value::String("attack".to_string()),
        );
        assert!(
            validate_shape(&package)
                .iter()
                .any(|error| error.contains("forbid-skill-v1 parameters"))
        );
    }

    #[test]
    fn rejects_unsupported_effect_parameters_and_duplicate_keys() {
        let mut package = minimal_package();
        package.effects[0]
            .parameters
            .insert("script".to_string(), Value::String("x".to_string()));
        assert!(
            validate_shape(&package)
                .iter()
                .any(|error| error.contains("parameters"))
        );
        package.effects.push(package.effects[0].clone());
        assert!(
            validate_shape(&package)
                .iter()
                .any(|error| error.contains("效果键重复"))
        );
    }

    #[test]
    fn transition_declarations_preserve_legacy_hash_and_enforce_shape() {
        let package = minimal_package();
        let canonical = canonical_json(&package).expect("空 transition 包应可规范化");
        assert!(!canonical.contains("\"transitions\""));
        let legacy = parse_package_text(&canonical, "json").expect("旧格式内容包应继续可解析");
        assert_eq!(legacy.content_hash, content_hash(&package).unwrap());

        let mut invalid = package;
        invalid.transitions = vec![
            ContentTransitionPackageEntry {
                entity_kind: "skill".to_string(),
                source_key: "test-skill".to_string(),
                target_key: None,
                transition_kind: "replaced".to_string(),
                reason: "缺少 target".to_string(),
            },
            ContentTransitionPackageEntry {
                entity_kind: "skill".to_string(),
                source_key: "test-skill".to_string(),
                target_key: Some("test-skill".to_string()),
                transition_kind: "deprecated".to_string(),
                reason: "错误的弃用目标".to_string(),
            },
        ];
        let errors = validate_shape(&invalid);
        assert!(errors.iter().any(|error| error.contains("必须提供 target")));
        assert!(errors.iter().any(|error| error.contains("不允许 target")));
        assert!(
            errors
                .iter()
                .any(|error| error.contains("source 与 target 不能相同"))
        );
        assert!(errors.iter().any(|error| error.contains("source 重复")));
    }

    #[test]
    fn package_file_rejects_non_regular_or_oversized_input() {
        let directory = tempfile::tempdir().expect("应创建内容包临时目录");
        let non_regular = directory.path().join("content-dir");
        fs::create_dir_all(&non_regular).expect("应创建目录输入");
        let error =
            load_package_file(directory.path(), "content-dir").expect_err("内容包不能是目录");
        assert!(error.contains("常规文件"));

        let oversized = directory.path().join("large.json");
        fs::write(&oversized, vec![b' '; MAX_PACKAGE_BYTES + 1]).expect("应写入超大内容包");
        let error =
            load_package_file(directory.path(), "large.json").expect_err("超大内容包必须被拒绝");
        assert!(error.contains("超过"));
    }
}
