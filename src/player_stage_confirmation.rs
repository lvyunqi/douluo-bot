//! v42.1 玩家 staging 的只读候选读取与二次校验。
//!
//! 本模块只接受部署面放入 `data_dir` 的 v42.1 SQLite 文件。它不读取原始玩家库，也不写入
//! stage 文件；目标数据库确认由 Store 在独立事务中完成。
use std::{fs, path::Path};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::{
    config::is_safe_data_relative_path,
    player_staging::{SOURCE_FORMAT, STAGING_SCHEMA_VERSION},
};

const MAX_STAGING_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STAGE_PAGE_SIZE: usize = 100;

/// 只读候选页的批次摘要；来源文件哈希不通过管理 API 返回。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerStageCandidatePage {
    pub protocol: String,
    pub account_id: String,
    pub namespace: String,
    pub staged_at: i64,
    pub total_players: i64,
    pub ready_players: i64,
    pub rejected_players: i64,
    pub entries: Vec<PlayerStageCandidate>,
    pub next_after_source_player_id: Option<i64>,
}

/// 已通过 v42.1 stage 结构和基础字段校验的单条基础角色资料。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerStageCandidate {
    pub source_sha256: String,
    pub protocol: String,
    pub account_id: String,
    pub namespace: String,
    pub source_player_id: i64,
    pub subject_id: String,
    pub name: String,
    pub gender: String,
    pub level: i64,
    pub exp: i64,
    pub hp: i64,
    pub max_hp: i64,
    pub soul_power: i64,
    pub max_soul_power: i64,
    pub strength: i64,
    pub agility: i64,
    pub spirit: i64,
    pub endurance: i64,
    pub perception: i64,
    pub luck: i64,
    pub life_count: i64,
    pub state: String,
}

#[derive(Clone, Debug)]
struct StageBatch {
    source_sha256: String,
    protocol: String,
    account_id: String,
    namespace: String,
    staged_at: i64,
    total_players: i64,
    ready_players: i64,
    rejected_players: i64,
}

#[derive(Debug)]
struct StagePlayerRow {
    id: i64,
    source_player_id: i64,
    subject_id: Option<String>,
    name: Option<String>,
    gender: Option<String>,
    level: Option<i64>,
    exp: Option<i64>,
    hp: Option<i64>,
    max_hp: Option<i64>,
    soul_power: Option<i64>,
    max_soul_power: Option<i64>,
    strength: Option<i64>,
    agility: Option<i64>,
    spirit: Option<i64>,
    endurance: Option<i64>,
    perception: Option<i64>,
    luck: Option<i64>,
    life_count: Option<i64>,
    state: Option<String>,
    validation_state: String,
    issue_count: i64,
}

/// 读取受限 stage 文件中的可确认候选，分页上限与管理 API 保持一致。
pub fn list_player_stage_candidates(
    data_dir: &Path,
    stage_file: &str,
    after_source_player_id: Option<i64>,
    limit: usize,
) -> Result<PlayerStageCandidatePage, String> {
    if after_source_player_id.is_some_and(|value| value < 0)
        || !(1..=MAX_STAGE_PAGE_SIZE).contains(&limit)
    {
        return Err("玩家 staging 分页参数无效".to_string());
    }
    let (connection, batch) = open_verified_stage(data_dir, stage_file)?;
    let fetch_limit =
        i64::try_from(limit + 1).map_err(|_| "玩家 staging 分页数量超出范围".to_string())?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, source_player_id, subject_id, name, gender, level, exp, hp, max_hp,
                   soul_power, max_soul_power, strength, agility, spirit, endurance,
                   perception, luck, life_count, state, validation_state, issue_count
              FROM stage_player
             WHERE validation_state = 'ready'
               AND state = 'alive'
               AND (?1 IS NULL OR source_player_id > ?1)
             ORDER BY source_player_id ASC
             LIMIT ?2
            "#,
        )
        .map_err(|error| format!("查询玩家 staging 候选失败：{error}"))?;
    let rows = statement
        .query_map(
            params![after_source_player_id, fetch_limit],
            stage_player_row_from_row,
        )
        .map_err(|error| format!("读取玩家 staging 候选失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析玩家 staging 候选失败：{error}"))?;

    let mut entries = Vec::with_capacity(rows.len().min(limit));
    for row in rows.iter().take(limit) {
        entries.push(candidate_from_row(&connection, &batch, row)?);
    }
    let next_after_source_player_id = rows.get(limit).map(|row| row.source_player_id);
    Ok(PlayerStageCandidatePage {
        protocol: batch.protocol,
        account_id: batch.account_id,
        namespace: batch.namespace,
        staged_at: batch.staged_at,
        total_players: batch.total_players,
        ready_players: batch.ready_players,
        rejected_players: batch.rejected_players,
        entries,
        next_after_source_player_id,
    })
}

/// 读取一个候选供确认事务使用，任何非可确认状态都在此处拒绝。
pub fn load_player_stage_candidate(
    data_dir: &Path,
    stage_file: &str,
    source_player_id: i64,
) -> Result<PlayerStageCandidate, String> {
    if source_player_id <= 0 {
        return Err("玩家 staging 源角色 ID 无效".to_string());
    }
    let (connection, batch) = open_verified_stage(data_dir, stage_file)?;
    let row = connection
        .query_row(
            r#"
            SELECT id, source_player_id, subject_id, name, gender, level, exp, hp, max_hp,
                   soul_power, max_soul_power, strength, agility, spirit, endurance,
                   perception, luck, life_count, state, validation_state, issue_count
              FROM stage_player
             WHERE source_player_id = ?1
            "#,
            [source_player_id],
            stage_player_row_from_row,
        )
        .optional()
        .map_err(|error| format!("读取玩家 staging 候选失败：{error}"))?
        .ok_or_else(|| "玩家 staging 候选不存在".to_string())?;
    candidate_from_row(&connection, &batch, &row)
}

/// Store 在写入前复用基础字段校验，避免调用方绕过 stage loader 伪造候选。
pub(crate) fn validate_player_stage_candidate(
    candidate: &PlayerStageCandidate,
) -> Result<(), String> {
    if !is_lower_hex_digest(&candidate.source_sha256) {
        return Err("玩家 staging 来源摘要无效".to_string());
    }
    if candidate.protocol != "onebot11" {
        return Err("玩家 staging 协议无效".to_string());
    }
    validate_account_id(&candidate.account_id)?;
    validate_namespace(&candidate.namespace)?;
    if candidate.source_player_id <= 0 {
        return Err("玩家 staging 源角色 ID 无效".to_string());
    }
    if !valid_text(&candidate.subject_id, 256, false) {
        return Err("玩家 staging 身份 ID 无效".to_string());
    }
    if !valid_text(&candidate.name, 128, true) {
        return Err("玩家 staging 角色名称无效".to_string());
    }
    if !matches!(candidate.gender.as_str(), "男" | "女") {
        return Err("玩家 staging 性别无效".to_string());
    }
    if !(1..=120).contains(&candidate.level) || candidate.exp < 0 {
        return Err("玩家 staging 等级或经验无效".to_string());
    }
    if candidate.hp < 0 || candidate.max_hp <= 0 || candidate.hp > candidate.max_hp {
        return Err("玩家 staging 生命值无效".to_string());
    }
    if candidate.soul_power < 0
        || candidate.max_soul_power <= 0
        || candidate.soul_power > candidate.max_soul_power
    {
        return Err("玩家 staging 魂力无效".to_string());
    }
    if [
        candidate.strength,
        candidate.agility,
        candidate.spirit,
        candidate.endurance,
        candidate.perception,
        candidate.luck,
    ]
    .into_iter()
    .any(|value| value < 0)
    {
        return Err("玩家 staging 基础属性无效".to_string());
    }
    if !(1..=3).contains(&candidate.life_count) {
        return Err("玩家 staging 转生次数无效".to_string());
    }
    if candidate.state != "alive" {
        return Err("玩家 staging 状态不可确认".to_string());
    }
    Ok(())
}

/// 打开并复核 stage 文件，避免 HTTP 层变成任意文件读取入口。
fn open_verified_stage(
    data_dir: &Path,
    stage_file: &str,
) -> Result<(Connection, StageBatch), String> {
    if !valid_stage_file_path(stage_file) {
        return Err("玩家 staging 文件路径无效".to_string());
    }
    let root = fs::canonicalize(data_dir)
        .map_err(|error| format!("解析玩家 staging 根目录失败：{error}"))?;
    if !root.is_dir() {
        return Err("玩家 staging 根目录无效".to_string());
    }
    let candidate = root.join(stage_file);
    let metadata = fs::symlink_metadata(&candidate)
        .map_err(|error| format!("读取玩家 staging 文件失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("玩家 staging 文件必须是常规 SQLite 文件".to_string());
    }
    if metadata.len() > MAX_STAGING_BYTES {
        return Err("玩家 staging 文件超过大小上限".to_string());
    }
    let path = fs::canonicalize(&candidate)
        .map_err(|error| format!("解析玩家 staging 文件路径失败：{error}"))?;
    if !path.starts_with(&root) {
        return Err("玩家 staging 文件必须位于 data_dir 内".to_string());
    }
    let connection = open_read_only_stage(&path)?;
    verify_stage_schema(&connection)?;
    let batch = load_stage_batch(&connection)?;
    verify_stage_batch_integrity(&connection, &batch)?;
    Ok((connection, batch))
}

fn valid_stage_file_path(stage_file: &str) -> bool {
    is_safe_data_relative_path(stage_file)
        && Path::new(stage_file)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("sqlite"))
}

fn open_read_only_stage(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("以只读模式打开玩家 staging 失败：{error}"))?;
    connection
        .execute_batch("PRAGMA query_only = ON;")
        .map_err(|error| format!("限制玩家 staging 为只读失败：{error}"))?;
    Ok(connection)
}

fn verify_stage_schema(connection: &Connection) -> Result<(), String> {
    verify_stage_table_columns(
        connection,
        "stage_batch",
        &[
            ("id", "INTEGER"),
            ("schema_version", "INTEGER"),
            ("source_format", "TEXT"),
            ("source_sha256", "TEXT"),
            ("protocol", "TEXT"),
            ("account_id", "TEXT"),
            ("namespace", "TEXT"),
            ("staged_at", "INTEGER"),
            ("total_players", "INTEGER"),
            ("ready_players", "INTEGER"),
            ("rejected_players", "INTEGER"),
        ],
    )?;
    verify_stage_table_columns(
        connection,
        "stage_player",
        &[
            ("id", "INTEGER"),
            ("source_player_id", "INTEGER"),
            ("subject_id", "TEXT"),
            ("name", "TEXT"),
            ("gender", "TEXT"),
            ("level", "INTEGER"),
            ("exp", "INTEGER"),
            ("hp", "INTEGER"),
            ("max_hp", "INTEGER"),
            ("soul_power", "INTEGER"),
            ("max_soul_power", "INTEGER"),
            ("strength", "INTEGER"),
            ("agility", "INTEGER"),
            ("spirit", "INTEGER"),
            ("endurance", "INTEGER"),
            ("perception", "INTEGER"),
            ("luck", "INTEGER"),
            ("life_count", "INTEGER"),
            ("state", "TEXT"),
            ("validation_state", "TEXT"),
            ("issue_count", "INTEGER"),
        ],
    )?;
    verify_stage_table_columns(
        connection,
        "stage_issue",
        &[
            ("id", "INTEGER"),
            ("stage_player_id", "INTEGER"),
            ("field", "TEXT"),
            ("code", "TEXT"),
        ],
    )
}

fn verify_stage_table_columns(
    connection: &Connection,
    table: &str,
    expected: &[(&str, &str)],
) -> Result<(), String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_xinfo(\"{table}\")"))
        .map_err(|error| format!("读取玩家 staging 表结构失败：{error}"))?;
    let actual = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(|error| format!("查询玩家 staging 表结构失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析玩家 staging 表结构失败：{error}"))?;
    let expected = expected
        .iter()
        .map(|(name, sql_type)| ((*name).to_string(), (*sql_type).to_string()))
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(format!("玩家 staging 表 {table} 结构不匹配"));
    }
    let sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取玩家 staging 表声明失败：{error}"))?
        .ok_or_else(|| format!("玩家 staging 缺少表 {table}"))?;
    if !sql.to_ascii_uppercase().contains(") STRICT") {
        return Err(format!("玩家 staging 表 {table} 不是严格表"));
    }
    Ok(())
}

fn load_stage_batch(connection: &Connection) -> Result<StageBatch, String> {
    let count = connection
        .query_row("SELECT COUNT(*) FROM stage_batch", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| format!("统计玩家 staging 批次失败：{error}"))?;
    if count != 1 {
        return Err("玩家 staging 必须恰好包含一个批次".to_string());
    }
    let (schema_version, source_format, batch) = connection
        .query_row(
            r#"
            SELECT schema_version, source_format, source_sha256, protocol, account_id, namespace,
                   staged_at, total_players, ready_players, rejected_players
              FROM stage_batch
             WHERE id = 1
            "#,
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    StageBatch {
                        source_sha256: row.get(2)?,
                        protocol: row.get(3)?,
                        account_id: row.get(4)?,
                        namespace: row.get(5)?,
                        staged_at: row.get(6)?,
                        total_players: row.get(7)?,
                        ready_players: row.get(8)?,
                        rejected_players: row.get(9)?,
                    },
                ))
            },
        )
        .optional()
        .map_err(|error| format!("读取玩家 staging 批次失败：{error}"))?
        .ok_or_else(|| "玩家 staging 缺少主批次".to_string())?;
    if schema_version != STAGING_SCHEMA_VERSION || source_format != SOURCE_FORMAT {
        return Err("玩家 staging 版本或来源格式不受支持".to_string());
    }
    if !is_lower_hex_digest(&batch.source_sha256) || batch.protocol != "onebot11" {
        return Err("玩家 staging 批次元数据无效".to_string());
    }
    validate_account_id(&batch.account_id)?;
    validate_namespace(&batch.namespace)?;
    if batch.staged_at < 0
        || batch.total_players < 0
        || batch.ready_players < 0
        || batch.rejected_players < 0
        || batch.total_players != batch.ready_players + batch.rejected_players
    {
        return Err("玩家 staging 批次统计无效".to_string());
    }
    Ok(batch)
}

fn verify_stage_batch_integrity(connection: &Connection, batch: &StageBatch) -> Result<(), String> {
    let (total_players, ready_players, rejected_players, invalid_issue_count) = connection
        .query_row(
            r#"
            SELECT
                COUNT(*),
                SUM(validation_state = 'ready'),
                SUM(validation_state = 'rejected'),
                EXISTS(
                    SELECT 1
                      FROM stage_player player
                     WHERE player.validation_state NOT IN ('ready', 'rejected')
                        OR (player.validation_state = 'ready' AND player.issue_count <> 0)
                        OR (player.validation_state = 'rejected' AND player.issue_count <= 0)
                        OR player.issue_count <> (
                            SELECT COUNT(*) FROM stage_issue issue WHERE issue.stage_player_id = player.id
                        )
                )
              FROM stage_player
            "#,
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    row.get::<_, bool>(3)?,
                ))
            },
        )
        .map_err(|error| format!("校验玩家 staging 批次失败：{error}"))?;
    if invalid_issue_count
        || total_players != batch.total_players
        || ready_players != batch.ready_players
        || rejected_players != batch.rejected_players
    {
        return Err("玩家 staging 记录与批次摘要不一致".to_string());
    }
    Ok(())
}

fn stage_player_row_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StagePlayerRow> {
    Ok(StagePlayerRow {
        id: row.get(0)?,
        source_player_id: row.get(1)?,
        subject_id: row.get(2)?,
        name: row.get(3)?,
        gender: row.get(4)?,
        level: row.get(5)?,
        exp: row.get(6)?,
        hp: row.get(7)?,
        max_hp: row.get(8)?,
        soul_power: row.get(9)?,
        max_soul_power: row.get(10)?,
        strength: row.get(11)?,
        agility: row.get(12)?,
        spirit: row.get(13)?,
        endurance: row.get(14)?,
        perception: row.get(15)?,
        luck: row.get(16)?,
        life_count: row.get(17)?,
        state: row.get(18)?,
        validation_state: row.get(19)?,
        issue_count: row.get(20)?,
    })
}

fn candidate_from_row(
    connection: &Connection,
    batch: &StageBatch,
    row: &StagePlayerRow,
) -> Result<PlayerStageCandidate, String> {
    if row.validation_state != "ready" || row.issue_count != 0 {
        return Err("玩家 staging 候选尚不可确认".to_string());
    }
    let actual_issue_count = connection
        .query_row(
            "SELECT COUNT(*) FROM stage_issue WHERE stage_player_id = ?1",
            [row.id],
            |result| result.get::<_, i64>(0),
        )
        .map_err(|error| format!("读取玩家 staging 校验问题失败：{error}"))?;
    if actual_issue_count != 0 {
        return Err("玩家 staging 候选仍包含校验问题".to_string());
    }
    let candidate = PlayerStageCandidate {
        source_sha256: batch.source_sha256.clone(),
        protocol: batch.protocol.clone(),
        account_id: batch.account_id.clone(),
        namespace: batch.namespace.clone(),
        source_player_id: row.source_player_id,
        subject_id: required_text(&row.subject_id, "身份 ID")?,
        name: required_text(&row.name, "角色名称")?,
        gender: required_text(&row.gender, "性别")?,
        level: required_number(row.level, "等级")?,
        exp: required_number(row.exp, "经验")?,
        hp: required_number(row.hp, "生命")?,
        max_hp: required_number(row.max_hp, "生命上限")?,
        soul_power: required_number(row.soul_power, "魂力")?,
        max_soul_power: required_number(row.max_soul_power, "魂力上限")?,
        strength: required_number(row.strength, "力量")?,
        agility: required_number(row.agility, "敏捷")?,
        spirit: required_number(row.spirit, "精神")?,
        endurance: required_number(row.endurance, "耐力")?,
        perception: required_number(row.perception, "感知")?,
        luck: required_number(row.luck, "幸运")?,
        life_count: required_number(row.life_count, "转生次数")?,
        state: required_text(&row.state, "状态")?,
    };
    validate_player_stage_candidate(&candidate)?;
    Ok(candidate)
}

fn required_text(value: &Option<String>, field: &str) -> Result<String, String> {
    value
        .as_ref()
        .cloned()
        .ok_or_else(|| format!("玩家 staging 候选缺少{field}"))
}

fn required_number(value: Option<i64>, field: &str) -> Result<i64, String> {
    value.ok_or_else(|| format!("玩家 staging 候选缺少{field}"))
}

fn validate_account_id(value: &str) -> Result<(), String> {
    if !valid_text(value, 128, true) {
        return Err("玩家 staging 机器人 account_id 无效".to_string());
    }
    Ok(())
}

fn validate_namespace(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("玩家 staging namespace 无效".to_string());
    }
    Ok(())
}

fn valid_text(value: &str, max_chars: usize, require_trimmed: bool) -> bool {
    let length = value.chars().count();
    (1..=max_chars).contains(&length)
        && !value.chars().any(char::is_control)
        && (!require_trimmed || value == value.trim())
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value.bytes().all(|byte| {
            byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
        })
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::{list_player_stage_candidates, load_player_stage_candidate};
    use crate::player_staging::{PlayerStagingMetadata, stage_recent_sqlite_player_profiles};

    fn metadata() -> PlayerStagingMetadata {
        PlayerStagingMetadata {
            protocol: "onebot11".to_string(),
            account_id: "10001".to_string(),
            namespace: "default".to_string(),
        }
    }

    fn create_recent_player_source(path: &std::path::Path) -> Connection {
        let connection = Connection::open(path).expect("应创建最近玩家源库");
        connection
            .execute_batch(
                r#"
                CREATE TABLE player(
                    id INTEGER PRIMARY KEY,
                    user_id INTEGER,
                    name TEXT,
                    nickname TEXT,
                    sex TEXT,
                    level INTEGER,
                    exp INTEGER,
                    hp INTEGER,
                    max_hp INTEGER,
                    mp INTEGER,
                    max_mp INTEGER,
                    strength INTEGER,
                    agility INTEGER,
                    spirit INTEGER,
                    endurance INTEGER,
                    perception INTEGER,
                    luck INTEGER,
                    life_count INTEGER,
                    state INTEGER
                );
                "#,
            )
            .expect("应创建最近玩家表");
        connection
    }

    #[test]
    fn reads_only_alive_ready_candidates_from_generated_stage() {
        let directory = tempdir().expect("应创建 stage 读取临时目录");
        let source_path = directory.path().join("recent.sqlite");
        let source = create_recent_player_source(&source_path);
        source
            .execute_batch(
                r#"
                INSERT INTO player VALUES(
                    1, 20001, '可确认角色', NULL, '男', 1, 0, 100, 100, 50, 50,
                    10, 11, 12, 13, 14, 15, 1, 0
                );
                INSERT INTO player VALUES(
                    2, 20002, '死亡角色', NULL, '女', 1, 0, 100, 100, 50, 50,
                    10, 11, 12, 13, 14, 15, 1, 1
                );
                "#,
            )
            .expect("应写入最近玩家资料");
        drop(source);
        let data_dir = directory.path().join("data");
        std::fs::create_dir(&data_dir).expect("应创建 data_dir");
        let stage_path = data_dir.join("players.sqlite");
        stage_recent_sqlite_player_profiles(&source_path, &stage_path, &metadata())
            .expect("应生成玩家 stage");

        let page = list_player_stage_candidates(&data_dir, "players.sqlite", None, 20)
            .expect("应读取可确认候选");
        assert_eq!(page.total_players, 2);
        assert_eq!(page.ready_players, 1);
        assert_eq!(page.rejected_players, 1);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].source_player_id, 1);
        assert_eq!(page.entries[0].subject_id, "20001");
        assert_eq!(page.entries[0].state, "alive");
        assert!(
            load_player_stage_candidate(&data_dir, "players.sqlite", 2).is_err(),
            "非存活 stage 角色必须拒绝确认"
        );
        assert!(
            list_player_stage_candidates(&data_dir, "../recent.sqlite", None, 20).is_err(),
            "路径不得逃离 data_dir"
        );

        let stage = Connection::open(&stage_path).expect("应打开结构损坏测试 stage");
        stage
            .execute_batch("ALTER TABLE stage_player ADD COLUMN unexpected TEXT;")
            .expect("应构造非 v42.1 stage 结构");
        drop(stage);
        assert!(
            list_player_stage_candidates(&data_dir, "players.sqlite", None, 20).is_err(),
            "非 v42.1 stage 结构必须拒绝读取"
        );
    }
}
