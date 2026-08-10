//! 公共目录种子的只读 SQLite 导出与规范内容包写入。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::content::{
    ContentPackage, ItemPackageEntry, LoadedContentPackage, MapPackageEntry, NpcPackageEntry,
    NumericCurvePackageEntry, QuestPackageEntry, QuestRequirementPackageEntry,
    QuestRewardPackageEntry, canonical_json, parse_package_text,
};

const MAX_PUBLIC_SEED_ROWS: usize = 10_000;
const REQUIRED_PUBLIC_SEED_TABLES: [&str; 7] = [
    "map",
    "item",
    "npc",
    "quest",
    "quest_requirement",
    "quest_reward",
    "numeric_curve",
];

/// 公共种子内容包必须由调用者显式声明的发布元数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicSeedPackageMetadata {
    pub package_key: String,
    pub revision: i64,
    pub author: String,
    pub minimum_runtime: String,
}

struct SourceQuest {
    id: i64,
    entry: QuestPackageEntry,
}

/// 限制公共目录总行数，避免异常源库放大为无界内存占用。
struct PublicSeedReadBudget {
    used_rows: usize,
}

impl PublicSeedReadBudget {
    fn consume(&mut self, table: &str) -> Result<(), String> {
        if self.used_rows >= MAX_PUBLIC_SEED_ROWS {
            return Err(format!(
                "公共种子目录总行数不能超过 {MAX_PUBLIC_SEED_ROWS}，读取 {table} 时超出上限"
            ));
        }
        self.used_rows += 1;
        Ok(())
    }
}

/// 从 SQLite 源库只读提取公共目录，并归一化为可进入既有 revision 流程的 JSON 内容包。
pub fn import_public_seed_sqlite(
    source_path: &Path,
    metadata: &PublicSeedPackageMetadata,
) -> Result<LoadedContentPackage, String> {
    let connection = open_public_seed_source(source_path)?;
    require_public_seed_tables(&connection)?;

    let mut budget = PublicSeedReadBudget { used_rows: 0 };
    let package = ContentPackage {
        package_key: metadata.package_key.clone(),
        revision: metadata.revision,
        author: metadata.author.clone(),
        minimum_runtime: metadata.minimum_runtime.clone(),
        maps: read_maps(&connection, &mut budget)?,
        items: read_items(&connection, &mut budget)?,
        npcs: read_npcs(&connection, &mut budget)?,
        quests: read_quests(&connection, &mut budget)?,
        numeric_curves: read_numeric_curves(&connection, &mut budget)?,
        states: Vec::new(),
        wuhun: Vec::new(),
        skills: Vec::new(),
        effects: Vec::new(),
        soul_beasts: Vec::new(),
        soul_beast_skill_pools: Vec::new(),
        soul_rings: Vec::new(),
        transitions: Vec::new(),
    };
    let json = canonical_json(&package)?;
    parse_package_text(&json, "json")
}

/// 以新建文件语义写出导入后的规范 JSON，避免覆盖既有内容包。
pub fn write_public_seed_package_json(
    output_path: &Path,
    package: &LoadedContentPackage,
) -> Result<(), String> {
    if !output_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
    {
        return Err("公共种子输出文件必须使用 .json 扩展名".to_string());
    }
    let json = canonical_json(&package.package)?;
    let normalized = parse_package_text(&json, "json")?;
    if normalized.content_hash != package.content_hash {
        return Err("公共种子内容包哈希与规范 JSON 不一致".to_string());
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_path)
        .map_err(|error| format!("新建公共种子输出文件失败：{error}"))?;
    output
        .write_all(json.as_bytes())
        .map_err(|error| format!("写入公共种子输出文件失败：{error}"))?;
    output
        .flush()
        .map_err(|error| format!("刷新公共种子输出文件失败：{error}"))
}

/// 以只读模式打开常规 SQLite 文件，拒绝符号链接与非文件输入。
fn open_public_seed_source(source_path: &Path) -> Result<Connection, String> {
    let metadata = fs::symlink_metadata(source_path)
        .map_err(|error| format!("读取公共种子源文件失败：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("公共种子源文件不能是符号链接".to_string());
    }
    if !metadata.is_file() {
        return Err("公共种子源文件必须是常规 SQLite 文件".to_string());
    }
    let connection = Connection::open_with_flags(
        source_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("以只读模式打开公共种子源库失败：{error}"))?;
    connection
        .execute_batch("PRAGMA query_only = ON;")
        .map_err(|error| format!("限制公共种子源库为只读失败：{error}"))?;
    Ok(connection)
}

/// 验证源库只包含本切片支持的完整公共目录表，而不是把缺失数据当作空目录。
fn require_public_seed_tables(connection: &Connection) -> Result<(), String> {
    for table in REQUIRED_PUBLIC_SEED_TABLES {
        let exists = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("检查公共种子表 {table} 失败：{error}"))?;
        if !exists {
            return Err(format!("公共种子源库缺少必需表：{table}"));
        }
    }
    Ok(())
}

/// 按稳定排序读取地图目录，不导入地图出口或玩家位置。
fn read_maps(
    connection: &Connection,
    budget: &mut PublicSeedReadBudget,
) -> Result<Vec<MapPackageEntry>, String> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT map_key, name, description, level_required, safe, pvp_enabled,
                   teleport_enabled, sort_order
              FROM map
             ORDER BY sort_order ASC, map_key ASC
            "#,
        )
        .map_err(|error| format!("准备读取地图公共种子失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(MapPackageEntry {
                map_key: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                level_required: row.get(3)?,
                safe: row.get(4)?,
                pvp_enabled: row.get(5)?,
                teleport_enabled: row.get(6)?,
                sort_order: row.get(7)?,
            })
        })
        .map_err(|error| format!("查询地图公共种子失败：{error}"))?;
    let mut entries = Vec::new();
    for row in rows {
        budget.consume("map")?;
        entries.push(row.map_err(|error| format!("解析地图公共种子失败：{error}"))?);
    }
    Ok(entries)
}

/// 按稳定键读取物品目录，不导入背包、商店商品或库存。
fn read_items(
    connection: &Connection,
    budget: &mut PublicSeedReadBudget,
) -> Result<Vec<ItemPackageEntry>, String> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT item_key, name, category, quality, stackable, max_stack,
                   buy_price, sell_price, level_required, effect_kind, effect_amount,
                   revive_hp_percent, purchasable, sellable, usable, description
              FROM item
             ORDER BY item_key ASC
            "#,
        )
        .map_err(|error| format!("准备读取物品公共种子失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(ItemPackageEntry {
                item_key: row.get(0)?,
                name: row.get(1)?,
                category: row.get(2)?,
                quality: row.get(3)?,
                stackable: row.get(4)?,
                max_stack: row.get(5)?,
                buy_price: row.get(6)?,
                sell_price: row.get(7)?,
                level_required: row.get(8)?,
                effect_kind: row.get(9)?,
                effect_amount: row.get(10)?,
                revive_hp_percent: row.get(11)?,
                purchasable: row.get(12)?,
                sellable: row.get(13)?,
                usable: row.get(14)?,
                description: row.get(15)?,
            })
        })
        .map_err(|error| format!("查询物品公共种子失败：{error}"))?;
    let mut entries = Vec::new();
    for row in rows {
        budget.consume("item")?;
        entries.push(row.map_err(|error| format!("解析物品公共种子失败：{error}"))?);
    }
    Ok(entries)
}

/// 按地图与排序读取 NPC 目录，不导入 NPC 会话或商店数据。
fn read_npcs(
    connection: &Connection,
    budget: &mut PublicSeedReadBudget,
) -> Result<Vec<NpcPackageEntry>, String> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT npc_key, map_key, name, npc_kind, dialogue, description, enabled, sort_order
              FROM npc
             ORDER BY map_key ASC, sort_order ASC, npc_key ASC
            "#,
        )
        .map_err(|error| format!("准备读取 NPC 公共种子失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(NpcPackageEntry {
                npc_key: row.get(0)?,
                map_key: row.get(1)?,
                name: row.get(2)?,
                npc_kind: row.get(3)?,
                dialogue: row.get(4)?,
                description: row.get(5)?,
                enabled: row.get(6)?,
                sort_order: row.get(7)?,
            })
        })
        .map_err(|error| format!("查询 NPC 公共种子失败：{error}"))?;
    let mut entries = Vec::new();
    for row in rows {
        budget.consume("npc")?;
        entries.push(row.map_err(|error| format!("解析 NPC 公共种子失败：{error}"))?);
    }
    Ok(entries)
}

/// 读取任务及其不可变条件和奖励目录，不读取玩家任务或进度。
fn read_quests(
    connection: &Connection,
    budget: &mut PublicSeedReadBudget,
) -> Result<Vec<QuestPackageEntry>, String> {
    let mut source_quests = Vec::new();
    {
        let mut statement = connection
            .prepare(
                r#"
                SELECT id, quest_key, name, description, category, map_key, level_required,
                       repeatable, enabled
                  FROM quest
                 ORDER BY quest_key ASC
                "#,
            )
            .map_err(|error| format!("准备读取任务公共种子失败：{error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(SourceQuest {
                    id: row.get(0)?,
                    entry: QuestPackageEntry {
                        quest_key: row.get(1)?,
                        name: row.get(2)?,
                        description: row.get(3)?,
                        category: row.get(4)?,
                        map_key: row.get(5)?,
                        level_required: row.get(6)?,
                        repeatable: row.get(7)?,
                        enabled: row.get(8)?,
                        requirements: Vec::new(),
                        rewards: Vec::new(),
                    },
                })
            })
            .map_err(|error| format!("查询任务公共种子失败：{error}"))?;
        for row in rows {
            budget.consume("quest")?;
            source_quests.push(row.map_err(|error| format!("解析任务公共种子失败：{error}"))?);
        }
    }

    let mut entries = Vec::new();
    for source in source_quests {
        let mut entry = source.entry;
        entry.requirements = read_quest_requirements(connection, source.id, budget)?;
        entry.rewards = read_quest_rewards(connection, source.id, budget)?;
        entries.push(entry);
    }
    Ok(entries)
}

/// 按任务内排序读取任务条件，保留现有受控条件类型和值。
fn read_quest_requirements(
    connection: &Connection,
    quest_id: i64,
    budget: &mut PublicSeedReadBudget,
) -> Result<Vec<QuestRequirementPackageEntry>, String> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT requirement_kind, target_key, required_quantity, sort_order, description
              FROM quest_requirement
             WHERE quest_id = ?1
             ORDER BY sort_order ASC, id ASC
            "#,
        )
        .map_err(|error| format!("准备读取任务条件公共种子失败：{error}"))?;
    let rows = statement
        .query_map([quest_id], |row| {
            Ok(QuestRequirementPackageEntry {
                requirement_kind: row.get(0)?,
                target_key: row.get(1)?,
                required_quantity: row.get(2)?,
                sort_order: row.get(3)?,
                description: row.get(4)?,
            })
        })
        .map_err(|error| format!("查询任务条件公共种子失败：{error}"))?;
    let mut entries = Vec::new();
    for row in rows {
        budget.consume("quest_requirement")?;
        entries.push(row.map_err(|error| format!("解析任务条件公共种子失败：{error}"))?);
    }
    Ok(entries)
}

/// 按任务内排序读取任务奖励，保留现有受控奖励字段。
fn read_quest_rewards(
    connection: &Connection,
    quest_id: i64,
    budget: &mut PublicSeedReadBudget,
) -> Result<Vec<QuestRewardPackageEntry>, String> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT reward_kind, currency_code, item_key, amount, sort_order, description
              FROM quest_reward
             WHERE quest_id = ?1
             ORDER BY sort_order ASC, id ASC
            "#,
        )
        .map_err(|error| format!("准备读取任务奖励公共种子失败：{error}"))?;
    let rows = statement
        .query_map([quest_id], |row| {
            Ok(QuestRewardPackageEntry {
                reward_kind: row.get(0)?,
                currency_code: row.get(1)?,
                item_key: row.get(2)?,
                amount: row.get(3)?,
                sort_order: row.get(4)?,
                description: row.get(5)?,
            })
        })
        .map_err(|error| format!("查询任务奖励公共种子失败：{error}"))?;
    let mut entries = Vec::new();
    for row in rows {
        budget.consume("quest_reward")?;
        entries.push(row.map_err(|error| format!("解析任务奖励公共种子失败：{error}"))?);
    }
    Ok(entries)
}

/// 按稳定排序读取数值曲线展示目录，不导入公式或玩家数值。
fn read_numeric_curves(
    connection: &Connection,
    budget: &mut PublicSeedReadBudget,
) -> Result<Vec<NumericCurvePackageEntry>, String> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT curve_key, name, unit, range_min, range_max, reference_key,
                   description, sort_order
              FROM numeric_curve
             ORDER BY sort_order ASC, curve_key ASC
            "#,
        )
        .map_err(|error| format!("准备读取数值曲线公共种子失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(NumericCurvePackageEntry {
                curve_key: row.get(0)?,
                name: row.get(1)?,
                unit: row.get(2)?,
                range_min: row.get(3)?,
                range_max: row.get(4)?,
                reference_key: row.get(5)?,
                description: row.get(6)?,
                sort_order: row.get(7)?,
            })
        })
        .map_err(|error| format!("查询数值曲线公共种子失败：{error}"))?;
    let mut entries = Vec::new();
    for row in rows {
        budget.consume("numeric_curve")?;
        entries.push(row.map_err(|error| format!("解析数值曲线公共种子失败：{error}"))?);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::tempdir;

    use crate::content::{ContentPackage, MapPackageEntry, parse_package_text};

    use super::{
        PublicSeedPackageMetadata, import_public_seed_sqlite, write_public_seed_package_json,
    };

    fn metadata() -> PublicSeedPackageMetadata {
        PublicSeedPackageMetadata {
            package_key: "public-seed-test".to_string(),
            revision: 1,
            author: "test".to_string(),
            minimum_runtime: "0.1.20".to_string(),
        }
    }

    #[test]
    fn importer_rejects_missing_public_catalog_table() {
        let directory = tempdir().expect("应创建公共种子测试目录");
        let source = directory.path().join("incomplete.sqlite");
        let connection = Connection::open(&source).expect("应创建不完整公共种子源库");
        connection
            .execute_batch("CREATE TABLE map(map_key TEXT NOT NULL);")
            .expect("应创建地图探针表");
        drop(connection);

        let error = import_public_seed_sqlite(&source, &metadata())
            .expect_err("缺少公共目录表必须拒绝导入");
        assert!(
            error.contains("item"),
            "缺表错误应指出首个缺失目录：{error}"
        );
    }

    #[test]
    fn writer_creates_one_new_canonical_json_file() {
        let package = ContentPackage {
            package_key: "public-seed-writer".to_string(),
            revision: 1,
            author: "test".to_string(),
            minimum_runtime: "0.1.20".to_string(),
            maps: vec![MapPackageEntry {
                map_key: "public-seed-map".to_string(),
                name: "公共种子地图".to_string(),
                description: "用于验证公共种子输出。".to_string(),
                level_required: 1,
                safe: true,
                pvp_enabled: false,
                teleport_enabled: true,
                sort_order: 9_000,
            }],
            items: Vec::new(),
            npcs: Vec::new(),
            quests: Vec::new(),
            numeric_curves: Vec::new(),
            states: Vec::new(),
            wuhun: Vec::new(),
            skills: Vec::new(),
            effects: Vec::new(),
            soul_beasts: Vec::new(),
            soul_beast_skill_pools: Vec::new(),
            soul_rings: Vec::new(),
            transitions: Vec::new(),
        };
        let loaded = parse_package_text(
            &crate::content::canonical_json(&package).expect("应序列化公共种子测试包"),
            "json",
        )
        .expect("公共种子测试包应可解析");
        let directory = tempdir().expect("应创建公共种子输出目录");
        let output = directory.path().join("public-seed.json");

        write_public_seed_package_json(&output, &loaded).expect("应新建公共种子 JSON");
        let written = std::fs::read_to_string(&output).expect("应读取公共种子 JSON");
        let parsed = parse_package_text(&written, "json").expect("输出 JSON 应可重新解析");
        assert_eq!(parsed, loaded);
        assert!(
            write_public_seed_package_json(&output, &loaded).is_err(),
            "已有输出文件不能被公共种子导入器覆盖"
        );

        let mismatch_output = directory.path().join("mismatch.json");
        let mut mismatched = loaded.clone();
        mismatched.content_hash = "0".repeat(64);
        assert!(
            write_public_seed_package_json(&mismatch_output, &mismatched).is_err(),
            "哈希不一致的公共种子内容包不能被写出"
        );
        assert!(!mismatch_output.exists(), "哈希不一致时不应创建输出文件");
    }
}
