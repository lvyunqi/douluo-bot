//! 最近 SQLite 角色资料的离线 staging 与校验。

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use sha2::{Digest, Sha256};

const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SOURCE_PLAYERS: usize = 10_000;
pub(crate) const STAGING_SCHEMA_VERSION: i64 = 1;
const STAGING_OUTPUT_EXTENSION: &str = "sqlite";
pub(crate) const SOURCE_FORMAT: &str = "recent-sqlite-player-v1";
const REQUIRED_PLAYER_COLUMNS: [&str; 19] = [
    "id",
    "user_id",
    "name",
    "nickname",
    "sex",
    "level",
    "exp",
    "hp",
    "max_hp",
    "mp",
    "max_mp",
    "strength",
    "agility",
    "spirit",
    "endurance",
    "perception",
    "luck",
    "life_count",
    "state",
];

/// 角色资料 staging 必须由操作者显式提供的稳定身份作用域。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerStagingMetadata {
    pub protocol: String,
    pub account_id: String,
    pub namespace: String,
}

/// 已写入 staging 文件的脱敏批次摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerStageSummary {
    pub source_sha256: String,
    pub total_players: usize,
    pub ready_players: usize,
    pub rejected_players: usize,
    pub issue_counts: BTreeMap<String, usize>,
}

struct SourcePlayer {
    id: i64,
    user_id: Option<i64>,
    name: Option<String>,
    nickname: Option<String>,
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
    state: Option<i64>,
}

struct StageIssue {
    field: &'static str,
    code: &'static str,
}

struct StagedPlayer {
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
    issues: Vec<StageIssue>,
}

impl StagedPlayer {
    fn is_ready(&self) -> bool {
        self.issues.is_empty()
    }
}

/// 只读读取最近 SQLite 的基础角色资料，并以新建文件语义写入本地 staging SQLite。
///
/// 本函数不会连接目标游戏库，也不会读取魂环、魂技、背包、钱包、地图、NPC 或战斗数据。
pub fn stage_recent_sqlite_player_profiles(
    source_path: &Path,
    output_path: &Path,
    metadata: &PlayerStagingMetadata,
) -> Result<PlayerStageSummary, String> {
    validate_metadata(metadata)?;
    let source_sha256 = hash_regular_source_file(source_path)?;
    let source = open_read_only_source(source_path)?;
    require_recent_player_schema(&source)?;

    let mut players = read_source_players(&source)?;
    add_duplicate_subject_issues(&mut players);
    let summary = summarize_players(&source_sha256, &players);
    write_staging_database(output_path, metadata, &summary, &players)?;
    Ok(summary)
}

/// 验证调用方明确选择的稳定身份作用域，避免后续确认阶段混合不同机器人数据。
fn validate_metadata(metadata: &PlayerStagingMetadata) -> Result<(), String> {
    if metadata.protocol != "onebot11" {
        return Err("v42.1 玩家 staging 只支持 protocol=onebot11".to_string());
    }
    if metadata.account_id != metadata.account_id.trim() || !valid_text(&metadata.account_id, 128) {
        return Err("机器人 account_id 必须是 1 到 128 个无控制字符的非空字符串".to_string());
    }
    if metadata.namespace.is_empty()
        || metadata.namespace.len() > 64
        || !metadata
            .namespace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("身份 namespace 无效".to_string());
    }
    Ok(())
}

/// 只接受受大小限制的常规源文件，并在读取前计算来源指纹而不保存路径。
fn hash_regular_source_file(source_path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(source_path)
        .map_err(|error| format!("读取玩家 staging 源文件失败：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("玩家 staging 源文件不能是符号链接".to_string());
    }
    if !metadata.is_file() {
        return Err("玩家 staging 源文件必须是常规 SQLite 文件".to_string());
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "玩家 staging 源文件不能超过 {MAX_SOURCE_BYTES} 字节"
        ));
    }

    let mut source =
        File::open(source_path).map_err(|error| format!("打开玩家 staging 源文件失败：{error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(|error| format!("计算玩家 staging 源文件哈希失败：{error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 以 SQLite 只读连接打开已验证的源库，阻止 staging 修改任何源数据。
fn open_read_only_source(source_path: &Path) -> Result<Connection, String> {
    let connection = Connection::open_with_flags(
        source_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("以只读模式打开玩家 staging 源库失败：{error}"))?;
    connection
        .execute_batch("PRAGMA query_only = ON;")
        .map_err(|error| format!("限制玩家 staging 源库为只读失败：{error}"))?;
    Ok(connection)
}

/// 严格要求最近 Spring/SQLite player 表的完整字段，避免以猜测方式兼容未知版本。
fn require_recent_player_schema(connection: &Connection) -> Result<(), String> {
    let player_exists = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'player')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| format!("检查玩家 staging 源表失败：{error}"))?;
    if !player_exists {
        return Err("玩家 staging 源库缺少必需表：player".to_string());
    }

    let mut statement = connection
        .prepare("PRAGMA table_info(player)")
        .map_err(|error| format!("读取玩家 staging 表结构失败：{error}"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("查询玩家 staging 表结构失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析玩家 staging 表结构失败：{error}"))?;
    for required in REQUIRED_PLAYER_COLUMNS {
        if !columns.iter().any(|column| column == required) {
            return Err(format!("玩家 staging 源表缺少必需字段：{required}"));
        }
    }
    Ok(())
}

/// 只读取基础角色资料列；所有魂环、魂技、资产、地图和战斗列都不在查询中。
fn read_source_players(connection: &Connection) -> Result<Vec<StagedPlayer>, String> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, user_id, name, nickname, sex, level, exp, hp, max_hp, mp, max_mp,
                   strength, agility, spirit, endurance, perception, luck, life_count, state
              FROM player
             ORDER BY id ASC
            "#,
        )
        .map_err(|error| format!("准备读取玩家 staging 源资料失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(SourcePlayer {
                id: row.get(0)?,
                user_id: row.get(1)?,
                name: row.get(2)?,
                nickname: row.get(3)?,
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
            })
        })
        .map_err(|error| format!("查询玩家 staging 源资料失败：{error}"))?;

    let mut players = Vec::new();
    for row in rows {
        if players.len() >= MAX_SOURCE_PLAYERS {
            return Err(format!(
                "玩家 staging 源资料不能超过 {MAX_SOURCE_PLAYERS} 条"
            ));
        }
        let source = row.map_err(|error| format!("解析玩家 staging 源资料失败：{error}"))?;
        players.push(normalize_source_player(source));
    }
    Ok(players)
}

/// 将可确认的最近 SQLite 角色字段转换为目标模型可验证的基础资料。
fn normalize_source_player(source: SourcePlayer) -> StagedPlayer {
    let mut issues = Vec::new();
    if source.id <= 0 {
        issues.push(StageIssue {
            field: "id",
            code: "source_player_id_invalid",
        });
    }

    let subject_id = match source.user_id {
        Some(user_id) if user_id > 0 => Some(user_id.to_string()),
        _ => {
            issues.push(StageIssue {
                field: "user_id",
                code: "subject_id_invalid",
            });
            None
        }
    };
    let name_source = source
        .name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or(source.nickname.as_deref());
    let name = normalize_required_text(name_source, "name", 128, &mut issues);
    let gender = normalize_gender(source.gender.as_deref(), &mut issues);
    let level = normalize_range(source.level, "level", 1, 120, &mut issues);
    let exp = normalize_minimum(source.exp, "exp", 0, &mut issues);
    let max_hp = normalize_minimum(source.max_hp, "max_hp", 1, &mut issues);
    let hp = normalize_minimum(source.hp, "hp", 0, &mut issues);
    if let (Some(hp), Some(max_hp)) = (hp, max_hp)
        && hp > max_hp
    {
        issues.push(StageIssue {
            field: "hp",
            code: "hp_exceeds_max_hp",
        });
    }
    let max_soul_power = normalize_minimum(source.max_soul_power, "max_mp", 1, &mut issues);
    let soul_power = normalize_minimum(source.soul_power, "mp", 0, &mut issues);
    if let (Some(soul_power), Some(max_soul_power)) = (soul_power, max_soul_power)
        && soul_power > max_soul_power
    {
        issues.push(StageIssue {
            field: "mp",
            code: "mp_exceeds_max_mp",
        });
    }

    let strength = normalize_minimum(source.strength, "strength", 0, &mut issues);
    let agility = normalize_minimum(source.agility, "agility", 0, &mut issues);
    let spirit = normalize_minimum(source.spirit, "spirit", 0, &mut issues);
    let endurance = normalize_minimum(source.endurance, "endurance", 0, &mut issues);
    let perception = normalize_minimum(source.perception, "perception", 0, &mut issues);
    let luck = normalize_minimum(source.luck, "luck", 0, &mut issues);
    let life_count = normalize_range(source.life_count, "life_count", 1, 3, &mut issues);
    let state = normalize_state(source.state, &mut issues);

    StagedPlayer {
        source_player_id: source.id,
        subject_id,
        name,
        gender,
        level,
        exp,
        hp,
        max_hp,
        soul_power,
        max_soul_power,
        strength,
        agility,
        spirit,
        endurance,
        perception,
        luck,
        life_count,
        state,
        issues,
    }
}

/// 验证必填文本后仅保留去除首尾空白的规范值。
fn normalize_required_text(
    value: Option<&str>,
    field: &'static str,
    max_chars: usize,
    issues: &mut Vec<StageIssue>,
) -> Option<String> {
    let Some(value) = value else {
        issues.push(StageIssue {
            field,
            code: "required_text_missing",
        });
        return None;
    };
    let normalized = value.trim();
    if !valid_text(normalized, max_chars) {
        issues.push(StageIssue {
            field,
            code: "text_invalid",
        });
        return None;
    }
    Some(normalized.to_string())
}

/// 仅接受当前目标角色模型允许的两种性别值。
fn normalize_gender(value: Option<&str>, issues: &mut Vec<StageIssue>) -> Option<String> {
    let value = normalize_required_text(value, "sex", 3, issues)?;
    if !matches!(value.as_str(), "男" | "女") {
        issues.push(StageIssue {
            field: "sex",
            code: "gender_invalid",
        });
        return None;
    }
    Some(value)
}

/// 读取必须落入闭区间的整数，缺失与越界均成为无原始值的校验问题。
fn normalize_range(
    value: Option<i64>,
    field: &'static str,
    minimum: i64,
    maximum: i64,
    issues: &mut Vec<StageIssue>,
) -> Option<i64> {
    match value {
        Some(value) if (minimum..=maximum).contains(&value) => Some(value),
        Some(_) => {
            issues.push(StageIssue {
                field,
                code: "number_out_of_range",
            });
            None
        }
        None => {
            issues.push(StageIssue {
                field,
                code: "number_missing",
            });
            None
        }
    }
}

/// 读取不小于下界的整数，避免在确认导入前接受负资产或非法上限。
fn normalize_minimum(
    value: Option<i64>,
    field: &'static str,
    minimum: i64,
    issues: &mut Vec<StageIssue>,
) -> Option<i64> {
    match value {
        Some(value) if value >= minimum => Some(value),
        Some(_) => {
            issues.push(StageIssue {
                field,
                code: "number_out_of_range",
            });
            None
        }
        None => {
            issues.push(StageIssue {
                field,
                code: "number_missing",
            });
            None
        }
    }
}

/// 将最近项目的整数状态码转成当前角色模型的规范状态值。
fn normalize_state(value: Option<i64>, issues: &mut Vec<StageIssue>) -> Option<String> {
    let state = match value {
        Some(0) => "alive",
        Some(1) => "dead",
        Some(2) => "reviving",
        Some(3) => "deleted",
        Some(_) => {
            issues.push(StageIssue {
                field: "state",
                code: "state_invalid",
            });
            return None;
        }
        None => {
            issues.push(StageIssue {
                field: "state",
                code: "number_missing",
            });
            return None;
        }
    };
    if state != "alive" {
        // 死亡、复活中和封存角色没有可安全恢复的最小状态，必须人工重新开始。
        issues.push(StageIssue {
            field: "state",
            code: "state_not_confirmable",
        });
    }
    Some(state.to_string())
}

/// 标记源库中同一稳定用户标识的多条记录，后续确认阶段不得静默覆盖。
fn add_duplicate_subject_issues(players: &mut [StagedPlayer]) {
    let mut subject_rows: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, player) in players.iter().enumerate() {
        if let Some(subject_id) = player.subject_id.as_ref() {
            subject_rows
                .entry(subject_id.clone())
                .or_default()
                .push(index);
        }
    }
    for indexes in subject_rows.values().filter(|indexes| indexes.len() > 1) {
        for &index in indexes {
            players[index].issues.push(StageIssue {
                field: "user_id",
                code: "duplicate_subject_id",
            });
        }
    }
}

/// 汇总可确认与拒绝记录，并只公开固定错误码计数。
fn summarize_players(source_sha256: &str, players: &[StagedPlayer]) -> PlayerStageSummary {
    let mut issue_counts = BTreeMap::new();
    let mut ready_players = 0;
    for player in players {
        if player.is_ready() {
            ready_players += 1;
        }
        for issue in &player.issues {
            *issue_counts.entry(issue.code.to_string()).or_insert(0) += 1;
        }
    }
    PlayerStageSummary {
        source_sha256: source_sha256.to_string(),
        total_players: players.len(),
        ready_players,
        rejected_players: players.len() - ready_players,
        issue_counts,
    }
}

/// 建立新 staging SQLite 并在同一事务中写入批次、角色资料和校验问题。
fn write_staging_database(
    output_path: &Path,
    metadata: &PlayerStagingMetadata,
    summary: &PlayerStageSummary,
    players: &[StagedPlayer],
) -> Result<(), String> {
    validate_output_path(output_path)?;
    let mut connection = create_new_staging_database(output_path)?;
    let write_result = (|| -> Result<(), String> {
        connection
            .execute_batch(STAGING_SCHEMA)
            .map_err(|error| format!("初始化玩家 staging 数据库失败：{error}"))?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始玩家 staging 写入事务失败：{error}"))?;
        transaction
            .execute(
                r#"
                INSERT INTO stage_batch(
                    id, schema_version, source_format, source_sha256, protocol, account_id,
                    namespace, staged_at, total_players, ready_players, rejected_players
                ) VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    STAGING_SCHEMA_VERSION,
                    SOURCE_FORMAT,
                    summary.source_sha256,
                    metadata.protocol,
                    metadata.account_id,
                    metadata.namespace,
                    now_timestamp()?,
                    i64::try_from(summary.total_players)
                        .map_err(|_| "玩家 staging 总记录数超出范围".to_string())?,
                    i64::try_from(summary.ready_players)
                        .map_err(|_| "玩家 staging 可确认记录数超出范围".to_string())?,
                    i64::try_from(summary.rejected_players)
                        .map_err(|_| "玩家 staging 拒绝记录数超出范围".to_string())?,
                ],
            )
            .map_err(|error| format!("写入玩家 staging 批次失败：{error}"))?;

        for player in players {
            transaction
                .execute(
                    r#"
                    INSERT INTO stage_player(
                        source_player_id, subject_id, name, gender, level, exp, hp, max_hp,
                        soul_power, max_soul_power, strength, agility, spirit, endurance,
                        perception, luck, life_count, state, validation_state, issue_count
                    ) VALUES(
                        ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                        ?15, ?16, ?17, ?18, ?19, ?20
                    )
                    "#,
                    params![
                        player.source_player_id,
                        player.subject_id,
                        player.name,
                        player.gender,
                        player.level,
                        player.exp,
                        player.hp,
                        player.max_hp,
                        player.soul_power,
                        player.max_soul_power,
                        player.strength,
                        player.agility,
                        player.spirit,
                        player.endurance,
                        player.perception,
                        player.luck,
                        player.life_count,
                        player.state,
                        if player.is_ready() {
                            "ready"
                        } else {
                            "rejected"
                        },
                        i64::try_from(player.issues.len())
                            .map_err(|_| "玩家 staging 校验问题数量超出范围".to_string())?,
                    ],
                )
                .map_err(|error| format!("写入玩家 staging 资料失败：{error}"))?;
            let stage_player_id = transaction.last_insert_rowid();
            for issue in &player.issues {
                transaction
                    .execute(
                        "INSERT INTO stage_issue(stage_player_id, field, code) VALUES(?1, ?2, ?3)",
                        params![stage_player_id, issue.field, issue.code],
                    )
                    .map_err(|error| format!("写入玩家 staging 校验问题失败：{error}"))?;
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("提交玩家 staging 写入事务失败：{error}"))
    })();
    drop(connection);
    if let Err(error) = write_result {
        let _ = fs::remove_file(output_path);
        return Err(error);
    }
    Ok(())
}

/// 仅接受显式的新 SQLite 输出文件，避免误把 staging 写到其他格式或覆盖既有数据。
fn validate_output_path(output_path: &Path) -> Result<(), String> {
    if !output_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(STAGING_OUTPUT_EXTENSION))
    {
        return Err("玩家 staging 输出文件必须使用 .sqlite 扩展名".to_string());
    }
    Ok(())
}

/// 用 create_new 预留输出路径；失败时绝不打开、截断或替换已有文件。
fn create_new_staging_database(output_path: &Path) -> Result<Connection, String> {
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_path)
        .map_err(|error| format!("新建玩家 staging 输出文件失败：{error}"))?;
    drop(output);
    match Connection::open_with_flags(
        output_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(connection) => Ok(connection),
        Err(error) => {
            let _ = fs::remove_file(output_path);
            Err(format!("打开玩家 staging 输出数据库失败：{error}"))
        }
    }
}

/// 获得用于批次元数据的 Unix 秒时间戳，系统时间异常时拒绝生成不可信 staging。
fn now_timestamp() -> Result<i64, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("读取玩家 staging 系统时间失败：{error}"))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| "玩家 staging 系统时间超出范围".to_string())
}

/// 验证文本既非空、不过长，也不携带控制字符。
fn valid_text(value: &str, max_chars: usize) -> bool {
    let length = value.chars().count();
    (1..=max_chars).contains(&length) && !value.chars().any(char::is_control)
}

const STAGING_SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE stage_batch (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    schema_version INTEGER NOT NULL CHECK(schema_version = 1),
    source_format TEXT NOT NULL CHECK(source_format = 'recent-sqlite-player-v1'),
    source_sha256 TEXT NOT NULL CHECK(length(source_sha256) = 64),
    protocol TEXT NOT NULL CHECK(protocol = 'onebot11'),
    account_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    staged_at INTEGER NOT NULL CHECK(staged_at >= 0),
    total_players INTEGER NOT NULL CHECK(total_players >= 0),
    ready_players INTEGER NOT NULL CHECK(ready_players >= 0),
    rejected_players INTEGER NOT NULL CHECK(rejected_players >= 0),
    CHECK(total_players = ready_players + rejected_players)
) STRICT;

CREATE TABLE stage_player (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_player_id INTEGER NOT NULL UNIQUE,
    subject_id TEXT,
    name TEXT,
    gender TEXT,
    level INTEGER,
    exp INTEGER,
    hp INTEGER,
    max_hp INTEGER,
    soul_power INTEGER,
    max_soul_power INTEGER,
    strength INTEGER,
    agility INTEGER,
    spirit INTEGER,
    endurance INTEGER,
    perception INTEGER,
    luck INTEGER,
    life_count INTEGER,
    state TEXT,
    validation_state TEXT NOT NULL CHECK(validation_state IN ('ready', 'rejected')),
    issue_count INTEGER NOT NULL CHECK(issue_count >= 0),
    CHECK(
        (validation_state = 'ready' AND issue_count = 0)
        OR (validation_state = 'rejected' AND issue_count > 0)
    )
) STRICT;

CREATE TABLE stage_issue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    stage_player_id INTEGER NOT NULL REFERENCES stage_player(id) ON DELETE CASCADE,
    field TEXT NOT NULL,
    code TEXT NOT NULL,
    UNIQUE(stage_player_id, field, code)
) STRICT;

CREATE INDEX stage_player_validation_page ON stage_player(validation_state, source_player_id);
CREATE INDEX stage_issue_player ON stage_issue(stage_player_id, id);
"#;

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::{PlayerStagingMetadata, stage_recent_sqlite_player_profiles};

    fn metadata() -> PlayerStagingMetadata {
        PlayerStagingMetadata {
            protocol: "onebot11".to_string(),
            account_id: "10001".to_string(),
            namespace: "default".to_string(),
        }
    }

    fn create_source(path: &std::path::Path) -> Connection {
        let source = Connection::open(path).expect("应创建最近 SQLite 玩家源库");
        source
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
                CREATE TABLE player_skill(player_id INTEGER NOT NULL, secret_payload TEXT NOT NULL);
                "#,
            )
            .expect("应创建最近 SQLite 玩家表");
        source
    }

    #[test]
    fn staging_writes_normalized_profiles_and_validation_issues_without_source_writes() {
        let directory = tempdir().expect("应创建玩家 staging 临时目录");
        let source_path = directory.path().join("recent.sqlite");
        let source = create_source(&source_path);
        source
            .execute_batch(
                r#"
                INSERT INTO player VALUES(
                    1, 20001, '唐三', '旧昵称', '男', 10, 190, 100, 100, 50, 50,
                    10, 11, 12, 13, 14, 15, 1, 0
                );
                INSERT INTO player VALUES(
                    2, 20002, '越界角色', '旧昵称', '女', 121, 0, 100, 100, 50, 50,
                    10, 11, 12, 13, 14, 15, 1, 0
                );
                INSERT INTO player_skill VALUES(1, '不应被读取');
                "#,
            )
            .expect("应写入最近 SQLite 玩家测试资料");
        drop(source);
        let source_before = std::fs::read(&source_path).expect("应读取 staging 前源库字节");
        let output_path = directory.path().join("stage.sqlite");

        let summary = stage_recent_sqlite_player_profiles(&source_path, &output_path, &metadata())
            .expect("应生成基础角色资料 staging");

        assert_eq!(summary.total_players, 2);
        assert_eq!(summary.ready_players, 1);
        assert_eq!(summary.rejected_players, 1);
        assert_eq!(summary.issue_counts.get("number_out_of_range"), Some(&1));
        assert_eq!(
            std::fs::read(&source_path).expect("应读取 staging 后源库字节"),
            source_before,
            "只读 staging 不应改写源库"
        );

        let staged = Connection::open(&output_path).expect("应打开 staging 输出库");
        assert_eq!(
            staged
                .query_row(
                    "SELECT total_players FROM stage_batch WHERE id = 1",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("应读取 staging 批次摘要"),
            2
        );
        assert_eq!(
            staged
                .query_row(
                    "SELECT validation_state FROM stage_player WHERE source_player_id = 1",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .expect("应读取可确认 staging 角色"),
            "ready"
        );
        assert_eq!(
            staged
                .query_row(
                    "SELECT validation_state FROM stage_player WHERE source_player_id = 2",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .expect("应读取拒绝 staging 角色"),
            "rejected"
        );
        assert_eq!(
            staged
                .query_row("SELECT COUNT(*) FROM stage_issue", [], |row| row
                    .get::<_, i64>(0))
                .expect("应读取 staging 校验问题"),
            1
        );
        assert!(
            !staged
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'player_skill')",
                    [],
                    |row| row.get::<_, bool>(0),
                )
                .expect("应检查 staging 没有复制魂技表"),
            "staging 输出不能复制源魂技表"
        );
    }

    #[test]
    fn staging_rejects_incomplete_schema_without_creating_output() {
        let directory = tempdir().expect("应创建不完整 staging 临时目录");
        let source_path = directory.path().join("incomplete.sqlite");
        let source = Connection::open(&source_path).expect("应创建不完整源库");
        source
            .execute_batch("CREATE TABLE player(id INTEGER PRIMARY KEY, user_id INTEGER);")
            .expect("应创建不完整玩家表");
        drop(source);
        let output_path = directory.path().join("stage.sqlite");

        let error = stage_recent_sqlite_player_profiles(&source_path, &output_path, &metadata())
            .expect_err("缺少字段必须拒绝 staging");
        assert!(error.contains("name"), "缺字段错误应指出字段：{error}");
        assert!(!output_path.exists(), "源库结构无效时不应创建 staging 输出");
    }

    #[test]
    fn staging_retains_invalid_source_ids_as_rejected_records() {
        let directory = tempdir().expect("应创建非法源主键 staging 临时目录");
        let source_path = directory.path().join("recent.sqlite");
        let source = create_source(&source_path);
        source
            .execute_batch(
                r#"
                INSERT INTO player VALUES(
                    -7, 20001, '待修复角色', NULL, '男', 1, 0, 100, 100, 50, 50,
                    10, 10, 10, 10, 10, 10, 1, 0
                );
                "#,
            )
            .expect("应写入非法源主键资料");
        drop(source);
        let output_path = directory.path().join("stage.sqlite");

        let summary = stage_recent_sqlite_player_profiles(&source_path, &output_path, &metadata())
            .expect("非法源主键应进入拒绝 staging，而不是中断整批导入");
        assert_eq!(summary.total_players, 1);
        assert_eq!(summary.ready_players, 0);
        assert_eq!(summary.rejected_players, 1);
        assert_eq!(
            summary.issue_counts.get("source_player_id_invalid"),
            Some(&1)
        );
        let staged = Connection::open(&output_path).expect("应打开非法源主键 staging 输出");
        assert_eq!(
            staged
                .query_row("SELECT source_player_id FROM stage_player", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .expect("应保留非法源主键以供后续人工处理"),
            -7
        );
    }

    #[test]
    fn staging_retains_duplicate_subjects_without_overwrite() {
        let directory = tempdir().expect("应创建重复用户 staging 临时目录");
        let source_path = directory.path().join("recent.sqlite");
        let source = create_source(&source_path);
        source
            .execute_batch(
                r#"
                INSERT INTO player VALUES(
                    1, 20001, '重复角色甲', NULL, '男', 1, 0, 100, 100, 50, 50,
                    10, 10, 10, 10, 10, 10, 1, 0
                );
                INSERT INTO player VALUES(
                    2, 20001, '重复角色乙', NULL, '女', 1, 0, 100, 100, 50, 50,
                    10, 10, 10, 10, 10, 10, 1, 0
                );
                "#,
            )
            .expect("应写入重复用户资料");
        drop(source);
        let output_path = directory.path().join("stage.sqlite");

        let summary = stage_recent_sqlite_player_profiles(&source_path, &output_path, &metadata())
            .expect("重复用户应进入 staging 等待后续选择");
        assert_eq!(summary.total_players, 2);
        assert_eq!(summary.ready_players, 0);
        assert_eq!(summary.rejected_players, 2);
        assert_eq!(summary.issue_counts.get("duplicate_subject_id"), Some(&2));
        let staged = Connection::open(&output_path).expect("应打开重复用户 staging 输出");
        assert_eq!(
            staged
                .query_row("SELECT COUNT(*) FROM stage_player", [], |row| row
                    .get::<_, i64>(0))
                .expect("应统计重复用户 staging 记录"),
            2,
            "重复用户不得被 staging 静默覆盖"
        );
    }

    #[test]
    fn staging_never_overwrites_existing_output() {
        let directory = tempdir().expect("应创建 staging 输出冲突临时目录");
        let source_path = directory.path().join("recent.sqlite");
        let source = create_source(&source_path);
        source
            .execute_batch(
                r#"
                INSERT INTO player VALUES(
                    1, 20001, '唐三', NULL, '男', 1, 0, 100, 100, 50, 50,
                    10, 10, 10, 10, 10, 10, 1, 0
                );
                "#,
            )
            .expect("应写入可确认玩家资料");
        drop(source);
        let output_path = directory.path().join("stage.sqlite");
        stage_recent_sqlite_player_profiles(&source_path, &output_path, &metadata())
            .expect("应首次创建 staging 输出");
        let before = std::fs::read(&output_path).expect("应读取首次 staging 输出");

        assert!(
            stage_recent_sqlite_player_profiles(&source_path, &output_path, &metadata()).is_err(),
            "已有 staging 输出不能被覆盖"
        );
        assert_eq!(
            std::fs::read(&output_path).expect("应读取冲突后的 staging 输出"),
            before,
            "输出冲突不能修改原 staging 文件"
        );
    }
}
