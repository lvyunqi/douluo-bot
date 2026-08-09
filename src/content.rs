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

/// 可发布目录数据的文件格式。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContentPackage {
    pub package_key: String,
    pub revision: i64,
    pub author: String,
    #[serde(default)]
    pub minimum_runtime: String,
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
    let total = package.wuhun.len()
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
        if !beast_attack_reduction
            && !poison_damage
            && !stun_control
            && !shield_protection
            && !heal_restore
            && !target_selection
        {
            errors.push(format!(
                "效果 {} 当前只支持减攻、poison-v1 中毒伤害、stun-v1 眩晕、shield-v1 护盾、heal-v1 治疗或 target-v1 目标节点",
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
                    "效果 {} 的 target-v1 parameters 必须固定声明选择器、快照来源和缺失行为",
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
