use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::config::DatabaseConfig;
use crate::message::Protocol;

const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migration (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS identity (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    protocol TEXT NOT NULL,
    namespace TEXT NOT NULL,
    subject_kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(protocol, namespace, subject_kind, subject_id)
);

CREATE TABLE IF NOT EXISTS player (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    identity_id INTEGER NOT NULL UNIQUE REFERENCES identity(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    gender TEXT NOT NULL CHECK(gender IN ('男', '女')),
    level INTEGER NOT NULL DEFAULT 1 CHECK(level BETWEEN 1 AND 100),
    exp INTEGER NOT NULL DEFAULT 0 CHECK(exp >= 0),
    hp INTEGER NOT NULL DEFAULT 100 CHECK(hp >= 0),
    max_hp INTEGER NOT NULL DEFAULT 100 CHECK(max_hp > 0),
    soul_power INTEGER NOT NULL DEFAULT 50 CHECK(soul_power >= 0),
    max_soul_power INTEGER NOT NULL DEFAULT 50 CHECK(max_soul_power > 0),
    strength INTEGER NOT NULL DEFAULT 10 CHECK(strength >= 0),
    agility INTEGER NOT NULL DEFAULT 10 CHECK(agility >= 0),
    spirit INTEGER NOT NULL DEFAULT 10 CHECK(spirit >= 0),
    endurance INTEGER NOT NULL DEFAULT 10 CHECK(endurance >= 0),
    perception INTEGER NOT NULL DEFAULT 10 CHECK(perception >= 0),
    luck INTEGER NOT NULL DEFAULT 10 CHECK(luck >= 0),
    life_count INTEGER NOT NULL DEFAULT 1 CHECK(life_count BETWEEN 1 AND 3),
    state TEXT NOT NULL DEFAULT 'alive' CHECK(state IN ('alive', 'dead', 'reviving', 'deleted')),
    map_name TEXT NOT NULL DEFAULT '圣魂村',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS wuhun (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    category TEXT NOT NULL,
    form TEXT NOT NULL,
    description TEXT NOT NULL,
    weight INTEGER NOT NULL DEFAULT 1 CHECK(weight > 0)
);

CREATE TABLE IF NOT EXISTS player_wuhun (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    player_id INTEGER NOT NULL REFERENCES player(id) ON DELETE CASCADE,
    wuhun_id INTEGER NOT NULL REFERENCES wuhun(id),
    slot INTEGER NOT NULL CHECK(slot BETWEEN 1 AND 3),
    awaken_life INTEGER NOT NULL CHECK(awaken_life BETWEEN 1 AND 3),
    created_at INTEGER NOT NULL,
    UNIQUE(player_id, slot)
);

INSERT OR IGNORE INTO wuhun(name, category, form, description, weight) VALUES
    ('独狼', '敏攻系', '附体型', '兽武魂，擅长速度、闪避与暴击。', 5),
    ('萝卜', '食物系', '召唤型', '器武魂，擅长恢复与辅助。', 5),
    ('镰刀', '敏攻系', '手持型', '器武魂，擅长高速近战攻击。', 5);
"#;

const MIGRATION_V2: &str = r#"
CREATE TABLE identity_v2 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    protocol TEXT NOT NULL,
    account_id TEXT CHECK(
        account_id IS NULL OR (
            length(account_id) BETWEEN 1 AND 128 AND account_id = trim(account_id)
        )
    ),
    namespace TEXT NOT NULL,
    subject_kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

INSERT INTO identity_v2(
    id, protocol, account_id, namespace, subject_kind, subject_id, created_at
)
SELECT id, protocol, NULL, namespace, subject_kind, subject_id, created_at
  FROM identity;

DROP TABLE identity;
ALTER TABLE identity_v2 RENAME TO identity;

CREATE UNIQUE INDEX identity_known_unique
    ON identity(protocol, account_id, namespace, subject_kind, subject_id)
 WHERE account_id IS NOT NULL;
CREATE UNIQUE INDEX identity_legacy_unique
    ON identity(protocol, namespace, subject_kind, subject_id)
 WHERE account_id IS NULL;

CREATE TABLE identity_claim_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    identity_id INTEGER NOT NULL UNIQUE REFERENCES identity(id) ON DELETE RESTRICT,
    protocol TEXT NOT NULL,
    namespace TEXT NOT NULL,
    subject_kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    claimed_account_id TEXT NOT NULL,
    actor_account_id TEXT NOT NULL,
    actor_subject_id TEXT NOT NULL,
    source_message_id TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(protocol, namespace, subject_kind, subject_id)
);

CREATE TRIGGER identity_claim_audit_no_update
BEFORE UPDATE ON identity_claim_audit
BEGIN
    SELECT RAISE(ABORT, 'identity claim audit is immutable');
END;

CREATE TRIGGER identity_claim_audit_no_delete
BEFORE DELETE ON identity_claim_audit
BEGIN
    SELECT RAISE(ABORT, 'identity claim audit is immutable');
END;

CREATE TRIGGER identity_claim_audit_no_reinsert
BEFORE INSERT ON identity_claim_audit
WHEN EXISTS(
    SELECT 1 FROM identity_claim_audit
     WHERE identity_id = NEW.identity_id
        OR (protocol = NEW.protocol AND namespace = NEW.namespace
            AND subject_kind = NEW.subject_kind AND subject_id = NEW.subject_id)
)
BEGIN
    SELECT RAISE(ABORT, 'identity claim audit is immutable');
END;
"#;

const MIGRATION_V3: &str = r#"
CREATE TABLE operation_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    protocol TEXT NOT NULL CHECK(protocol IN ('onebot11', 'qq-official')),
    account_id TEXT NOT NULL CHECK(
        length(account_id) BETWEEN 1 AND 128 AND account_id = trim(account_id)
    ),
    namespace TEXT NOT NULL CHECK(length(namespace) BETWEEN 1 AND 64),
    subject_kind TEXT NOT NULL CHECK(length(subject_kind) BETWEEN 1 AND 64),
    subject_id TEXT NOT NULL CHECK(length(subject_id) BETWEEN 1 AND 256),
    command TEXT NOT NULL CHECK(length(command) BETWEEN 1 AND 128),
    outcome TEXT NOT NULL CHECK(outcome IN ('ok', 'error', 'denied')),
    source_message_id TEXT NOT NULL CHECK(length(source_message_id) <= 256),
    details_json TEXT NOT NULL CHECK(
        length(CAST(details_json AS BLOB)) <= 8192
        AND json_valid(details_json)
        AND instr(lower(details_json), 'raw_event_json') = 0
        AND instr(lower(details_json), 'qimen_raw_event') = 0
        AND instr(lower(details_json), 'raw_json') = 0
        AND instr(lower(details_json), 'base64://') = 0
        AND instr(lower(details_json), 'data:image/') = 0
    ),
    created_at INTEGER NOT NULL CHECK(created_at >= 0)
);

CREATE INDEX operation_log_identity_page
    ON operation_log(protocol, account_id, namespace, subject_kind, subject_id, id);

CREATE TRIGGER operation_log_no_update
BEFORE UPDATE ON operation_log
BEGIN
    SELECT RAISE(ABORT, 'operation log is append-only');
END;

CREATE TRIGGER operation_log_no_delete
BEFORE DELETE ON operation_log
BEGIN
    SELECT RAISE(ABORT, 'operation log is append-only');
END;

CREATE TRIGGER operation_log_no_reinsert
BEFORE INSERT ON operation_log
WHEN EXISTS(SELECT 1 FROM operation_log WHERE id = NEW.id)
BEGIN
    SELECT RAISE(ABORT, 'operation log is append-only');
END;

CREATE TRIGGER operation_log_safe_details
BEFORE INSERT ON operation_log
WHEN json_type(NEW.details_json) != 'object'
OR EXISTS(
    SELECT 1 FROM json_each(NEW.details_json)
     WHERE key NOT IN ('context', 'has_args', 'reason', 'duration_ms')
)
BEGIN
    SELECT RAISE(ABORT, 'operation log details field is not allowed');
END;
"#;

const MIGRATION_V4: &str = r#"
CREATE TABLE authorized_context (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    protocol TEXT NOT NULL CHECK(protocol IN ('onebot11', 'qq-official')),
    account_id TEXT NOT NULL CHECK(
        length(account_id) BETWEEN 1 AND 128 AND account_id = trim(account_id)
    ),
    namespace TEXT NOT NULL CHECK(length(namespace) BETWEEN 1 AND 64),
    context_kind TEXT NOT NULL CHECK(context_kind IN ('group', 'channel')),
    context_id TEXT NOT NULL CHECK(length(context_id) BETWEEN 1 AND 256),
    label TEXT NOT NULL CHECK(length(label) <= 80),
    granted_by_subject_id TEXT NOT NULL CHECK(length(granted_by_subject_id) BETWEEN 1 AND 256),
    created_at INTEGER NOT NULL CHECK(created_at >= 0),
    UNIQUE(protocol, account_id, namespace, context_kind, context_id)
);

CREATE INDEX authorized_context_bot_page
    ON authorized_context(protocol, account_id, namespace, id);

DROP TRIGGER operation_log_safe_details;
CREATE TRIGGER operation_log_safe_details
BEFORE INSERT ON operation_log
WHEN json_type(NEW.details_json) != 'object'
OR EXISTS(
    SELECT 1 FROM json_each(NEW.details_json)
     WHERE key NOT IN (
         'context', 'has_args', 'reason', 'duration_ms', 'target_kind', 'target_id'
     )
)
OR (
    json_type(NEW.details_json, '$.target_kind') IS NOT NULL
    AND (
        json_type(NEW.details_json, '$.target_kind') != 'text'
        OR json_extract(NEW.details_json, '$.target_kind') NOT IN ('group', 'channel')
    )
)
OR (
    json_type(NEW.details_json, '$.target_id') IS NOT NULL
    AND (
        json_type(NEW.details_json, '$.target_id') != 'text'
        OR length(json_extract(NEW.details_json, '$.target_id')) NOT BETWEEN 1 AND 256
        OR instr(json_extract(NEW.details_json, '$.target_id'), char(0)) > 0
        OR json_extract(NEW.details_json, '$.target_id')
           GLOB ('*[' || char(1) || '-' || char(31) || char(127) || ']*')
    )
)
BEGIN
    SELECT RAISE(ABORT, 'operation log details field is not allowed');
END;
"#;

const LEGACY_CLAIM_REQUIRED: &str =
    "检测到尚未绑定机器人账号的旧存档，请联系机器人所有者在私聊中完成旧档认领";

#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
    busy_timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityKey<'a> {
    pub protocol: Protocol,
    pub account_id: &'a str,
    pub namespace: &'a str,
    pub subject_kind: &'a str,
    pub subject_id: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyIdentityState {
    Legacy,
    ClaimedToCurrent,
    ClaimedToOther,
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyClaimResult {
    Claimed { identity_id: i64 },
    AlreadyClaimed { identity_id: i64 },
    NotFound,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyClaimActor<'a> {
    pub account_id: &'a str,
    pub subject_id: &'a str,
    pub message_id: &'a str,
    pub reason: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerStatus {
    pub name: String,
    pub gender: String,
    pub level: i64,
    pub exp: i64,
    pub hp: i64,
    pub max_hp: i64,
    pub soul_power: i64,
    pub max_soul_power: i64,
    pub map_name: String,
    pub life_count: i64,
    pub state: String,
    pub wuhun_name: Option<String>,
    pub wuhun_category: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AwakenedWuhun {
    pub name: String,
    pub category: String,
    pub form: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // 为后续只读管理查询保留完整审计字段。
pub struct OperationLogEntry {
    pub id: i64,
    pub protocol: Protocol,
    pub account_id: String,
    pub namespace: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub command: String,
    pub outcome: String,
    pub source_message_id: String,
    pub details_json: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // 为后续只读管理查询保留分页结构。
pub struct OperationLogPage {
    pub entries: Vec<OperationLogEntry>,
    pub next_after_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperationLogInput<'a> {
    pub command: &'a str,
    pub outcome: &'a str,
    pub source_message_id: &'a str,
    pub details_json: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // 聊天列表只展示部分字段，存储层仍返回完整归属与审计信息。
pub struct AuthorizedContextEntry {
    pub id: i64,
    pub protocol: Protocol,
    pub account_id: String,
    pub namespace: String,
    pub context_kind: String,
    pub context_id: String,
    pub label: String,
    pub granted_by_subject_id: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedContextPage {
    pub entries: Vec<AuthorizedContextEntry>,
    pub next_after_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorizedContextChange {
    Granted { id: i64 },
    AlreadyGranted { id: i64 },
    Revoked { id: i64 },
    AlreadyRevoked,
}

impl Store {
    pub fn initialize(data_dir: &Path, config: &DatabaseConfig) -> Result<Self, String> {
        let path = data_dir.join(&config.relative_path);
        let parent = path
            .parent()
            .ok_or_else(|| "无法确定数据库目录".to_string())?;
        fs::create_dir_all(parent).map_err(|error| format!("创建数据库目录失败：{error}"))?;
        let store = Self {
            path,
            busy_timeout: Duration::from_millis(config.busy_timeout_ms),
        };
        let mut connection = store.open()?;
        store.migrate(&mut connection)?;
        Ok(store)
    }

    fn open(&self) -> Result<Connection, String> {
        let connection = Connection::open(&self.path)
            .map_err(|error| format!("打开 SQLite 数据库失败：{error}"))?;
        connection
            .busy_timeout(self.busy_timeout)
            .map_err(|error| format!("设置 SQLite busy timeout 失败：{error}"))?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
            .map_err(|error| format!("初始化 SQLite PRAGMA 失败：{error}"))?;
        verify_foreign_keys(&connection, true)?;
        Ok(connection)
    }

    fn migrate(&self, connection: &mut Connection) -> Result<(), String> {
        {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("开始数据库迁移 v1 失败：{error}"))?;
            transaction
                .execute_batch(MIGRATION_V1)
                .map_err(|error| format!("执行数据库迁移 v1 失败：{error}"))?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO schema_migration(version, applied_at) VALUES(1, ?1)",
                    [now_timestamp()?],
                )
                .map_err(|error| format!("记录数据库迁移 v1 失败：{error}"))?;
            transaction
                .commit()
                .map_err(|error| format!("提交数据库迁移 v1 失败：{error}"))?;
        }

        set_foreign_keys(connection, false)?;
        let migration_result = (|| -> Result<(), String> {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("开始数据库迁移 v2/v3/v4 失败：{error}"))?;
            // 所有版本检查都在同一写锁内，后启动者只会看到已提交版本并执行校验。
            if !migration_applied(&transaction, 2)? {
                let old_identity_sequence = transaction
                    .query_row(
                        "SELECT seq FROM sqlite_sequence WHERE name = 'identity'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
                    .map_err(|error| format!("读取旧身份序列失败：{error}"))?;
                transaction
                    .execute_batch(MIGRATION_V2)
                    .map_err(|error| format!("执行数据库迁移 v2 失败：{error}"))?;
                if let Some(sequence) = old_identity_sequence {
                    restore_identity_sequence(&transaction, sequence)?;
                }
                validate_v2_schema(&transaction)?;
                transaction
                    .execute(
                        "INSERT INTO schema_migration(version, applied_at) VALUES(2, ?1)",
                        [now_timestamp()?],
                    )
                    .map_err(|error| format!("记录数据库迁移 v2 失败：{error}"))?;
            } else {
                validate_v2_schema(&transaction)?;
            }

            if !migration_applied(&transaction, 3)? {
                transaction
                    .execute_batch(MIGRATION_V3)
                    .map_err(|error| format!("执行数据库迁移 v3 失败：{error}"))?;
                validate_v3_schema(&transaction)?;
                transaction
                    .execute(
                        "INSERT INTO schema_migration(version, applied_at) VALUES(3, ?1)",
                        [now_timestamp()?],
                    )
                    .map_err(|error| format!("记录数据库迁移 v3 失败：{error}"))?;
            } else {
                validate_v3_schema(&transaction)?;
            }

            if !migration_applied(&transaction, 4)? {
                transaction
                    .execute_batch(MIGRATION_V4)
                    .map_err(|error| format!("执行数据库迁移 v4 失败：{error}"))?;
                validate_v4_schema(&transaction)?;
                transaction
                    .execute(
                        "INSERT INTO schema_migration(version, applied_at) VALUES(4, ?1)",
                        [now_timestamp()?],
                    )
                    .map_err(|error| format!("记录数据库迁移 v4 失败：{error}"))?;
            } else {
                validate_v4_schema(&transaction)?;
            }

            ensure_no_foreign_key_violations(&transaction)?;
            transaction
                .commit()
                .map_err(|error| format!("提交数据库迁移 v2/v3/v4 失败：{error}"))?;
            Ok(())
        })();
        let restore_result = set_foreign_keys(connection, true);

        match (migration_result, restore_result) {
            (Ok(()), Ok(())) => {
                validate_v2_schema(connection)?;
                validate_v3_schema(connection)?;
                validate_v4_schema(connection)?;
                Ok(())
            }
            (Err(migration_error), Ok(())) => Err(migration_error),
            (Ok(()), Err(restore_error)) => Err(restore_error),
            (Err(migration_error), Err(restore_error)) => Err(format!(
                "{migration_error}；同时恢复 SQLite 外键约束失败：{restore_error}"
            )),
        }
    }

    #[allow(dead_code)]
    pub fn register_player(
        &self,
        key: &IdentityKey<'_>,
        name: &str,
        gender: &str,
    ) -> Result<PlayerStatus, String> {
        self.register_player_with_operation(key, name, gender, None)
    }

    pub fn register_player_with_operation(
        &self,
        key: &IdentityKey<'_>,
        name: &str,
        gender: &str,
        operation: Option<&OperationLogInput<'_>>,
    ) -> Result<PlayerStatus, String> {
        validate_identity_key(key)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始注册事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        let identity_id = ensure_identity(&transaction, key)?;
        let exists = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM player WHERE identity_id = ?1)",
                [identity_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("检查角色失败：{error}"))?;
        if exists {
            return Err("你已经穿越到斗罗大陆，无需重复创建角色".to_string());
        }
        let timestamp = now_timestamp()?;
        transaction
            .execute(
                "INSERT INTO player(identity_id, name, gender, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?4)",
                params![identity_id, name, gender, timestamp],
            )
            .map_err(|error| format!("创建角色失败：{error}"))?;
        if let Some(operation) = operation {
            insert_operation_log(&transaction, key, operation)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("提交注册事务失败：{error}"))?;
        self.player_status(key)?
            .ok_or_else(|| "角色已经创建，但重新读取失败".to_string())
    }

    pub fn player_status(&self, key: &IdentityKey<'_>) -> Result<Option<PlayerStatus>, String> {
        validate_identity_key(key)?;
        let connection = self.open()?;
        ensure_no_legacy_identity(&connection, key)?;
        connection
            .query_row(
                r#"
                SELECT p.name, p.gender, p.level, p.exp, p.hp, p.max_hp,
                       p.soul_power, p.max_soul_power, p.map_name, p.life_count, p.state,
                       w.name, w.category
                  FROM identity i
                  JOIN player p ON p.identity_id = i.id
             LEFT JOIN player_wuhun pw ON pw.player_id = p.id AND pw.slot = 1
             LEFT JOIN wuhun w ON w.id = pw.wuhun_id
                 WHERE i.protocol = ?1 AND i.account_id = ?2 AND i.namespace = ?3
                   AND i.subject_kind = ?4 AND i.subject_id = ?5
                "#,
                params![
                    key.protocol.as_str(),
                    key.account_id,
                    key.namespace,
                    key.subject_kind,
                    key.subject_id
                ],
                |row| {
                    Ok(PlayerStatus {
                        name: row.get(0)?,
                        gender: row.get(1)?,
                        level: row.get(2)?,
                        exp: row.get(3)?,
                        hp: row.get(4)?,
                        max_hp: row.get(5)?,
                        soul_power: row.get(6)?,
                        max_soul_power: row.get(7)?,
                        map_name: row.get(8)?,
                        life_count: row.get(9)?,
                        state: row.get(10)?,
                        wuhun_name: row.get(11)?,
                        wuhun_category: row.get(12)?,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("查询角色状态失败：{error}"))
    }

    #[allow(dead_code)]
    pub fn awaken_wuhun(&self, key: &IdentityKey<'_>) -> Result<AwakenedWuhun, String> {
        self.awaken_wuhun_with_operation(key, None)
    }

    pub fn awaken_wuhun_with_operation(
        &self,
        key: &IdentityKey<'_>,
        operation: Option<&OperationLogInput<'_>>,
    ) -> Result<AwakenedWuhun, String> {
        validate_identity_key(key)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始武魂觉醒事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        let player = transaction
            .query_row(
                r#"
                SELECT p.id, p.life_count
                  FROM identity i
                  JOIN player p ON p.identity_id = i.id
                 WHERE i.protocol = ?1 AND i.account_id = ?2 AND i.namespace = ?3
                   AND i.subject_kind = ?4 AND i.subject_id = ?5
                "#,
                params![
                    key.protocol.as_str(),
                    key.account_id,
                    key.namespace,
                    key.subject_kind,
                    key.subject_id
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(|error| format!("查询觉醒角色失败：{error}"))?
            .ok_or_else(|| "你还没有角色，请先使用“开始穿越 角色名 性别”".to_string())?;
        let already_awakened = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM player_wuhun WHERE player_id = ?1 AND slot = 1)",
                [player.0],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("检查武魂状态失败：{error}"))?;
        if already_awakened {
            return Err("你的第一武魂已经觉醒".to_string());
        }
        let wuhun = transaction
            .query_row(
                "SELECT id, name, category, form, description FROM wuhun ORDER BY RANDOM() LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        AwakenedWuhun {
                            name: row.get(1)?,
                            category: row.get(2)?,
                            form: row.get(3)?,
                            description: row.get(4)?,
                        },
                    ))
                },
            )
            .map_err(|error| format!("选择武魂失败：{error}"))?;
        transaction
            .execute(
                "INSERT INTO player_wuhun(player_id, wuhun_id, slot, awaken_life, created_at) VALUES(?1, ?2, 1, ?3, ?4)",
                params![player.0, wuhun.0, player.1, now_timestamp()?],
            )
            .map_err(|error| format!("保存觉醒武魂失败：{error}"))?;
        if let Some(operation) = operation {
            insert_operation_log(&transaction, key, operation)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("提交武魂觉醒事务失败：{error}"))?;
        Ok(wuhun.1)
    }

    pub fn inspect_legacy_identity(
        &self,
        key: &IdentityKey<'_>,
    ) -> Result<LegacyIdentityState, String> {
        validate_identity_key(key)?;
        let connection = self.open()?;
        let legacy_exists = matching_legacy_identity_id(&connection, key)?.is_some();
        if legacy_exists {
            return Ok(LegacyIdentityState::Legacy);
        }
        let claimed = connection
            .query_row(
                r#"
                SELECT identity_id, claimed_account_id
                  FROM identity_claim_audit
                 WHERE protocol = ?1 AND namespace = ?2
                   AND subject_kind = ?3 AND subject_id = ?4
                "#,
                params![
                    key.protocol.as_str(),
                    key.namespace,
                    key.subject_kind,
                    key.subject_id
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| format!("读取旧档认领状态失败：{error}"))?;
        Ok(match claimed {
            Some((_, account_id)) if account_id == key.account_id => {
                LegacyIdentityState::ClaimedToCurrent
            }
            Some(_) => LegacyIdentityState::ClaimedToOther,
            None => LegacyIdentityState::Missing,
        })
    }

    #[allow(dead_code)]
    pub fn claim_legacy_identity(
        &self,
        key: &IdentityKey<'_>,
        actor: &LegacyClaimActor<'_>,
    ) -> Result<LegacyClaimResult, String> {
        self.claim_legacy_identity_with_operation(key, actor, None)
    }

    pub fn claim_legacy_identity_with_operation(
        &self,
        key: &IdentityKey<'_>,
        actor: &LegacyClaimActor<'_>,
        operation: Option<&OperationLogInput<'_>>,
    ) -> Result<LegacyClaimResult, String> {
        validate_identity_key(key)?;
        validate_claim_actor(actor)?;
        if actor.account_id != key.account_id {
            return Err("确认的 account_id 与当前机器人账号不一致".to_string());
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始旧档认领事务失败：{error}"))?;
        let Some(identity_id) = matching_legacy_identity_id(&transaction, key)? else {
            let prior_claim = transaction
                .query_row(
                    r#"
                    SELECT identity_id, claimed_account_id
                      FROM identity_claim_audit
                     WHERE protocol = ?1 AND namespace = ?2
                       AND subject_kind = ?3 AND subject_id = ?4
                    "#,
                    params![
                        key.protocol.as_str(),
                        key.namespace,
                        key.subject_kind,
                        key.subject_id
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|error| format!("读取已有旧档认领记录失败：{error}"))?;
            return Ok(match prior_claim {
                Some((identity_id, account_id)) if account_id == key.account_id => {
                    LegacyClaimResult::AlreadyClaimed { identity_id }
                }
                Some(_) => LegacyClaimResult::Conflict,
                None => LegacyClaimResult::NotFound,
            });
        };

        let known_exists = transaction
            .query_row(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM identity
                     WHERE protocol = ?1 AND account_id = ?2 AND namespace = ?3
                       AND subject_kind = ?4 AND subject_id = ?5
                )
                "#,
                params![
                    key.protocol.as_str(),
                    key.account_id,
                    key.namespace,
                    key.subject_kind,
                    key.subject_id
                ],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("检查旧档认领冲突失败：{error}"))?;
        if known_exists {
            return Ok(LegacyClaimResult::Conflict);
        }

        let changed = transaction
            .execute(
                "UPDATE identity SET account_id = ?1 WHERE id = ?2 AND account_id IS NULL",
                params![key.account_id, identity_id],
            )
            .map_err(|error| format!("绑定旧档机器人账号失败：{error}"))?;
        if changed != 1 {
            return Ok(LegacyClaimResult::Conflict);
        }
        transaction
            .execute(
                r#"
                INSERT INTO identity_claim_audit(
                    identity_id, protocol, namespace, subject_kind, subject_id,
                    claimed_account_id, actor_account_id, actor_subject_id,
                    source_message_id, reason, created_at
                ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                "#,
                params![
                    identity_id,
                    key.protocol.as_str(),
                    key.namespace,
                    key.subject_kind,
                    key.subject_id,
                    key.account_id,
                    actor.account_id,
                    actor.subject_id,
                    actor.message_id,
                    actor.reason,
                    now_timestamp()?
                ],
            )
            .map_err(|error| format!("写入旧档认领审计失败：{error}"))?;
        if let Some(operation) = operation {
            insert_operation_log(&transaction, key, operation)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("提交旧档认领事务失败：{error}"))?;
        Ok(LegacyClaimResult::Claimed { identity_id })
    }

    #[allow(dead_code)] // Foundation API; chat commands are wired in a later slice.
    pub fn append_operation(
        &self,
        key: &IdentityKey<'_>,
        command: &str,
        outcome: &str,
        source_message_id: &str,
        details_json: &str,
    ) -> Result<i64, String> {
        validate_identity_key(key)?;
        let operation = OperationLogInput {
            command,
            outcome,
            source_message_id,
            details_json,
        };
        validate_operation_input(&operation)?;
        let connection = self.open()?;
        insert_operation_log(&connection, key, &operation)
    }

    #[allow(dead_code)] // Foundation API; chat commands are wired in a later slice.
    pub fn list_operation_logs(
        &self,
        key: &IdentityKey<'_>,
        after_id: Option<i64>,
        limit: usize,
    ) -> Result<OperationLogPage, String> {
        validate_identity_key(key)?;
        if !(1..=100).contains(&limit) {
            return Err("操作日志分页数量必须在 1 到 100 之间".to_string());
        }
        let after_id = after_id.unwrap_or(0);
        if after_id < 0 {
            return Err("操作日志分页游标不能为负数".to_string());
        }
        let fetch_limit =
            i64::try_from(limit + 1).map_err(|_| "操作日志分页数量无法转换".to_string())?;
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                r#"
                SELECT id, account_id, namespace, subject_kind, subject_id,
                       command, outcome, source_message_id, details_json, created_at
                  FROM operation_log
                 WHERE protocol = ?1 AND account_id = ?2 AND namespace = ?3
                   AND subject_kind = ?4 AND subject_id = ?5 AND id > ?6
                 ORDER BY id ASC
                 LIMIT ?7
                "#,
            )
            .map_err(|error| format!("准备操作日志分页查询失败：{error}"))?;
        let mut entries = statement
            .query_map(
                params![
                    key.protocol.as_str(),
                    key.account_id,
                    key.namespace,
                    key.subject_kind,
                    key.subject_id,
                    after_id,
                    fetch_limit
                ],
                |row| {
                    Ok(OperationLogEntry {
                        id: row.get(0)?,
                        protocol: key.protocol,
                        account_id: row.get(1)?,
                        namespace: row.get(2)?,
                        subject_kind: row.get(3)?,
                        subject_id: row.get(4)?,
                        command: row.get(5)?,
                        outcome: row.get(6)?,
                        source_message_id: row.get(7)?,
                        details_json: row.get(8)?,
                        created_at: row.get(9)?,
                    })
                },
            )
            .map_err(|error| format!("查询操作日志失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("解析操作日志失败：{error}"))?;
        let has_more = entries.len() > limit;
        entries.truncate(limit);
        let next_after_id = has_more
            .then(|| entries.last().map(|entry| entry.id))
            .flatten();
        Ok(OperationLogPage {
            entries,
            next_after_id,
        })
    }

    pub fn grant_authorized_context(
        &self,
        key: &IdentityKey<'_>,
        context_kind: &str,
        context_id: &str,
        label: &str,
        operation: &OperationLogInput<'_>,
    ) -> Result<AuthorizedContextChange, String> {
        validate_identity_key(key)?;
        validate_context_fields(context_kind, context_id, label, key.subject_id)?;
        validate_context_operation(operation, context_kind, context_id)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始授权上下文事务失败：{error}"))?;
        let changed = transaction
            .execute(
                r#"
                INSERT OR IGNORE INTO authorized_context(
                    protocol, account_id, namespace, context_kind, context_id,
                    label, granted_by_subject_id, created_at
                ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    key.protocol.as_str(),
                    key.account_id,
                    key.namespace,
                    context_kind,
                    context_id,
                    label,
                    key.subject_id,
                    now_timestamp()?
                ],
            )
            .map_err(|error| format!("写入授权上下文失败：{error}"))?;
        let id = transaction
            .query_row(
                r#"
                SELECT id FROM authorized_context
                 WHERE protocol = ?1 AND account_id = ?2 AND namespace = ?3
                   AND context_kind = ?4 AND context_id = ?5
                "#,
                params![
                    key.protocol.as_str(),
                    key.account_id,
                    key.namespace,
                    context_kind,
                    context_id
                ],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("读取授权上下文失败：{error}"))?;
        let result = if changed == 1 {
            AuthorizedContextChange::Granted { id }
        } else {
            AuthorizedContextChange::AlreadyGranted { id }
        };
        insert_operation_log(&transaction, key, operation)?;
        transaction
            .commit()
            .map_err(|error| format!("提交授权上下文事务失败：{error}"))?;
        Ok(result)
    }

    pub fn revoke_authorized_context(
        &self,
        key: &IdentityKey<'_>,
        context_kind: &str,
        context_id: &str,
        operation: &OperationLogInput<'_>,
    ) -> Result<AuthorizedContextChange, String> {
        validate_identity_key(key)?;
        validate_context_fields(context_kind, context_id, "", key.subject_id)?;
        validate_context_operation(operation, context_kind, context_id)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始撤销授权上下文事务失败：{error}"))?;
        let id = transaction
            .query_row(
                r#"
                SELECT id FROM authorized_context
                 WHERE protocol = ?1 AND account_id = ?2 AND namespace = ?3
                   AND context_kind = ?4 AND context_id = ?5
                "#,
                params![
                    key.protocol.as_str(),
                    key.account_id,
                    key.namespace,
                    context_kind,
                    context_id
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("查询授权上下文失败：{error}"))?;
        let result = match id {
            Some(id) => {
                let changed = transaction
                    .execute("DELETE FROM authorized_context WHERE id = ?1", [id])
                    .map_err(|error| format!("撤销授权上下文失败：{error}"))?;
                if changed != 1 {
                    return Err("撤销授权上下文时记录状态发生变化".to_string());
                }
                AuthorizedContextChange::Revoked { id }
            }
            None => AuthorizedContextChange::AlreadyRevoked,
        };
        insert_operation_log(&transaction, key, operation)?;
        transaction
            .commit()
            .map_err(|error| format!("提交撤销授权上下文事务失败：{error}"))?;
        Ok(result)
    }

    pub fn list_authorized_contexts(
        &self,
        key: &IdentityKey<'_>,
        after_id: Option<i64>,
        limit: usize,
    ) -> Result<AuthorizedContextPage, String> {
        validate_identity_key(key)?;
        validate_context_page(after_id, limit)?;
        let after_id = after_id.unwrap_or(0);
        let fetch_limit =
            i64::try_from(limit + 1).map_err(|_| "授权上下文分页数量无法转换".to_string())?;
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                r#"
                SELECT id, protocol, account_id, namespace, context_kind, context_id,
                       label, granted_by_subject_id, created_at
                  FROM authorized_context
                 WHERE protocol = ?1 AND account_id = ?2 AND namespace = ?3 AND id > ?4
                 ORDER BY id ASC
                 LIMIT ?5
                "#,
            )
            .map_err(|error| format!("准备授权上下文分页查询失败：{error}"))?;
        let mut entries = statement
            .query_map(
                params![
                    key.protocol.as_str(),
                    key.account_id,
                    key.namespace,
                    after_id,
                    fetch_limit
                ],
                |row| {
                    Ok(AuthorizedContextEntry {
                        id: row.get(0)?,
                        protocol: match row.get::<_, String>(1)?.as_str() {
                            "onebot11" => Protocol::OneBot11,
                            "qq-official" => Protocol::QqOfficial,
                            _ => return Err(rusqlite::Error::InvalidQuery),
                        },
                        account_id: row.get(2)?,
                        namespace: row.get(3)?,
                        context_kind: row.get(4)?,
                        context_id: row.get(5)?,
                        label: row.get(6)?,
                        granted_by_subject_id: row.get(7)?,
                        created_at: row.get(8)?,
                    })
                },
            )
            .map_err(|error| format!("查询授权上下文失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("解析授权上下文失败：{error}"))?;
        let has_more = entries.len() > limit;
        entries.truncate(limit);
        let next_after_id = has_more
            .then(|| entries.last().map(|entry| entry.id))
            .flatten();
        Ok(AuthorizedContextPage {
            entries,
            next_after_id,
        })
    }

    pub fn is_authorized(
        &self,
        key: &IdentityKey<'_>,
        context_kind: &str,
        context_id: &str,
    ) -> Result<bool, String> {
        validate_identity_key(key)?;
        validate_context_fields(context_kind, context_id, "", key.subject_id)?;
        let connection = self.open()?;
        connection
            .query_row(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM authorized_context
                     WHERE protocol = ?1 AND account_id = ?2 AND namespace = ?3
                       AND context_kind = ?4 AND context_id = ?5
                )
                "#,
                params![
                    key.protocol.as_str(),
                    key.account_id,
                    key.namespace,
                    context_kind,
                    context_id
                ],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("查询授权上下文状态失败：{error}"))
    }
}

fn migration_applied(connection: &Connection, version: i64) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migration WHERE version = ?1)",
            [version],
            |row| row.get(0),
        )
        .map_err(|error| format!("读取数据库迁移版本失败：{error}"))
}

fn restore_identity_sequence(transaction: &Transaction<'_>, sequence: i64) -> Result<(), String> {
    let current = transaction
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'identity'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| format!("读取迁移后身份序列失败：{error}"))?;
    let sequence = current.map_or(sequence, |current| current.max(sequence));
    let changed = transaction
        .execute(
            "UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'identity'",
            [sequence],
        )
        .map_err(|error| format!("恢复身份序列失败：{error}"))?;
    if changed == 0 {
        transaction
            .execute(
                "INSERT INTO sqlite_sequence(name, seq) VALUES('identity', ?1)",
                [sequence],
            )
            .map_err(|error| format!("写入身份序列失败：{error}"))?;
    }
    transaction
        .execute("DELETE FROM sqlite_sequence WHERE name = 'identity_v2'", [])
        .map_err(|error| format!("清理临时身份序列失败：{error}"))?;
    Ok(())
}

fn set_foreign_keys(connection: &Connection, enabled: bool) -> Result<(), String> {
    let statement = if enabled {
        "PRAGMA foreign_keys = ON;"
    } else {
        "PRAGMA foreign_keys = OFF;"
    };
    connection
        .execute_batch(statement)
        .map_err(|error| format!("切换 SQLite 外键约束失败：{error}"))?;
    verify_foreign_keys(connection, enabled)
}

fn verify_foreign_keys(connection: &Connection, expected: bool) -> Result<(), String> {
    let actual = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, bool>(0))
        .map_err(|error| format!("读取 SQLite 外键状态失败：{error}"))?;
    if actual != expected {
        return Err(format!(
            "SQLite 外键状态异常：期望 {}，实际 {}",
            u8::from(expected),
            u8::from(actual)
        ));
    }
    Ok(())
}

fn ensure_no_foreign_key_violations(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(|error| format!("准备 SQLite 外键检查失败：{error}"))?;
    let violation = statement
        .query_row([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .optional()
        .map_err(|error| format!("执行 SQLite 外键检查失败：{error}"))?;
    if let Some((table, row_id, parent, foreign_key_id)) = violation {
        return Err(format!(
            "数据库迁移产生外键损坏：table={table}, rowid={row_id:?}, parent={parent}, fk={foreign_key_id}"
        ));
    }
    Ok(())
}

fn validate_v2_schema(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare("PRAGMA table_info(identity)")
        .map_err(|error| format!("读取 identity 表结构失败：{error}"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("查询 identity 表字段失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 identity 表字段失败：{error}"))?;
    let expected = [
        "id",
        "protocol",
        "account_id",
        "namespace",
        "subject_kind",
        "subject_id",
        "created_at",
    ];
    if columns != expected {
        return Err(format!(
            "数据库已标记迁移 v2，但 identity 表结构不匹配：{columns:?}"
        ));
    }

    for (name, expected_columns, predicate) in [
        (
            "identity_known_unique",
            [
                "protocol",
                "account_id",
                "namespace",
                "subject_kind",
                "subject_id",
            ],
            "WHERE account_id IS NOT NULL",
        ),
        (
            "identity_legacy_unique",
            ["protocol", "namespace", "subject_kind", "subject_id", ""],
            "WHERE account_id IS NULL",
        ),
    ] {
        let sql = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [name],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("读取索引 {name} 失败：{error}"))?
            .ok_or_else(|| format!("数据库已标记迁移 v2，但缺少索引 {name}"))?;
        if !sql.contains(predicate) {
            return Err(format!("索引 {name} 缺少预期条件：{predicate}"));
        }
        let escaped_name = name.replace('"', "\"\"");
        let mut info = connection
            .prepare(&format!("PRAGMA index_info(\"{escaped_name}\")"))
            .map_err(|error| format!("读取索引 {name} 字段失败：{error}"))?;
        let actual_columns = info
            .query_map([], |row| row.get::<_, String>(2))
            .map_err(|error| format!("查询索引 {name} 字段失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("解析索引 {name} 字段失败：{error}"))?;
        let expected_columns = expected_columns
            .iter()
            .filter(|column| !column.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if actual_columns != expected_columns {
            return Err(format!(
                "索引 {name} 字段不匹配：期望 {expected_columns:?}，实际 {actual_columns:?}"
            ));
        }
    }

    validate_audit_schema(connection)?;
    ensure_no_foreign_key_violations(connection)
}

fn validate_audit_schema(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare("PRAGMA table_info(identity_claim_audit)")
        .map_err(|error| format!("读取旧档认领审计表结构失败：{error}"))?;
    let columns = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, bool>(3)?))
        })
        .map_err(|error| format!("查询旧档认领审计表字段失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析旧档认领审计表字段失败：{error}"))?;
    let expected_columns = vec![
        ("id".to_string(), false),
        ("identity_id".to_string(), true),
        ("protocol".to_string(), true),
        ("namespace".to_string(), true),
        ("subject_kind".to_string(), true),
        ("subject_id".to_string(), true),
        ("claimed_account_id".to_string(), true),
        ("actor_account_id".to_string(), true),
        ("actor_subject_id".to_string(), true),
        ("source_message_id".to_string(), true),
        ("reason".to_string(), true),
        ("created_at".to_string(), true),
    ];
    if columns != expected_columns {
        return Err(format!(
            "数据库已标记迁移 v2，但旧档认领审计字段不匹配：{columns:?}"
        ));
    }

    let mut foreign_keys = connection
        .prepare("PRAGMA foreign_key_list(identity_claim_audit)")
        .map_err(|error| format!("读取旧档认领审计外键失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|error| format!("查询旧档认领审计外键失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析旧档认领审计外键失败：{error}"))?;
    foreign_keys.sort();
    if foreign_keys
        != [(
            "identity".to_string(),
            "identity_id".to_string(),
            "id".to_string(),
            "RESTRICT".to_string(),
        )]
    {
        return Err(format!(
            "数据库已标记迁移 v2，但旧档认领审计外键不匹配：{foreign_keys:?}"
        ));
    }

    let mut index_list = connection
        .prepare("PRAGMA index_list(identity_claim_audit)")
        .map_err(|error| format!("读取旧档认领审计索引失败：{error}"))?
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, bool>(2)?))
        })
        .map_err(|error| format!("查询旧档认领审计索引失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析旧档认领审计索引失败：{error}"))?;
    index_list.retain(|(_, unique)| *unique);
    let mut unique_columns = Vec::new();
    for (name, _) in index_list {
        let escaped_name = name.replace('"', "\"\"");
        let mut info = connection
            .prepare(&format!("PRAGMA index_info(\"{escaped_name}\")"))
            .map_err(|error| format!("读取旧档认领审计索引 {name} 字段失败：{error}"))?;
        let columns = info
            .query_map([], |row| row.get::<_, String>(2))
            .map_err(|error| format!("查询旧档认领审计索引 {name} 字段失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("解析旧档认领审计索引 {name} 字段失败：{error}"))?;
        unique_columns.push(columns);
    }
    let mut expected_unique_columns = vec![
        vec!["identity_id".to_string()],
        vec![
            "protocol".to_string(),
            "namespace".to_string(),
            "subject_kind".to_string(),
            "subject_id".to_string(),
        ],
    ];
    unique_columns.sort();
    expected_unique_columns.sort();
    if unique_columns != expected_unique_columns {
        return Err(format!(
            "数据库已标记迁移 v2，但旧档认领审计唯一约束不匹配：{unique_columns:?}"
        ));
    }

    let expected_triggers = [
        (
            "identity_claim_audit_no_update",
            "BEFORE UPDATE ON IDENTITY_CLAIM_AUDIT",
        ),
        (
            "identity_claim_audit_no_delete",
            "BEFORE DELETE ON IDENTITY_CLAIM_AUDIT",
        ),
        (
            "identity_claim_audit_no_reinsert",
            "BEFORE INSERT ON IDENTITY_CLAIM_AUDIT",
        ),
    ];
    let trigger_count = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'identity_claim_audit'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("检查旧档认领审计触发器失败：{error}"))?;
    if trigger_count != expected_triggers.len() as i64 {
        return Err("数据库已标记迁移 v2，但旧档认领审计触发器数量不匹配".to_string());
    }
    for (name, marker) in expected_triggers {
        let (table_name, sql) = connection
            .query_row(
                "SELECT tbl_name, sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
                [name],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| format!("读取旧档认领审计触发器 {name} 失败：{error}"))?
            .ok_or_else(|| format!("数据库已标记迁移 v2，但缺少旧档认领审计触发器 {name}"))?;
        let normalized_sql = sql.to_ascii_uppercase();
        if table_name != "identity_claim_audit"
            || !normalized_sql.contains(marker)
            || !normalized_sql.contains("RAISE(ABORT")
        {
            return Err(format!(
                "数据库已标记迁移 v2，但旧档认领审计触发器 {name} 内容不匹配"
            ));
        }
    }
    Ok(())
}

fn validate_v3_schema(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare("PRAGMA table_info(operation_log)")
        .map_err(|error| format!("读取操作日志表结构失败：{error}"))?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, bool>(3)?,
                row.get::<_, bool>(5)?,
            ))
        })
        .map_err(|error| format!("查询操作日志字段失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析操作日志字段失败：{error}"))?;
    let expected_columns = vec![
        ("id".to_string(), false, true),
        ("protocol".to_string(), true, false),
        ("account_id".to_string(), true, false),
        ("namespace".to_string(), true, false),
        ("subject_kind".to_string(), true, false),
        ("subject_id".to_string(), true, false),
        ("command".to_string(), true, false),
        ("outcome".to_string(), true, false),
        ("source_message_id".to_string(), true, false),
        ("details_json".to_string(), true, false),
        ("created_at".to_string(), true, false),
    ];
    if columns != expected_columns {
        return Err(format!(
            "数据库已标记迁移 v3，但操作日志字段不匹配：{columns:?}"
        ));
    }

    let table_sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'operation_log'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取操作日志建表语句失败：{error}"))?
        .ok_or_else(|| "数据库已标记迁移 v3，但缺少 operation_log 表".to_string())?;
    let normalized_table_sql = table_sql.to_ascii_uppercase();
    for marker in [
        "AUTOINCREMENT",
        "PROTOCOL IN ('ONEBOT11', 'QQ-OFFICIAL')",
        "OUTCOME IN ('OK', 'ERROR', 'DENIED')",
        "JSON_VALID(DETAILS_JSON)",
        "LENGTH(CAST(DETAILS_JSON AS BLOB)) <= 8192",
        "INSTR(LOWER(DETAILS_JSON), 'RAW_EVENT_JSON') = 0",
        "INSTR(LOWER(DETAILS_JSON), 'QIMEN_RAW_EVENT') = 0",
        "INSTR(LOWER(DETAILS_JSON), 'RAW_JSON') = 0",
        "INSTR(LOWER(DETAILS_JSON), 'BASE64://') = 0",
        "INSTR(LOWER(DETAILS_JSON), 'DATA:IMAGE/') = 0",
    ] {
        if !normalized_table_sql.contains(marker) {
            return Err(format!("数据库已标记迁移 v3，但操作日志约束缺少：{marker}"));
        }
    }

    let index_sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'operation_log_identity_page'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取操作日志分页索引失败：{error}"))?
        .ok_or_else(|| "数据库已标记迁移 v3，但缺少操作日志分页索引".to_string())?;
    if !index_sql
        .to_ascii_uppercase()
        .contains("ON OPERATION_LOG(PROTOCOL, ACCOUNT_ID, NAMESPACE, SUBJECT_KIND, SUBJECT_ID, ID)")
    {
        return Err("数据库已标记迁移 v3，但操作日志分页索引字段不匹配".to_string());
    }

    let trigger_count = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'operation_log'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("检查操作日志触发器失败：{error}"))?;
    let expected_triggers = [
        ("operation_log_no_update", "BEFORE UPDATE ON OPERATION_LOG"),
        ("operation_log_no_delete", "BEFORE DELETE ON OPERATION_LOG"),
        (
            "operation_log_no_reinsert",
            "BEFORE INSERT ON OPERATION_LOG",
        ),
        (
            "operation_log_safe_details",
            "BEFORE INSERT ON OPERATION_LOG",
        ),
    ];
    if trigger_count != expected_triggers.len() as i64 {
        return Err("数据库已标记迁移 v3，但操作日志触发器数量不匹配".to_string());
    }
    for (name, marker) in expected_triggers {
        let (table_name, sql) = connection
            .query_row(
                "SELECT tbl_name, sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
                [name],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| format!("读取操作日志触发器 {name} 失败：{error}"))?
            .ok_or_else(|| format!("数据库已标记迁移 v3，但缺少操作日志触发器 {name}"))?;
        let normalized_sql = sql.to_ascii_uppercase();
        if table_name != "operation_log"
            || !normalized_sql.contains(marker)
            || !normalized_sql.contains("RAISE(ABORT")
        {
            return Err(format!(
                "数据库已标记迁移 v3，但操作日志触发器 {name} 内容不匹配"
            ));
        }
        if name == "operation_log_no_reinsert" && !normalized_sql.contains("EXISTS") {
            return Err("数据库已标记迁移 v3，但操作日志禁止重插入触发器不完整".to_string());
        }
        if name == "operation_log_safe_details" {
            let legacy_whitelist =
                normalized_sql.contains("NOT IN ('CONTEXT', 'HAS_ARGS', 'REASON', 'DURATION_MS')");
            let v4_whitelist = normalized_sql.contains(
                "'CONTEXT', 'HAS_ARGS', 'REASON', 'DURATION_MS', 'TARGET_KIND', 'TARGET_ID'",
            );
            if !normalized_sql.contains("JSON_EACH") || (!legacy_whitelist && !v4_whitelist) {
                return Err("数据库已标记迁移 v3，但操作日志详情白名单触发器不完整".to_string());
            }
        }
    }
    probe_operation_log_guards(connection)
}

fn probe_operation_log_guards(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch("SAVEPOINT qimen_operation_log_guard_probe;")
        .map_err(|error| format!("开始操作日志保护探针失败：{error}"))?;
    let probe_result = (|| -> Result<(), String> {
        connection
            .execute(
                r#"
                INSERT INTO operation_log(
                    protocol, account_id, namespace, subject_kind, subject_id,
                    command, outcome, source_message_id, details_json, created_at
                ) VALUES('onebot11', 'schema-probe', 'schema-probe', 'user',
                         'schema-probe', 'schema-probe', 'ok', '', '{}', 0)
                "#,
                [],
            )
            .map_err(|error| format!("操作日志保护探针无法插入临时行：{error}"))?;
        let id = connection.last_insert_rowid();
        for (label, sql) in [
            (
                "UPDATE",
                "UPDATE operation_log SET outcome = 'error' WHERE id = ?1",
            ),
            ("DELETE", "DELETE FROM operation_log WHERE id = ?1"),
            (
                "REPLACE",
                r#"
                INSERT OR REPLACE INTO operation_log(
                    id, protocol, account_id, namespace, subject_kind, subject_id,
                    command, outcome, source_message_id, details_json, created_at
                ) VALUES(?1, 'onebot11', 'schema-probe', 'schema-probe', 'user',
                         'schema-probe', 'tampered', 'error', '', '{}', 0)
                "#,
            ),
        ] {
            if connection.execute(sql, [id]).is_ok() {
                return Err(format!("操作日志 {label} 保护探针被绕过"));
            }
        }
        if connection
            .execute(
                r#"
                INSERT INTO operation_log(
                    protocol, account_id, namespace, subject_kind, subject_id,
                    command, outcome, source_message_id, details_json, created_at
                ) VALUES('onebot11', 'schema-probe', 'schema-probe', 'user',
                         'schema-probe-2', 'schema-probe', 'ok', '',
                         '{"password":"secret"}', 0)
                "#,
                [],
            )
            .is_ok()
        {
            return Err("操作日志详情白名单保护探针被绕过".to_string());
        }
        Ok(())
    })();
    let rollback_result = connection
        .execute_batch(
            "ROLLBACK TO qimen_operation_log_guard_probe; RELEASE qimen_operation_log_guard_probe;",
        )
        .map_err(|error| format!("回滚操作日志保护探针失败：{error}"));
    match (probe_result, rollback_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(probe_error), Err(rollback_error)) => Err(format!("{probe_error}；{rollback_error}")),
    }
}

fn validate_v4_schema(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare("PRAGMA table_info(authorized_context)")
        .map_err(|error| format!("读取授权上下文表结构失败：{error}"))?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, bool>(5)?,
            ))
        })
        .map_err(|error| format!("查询授权上下文字段失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析授权上下文字段失败：{error}"))?;
    let expected_columns = vec![
        ("id".to_string(), "INTEGER".to_string(), false, true),
        ("protocol".to_string(), "TEXT".to_string(), true, false),
        ("account_id".to_string(), "TEXT".to_string(), true, false),
        ("namespace".to_string(), "TEXT".to_string(), true, false),
        ("context_kind".to_string(), "TEXT".to_string(), true, false),
        ("context_id".to_string(), "TEXT".to_string(), true, false),
        ("label".to_string(), "TEXT".to_string(), true, false),
        (
            "granted_by_subject_id".to_string(),
            "TEXT".to_string(),
            true,
            false,
        ),
        ("created_at".to_string(), "INTEGER".to_string(), true, false),
    ];
    if columns != expected_columns {
        return Err(format!(
            "数据库已标记迁移 v4，但授权上下文字段不匹配：{columns:?}"
        ));
    }

    let table_sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'authorized_context'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取授权上下文建表语句失败：{error}"))?
        .ok_or_else(|| "数据库已标记迁移 v4，但缺少 authorized_context 表".to_string())?;
    let normalized_table_sql = table_sql.to_ascii_uppercase();
    for marker in [
        "AUTOINCREMENT",
        "PROTOCOL IN ('ONEBOT11', 'QQ-OFFICIAL')",
        "LENGTH(ACCOUNT_ID) BETWEEN 1 AND 128",
        "ACCOUNT_ID = TRIM(ACCOUNT_ID)",
        "LENGTH(NAMESPACE) BETWEEN 1 AND 64",
        "CONTEXT_KIND IN ('GROUP', 'CHANNEL')",
        "LENGTH(CONTEXT_ID) BETWEEN 1 AND 256",
        "LENGTH(LABEL) <= 80",
        "LENGTH(GRANTED_BY_SUBJECT_ID) BETWEEN 1 AND 256",
        "CREATED_AT >= 0",
        "UNIQUE(PROTOCOL, ACCOUNT_ID, NAMESPACE, CONTEXT_KIND, CONTEXT_ID)",
    ] {
        if !normalized_table_sql.contains(marker) {
            return Err(format!(
                "数据库已标记迁移 v4，但授权上下文约束缺少：{marker}"
            ));
        }
    }

    let mut unique_columns = Vec::new();
    let mut page_index_found = false;
    let mut indexes = connection
        .prepare("PRAGMA index_list(authorized_context)")
        .map_err(|error| format!("读取授权上下文索引失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .map_err(|error| format!("查询授权上下文索引失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析授权上下文索引失败：{error}"))?;
    indexes.sort();
    for (name, unique, origin, partial) in indexes {
        let escaped_name = name.replace('"', "\"\"");
        let columns = connection
            .prepare(&format!("PRAGMA index_info(\"{escaped_name}\")"))
            .map_err(|error| format!("读取授权上下文索引 {name} 失败：{error}"))?
            .query_map([], |row| row.get::<_, String>(2))
            .map_err(|error| format!("查询授权上下文索引 {name} 失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("解析授权上下文索引 {name} 失败：{error}"))?;
        if unique {
            if partial || origin != "u" {
                return Err(format!(
                    "数据库已标记迁移 v4，但授权上下文唯一索引来源或范围不匹配：{name}"
                ));
            }
            unique_columns.push(columns);
        } else if name == "authorized_context_bot_page" {
            if partial || origin != "c" || columns != ["protocol", "account_id", "namespace", "id"]
            {
                return Err("数据库已标记迁移 v4，但授权上下文分页索引定义不匹配".to_string());
            }
            page_index_found = true;
        }
    }
    if unique_columns
        != [vec![
            "protocol".to_string(),
            "account_id".to_string(),
            "namespace".to_string(),
            "context_kind".to_string(),
            "context_id".to_string(),
        ]]
    {
        return Err(format!(
            "数据库已标记迁移 v4，但授权上下文唯一约束不匹配：{unique_columns:?}"
        ));
    }
    if !page_index_found {
        return Err("数据库已标记迁移 v4，但缺少授权上下文分页索引".to_string());
    }

    let mut trigger_statement = connection
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'authorized_context' ORDER BY name",
        )
        .map_err(|error| format!("读取授权上下文触发器失败：{error}"))?;
    let table_triggers = trigger_statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("查询授权上下文触发器失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析授权上下文触发器失败：{error}"))?;
    if !table_triggers.is_empty() {
        return Err(format!(
            "数据库已标记迁移 v4，但授权上下文表存在未声明触发器：{table_triggers:?}"
        ));
    }

    let safe_details_sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'operation_log_safe_details' AND tbl_name = 'operation_log'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取 v4 操作日志详情触发器失败：{error}"))?
        .ok_or_else(|| "数据库已标记迁移 v4，但缺少操作日志详情触发器".to_string())?;
    let safe_details_sql = safe_details_sql.to_ascii_uppercase();
    for marker in [
        "'TARGET_KIND'",
        "'TARGET_ID'",
        "JSON_TYPE(NEW.DETAILS_JSON) != 'OBJECT'",
        "JSON_TYPE",
        "JSON_EXTRACT",
        "NOT IN ('GROUP', 'CHANNEL')",
        "BETWEEN 1 AND 256",
        "CHAR(0)",
        "CHAR(31)",
        "CHAR(127)",
        "RAISE(ABORT",
    ] {
        if !safe_details_sql.contains(marker) {
            return Err(format!(
                "数据库已标记迁移 v4，但操作日志目标详情约束缺少：{marker}"
            ));
        }
    }
    probe_authorized_context_guards(connection)?;
    probe_v4_details_guard(connection)
}

fn probe_authorized_context_guards(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch("SAVEPOINT qimen_authorized_context_guard_probe;")
        .map_err(|error| format!("开始授权上下文约束探针失败：{error}"))?;
    let probe_result = (|| -> Result<(), String> {
        let token = connection
            .query_row("SELECT lower(hex(randomblob(16)))", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("生成授权上下文探针标识失败：{error}"))?;
        let account_id = format!("v4-probe-{token}");
        let namespace = "v4-probe";
        let actor = "v4-probe";
        let insert = |protocol: &str,
                      account_id: &str,
                      namespace: &str,
                      context_kind: &str,
                      context_id: &str,
                      label: &str,
                      actor: &str,
                      created_at: i64| {
            connection.execute(
                r#"
                INSERT INTO authorized_context(
                    protocol, account_id, namespace, context_kind, context_id,
                    label, granted_by_subject_id, created_at
                ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    protocol,
                    account_id,
                    namespace,
                    context_kind,
                    context_id,
                    label,
                    actor,
                    created_at
                ],
            )
        };

        insert(
            "onebot11",
            &account_id,
            namespace,
            "channel",
            "valid",
            "",
            actor,
            0,
        )
        .map_err(|error| format!("授权上下文约束探针无法插入合法行：{error}"))?;
        if insert(
            "onebot11",
            &account_id,
            namespace,
            "channel",
            "valid",
            "",
            actor,
            0,
        )
        .is_ok()
        {
            return Err("授权上下文唯一约束探针被绕过".to_string());
        }

        let long_account_id = "a".repeat(129);
        let long_namespace = "n".repeat(65);
        let long_context_id = "c".repeat(257);
        let long_label = "标".repeat(81);
        let long_actor = "u".repeat(257);
        for (
            label,
            protocol,
            candidate_account,
            candidate_namespace,
            kind,
            id,
            entry_label,
            candidate_actor,
            created_at,
        ) in [
            (
                "protocol",
                "unknown",
                account_id.as_str(),
                namespace,
                "group",
                "invalid-protocol",
                "",
                actor,
                0,
            ),
            (
                "empty account_id",
                "onebot11",
                "",
                namespace,
                "group",
                "invalid-account-empty",
                "",
                actor,
                0,
            ),
            (
                "trimmed account_id",
                "onebot11",
                " account",
                namespace,
                "group",
                "invalid-account-trim",
                "",
                actor,
                0,
            ),
            (
                "long account_id",
                "onebot11",
                long_account_id.as_str(),
                namespace,
                "group",
                "invalid-account-long",
                "",
                actor,
                0,
            ),
            (
                "empty namespace",
                "onebot11",
                account_id.as_str(),
                "",
                "group",
                "invalid-namespace-empty",
                "",
                actor,
                0,
            ),
            (
                "long namespace",
                "onebot11",
                account_id.as_str(),
                long_namespace.as_str(),
                "group",
                "invalid-namespace-long",
                "",
                actor,
                0,
            ),
            (
                "context kind",
                "onebot11",
                account_id.as_str(),
                namespace,
                "private",
                "invalid-kind",
                "",
                actor,
                0,
            ),
            (
                "empty context_id",
                "onebot11",
                account_id.as_str(),
                namespace,
                "group",
                "",
                "",
                actor,
                0,
            ),
            (
                "long context_id",
                "onebot11",
                account_id.as_str(),
                namespace,
                "group",
                long_context_id.as_str(),
                "",
                actor,
                0,
            ),
            (
                "long label",
                "onebot11",
                account_id.as_str(),
                namespace,
                "group",
                "invalid-label",
                long_label.as_str(),
                actor,
                0,
            ),
            (
                "empty actor",
                "onebot11",
                account_id.as_str(),
                namespace,
                "group",
                "invalid-actor-empty",
                "",
                "",
                0,
            ),
            (
                "long actor",
                "onebot11",
                account_id.as_str(),
                namespace,
                "group",
                "invalid-actor-long",
                "",
                long_actor.as_str(),
                0,
            ),
            (
                "created_at",
                "onebot11",
                account_id.as_str(),
                namespace,
                "group",
                "invalid-created-at",
                "",
                actor,
                -1,
            ),
        ] {
            if insert(
                protocol,
                candidate_account,
                candidate_namespace,
                kind,
                id,
                entry_label,
                candidate_actor,
                created_at,
            )
            .is_ok()
            {
                return Err(format!("授权上下文 {label} 约束探针被绕过"));
            }
        }
        Ok(())
    })();
    let rollback_result = connection
        .execute_batch(
            "ROLLBACK TO qimen_authorized_context_guard_probe; RELEASE qimen_authorized_context_guard_probe;",
        )
        .map_err(|error| format!("回滚授权上下文约束探针失败：{error}"));
    match (probe_result, rollback_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(probe_error), Err(rollback_error)) => Err(format!("{probe_error}；{rollback_error}")),
    }
}

fn probe_v4_details_guard(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch("SAVEPOINT qimen_v4_details_probe;")
        .map_err(|error| format!("开始 v4 操作日志详情探针失败：{error}"))?;
    let probe_result = (|| -> Result<(), String> {
        connection
            .execute(
                r#"
                INSERT INTO operation_log(
                    protocol, account_id, namespace, subject_kind, subject_id,
                    command, outcome, source_message_id, details_json, created_at
                ) VALUES('onebot11', 'v4-probe', 'v4-probe', 'user', 'v4-probe',
                         'v4-probe', 'ok', '',
                         '{"target_kind":"group","target_id":"group-1"}', 0)
                "#,
                [],
            )
            .map_err(|error| format!("v4 操作日志详情探针无法插入合法行：{error}"))?;
        for details_expression in [
            r#"'\"secret\"'"#,
            "'null'",
            "'123'",
            r#"'{"target_kind":"private","target_id":"group-1"}'"#,
            r#"'{"target_kind":"group","target_id":123}'"#,
            "json_object('target_kind', 'group', 'target_id', printf('%0257d', 0))",
            "json_object('target_kind', 'group', 'target_id', char(10))",
        ] {
            let sql = format!(
                r#"
                INSERT INTO operation_log(
                    protocol, account_id, namespace, subject_kind, subject_id,
                    command, outcome, source_message_id, details_json, created_at
                ) VALUES('onebot11', 'v4-probe', 'v4-probe', 'user', 'v4-probe-invalid',
                         'v4-probe', 'ok', '', {details_expression}, 0)
                "#
            );
            if connection.execute(&sql, []).is_ok() {
                return Err("v4 操作日志目标详情保护探针被绕过".to_string());
            }
        }
        Ok(())
    })();
    let rollback_result = connection
        .execute_batch("ROLLBACK TO qimen_v4_details_probe; RELEASE qimen_v4_details_probe;")
        .map_err(|error| format!("回滚 v4 操作日志详情探针失败：{error}"));
    match (probe_result, rollback_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(probe_error), Err(rollback_error)) => Err(format!("{probe_error}；{rollback_error}")),
    }
}

fn validate_identity_key(key: &IdentityKey<'_>) -> Result<(), String> {
    validate_account_id(key.account_id)?;
    if key.namespace.is_empty()
        || key.namespace.len() > 64
        || !key
            .namespace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("身份 namespace 无效".to_string());
    }
    if key.subject_kind != "user" {
        return Err("当前只支持 user 身份类型".to_string());
    }
    if !valid_audit_value(key.subject_id, 256) {
        return Err("发送者 ID 必须是 1 到 256 个无控制字符的字符串".to_string());
    }
    Ok(())
}

fn validate_account_id(value: &str) -> Result<(), String> {
    if value != value.trim() || !valid_audit_value(value, 128) {
        return Err("机器人 account_id 必须是 1 到 128 个无控制字符的非空字符串".to_string());
    }
    Ok(())
}

fn insert_operation_log(
    connection: &Connection,
    key: &IdentityKey<'_>,
    operation: &OperationLogInput<'_>,
) -> Result<i64, String> {
    validate_operation_input(operation)?;
    connection
        .execute(
            r#"
            INSERT INTO operation_log(
                protocol, account_id, namespace, subject_kind, subject_id,
                command, outcome, source_message_id, details_json, created_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                key.protocol.as_str(),
                key.account_id,
                key.namespace,
                key.subject_kind,
                key.subject_id,
                operation.command,
                operation.outcome,
                operation.source_message_id,
                operation.details_json,
                now_timestamp()?
            ],
        )
        .map_err(|error| format!("写入操作日志失败：{error}"))?;
    Ok(connection.last_insert_rowid())
}

fn validate_operation_input(operation: &OperationLogInput<'_>) -> Result<(), String> {
    let command = operation.command;
    if command != command.trim() || !valid_audit_value(command, 128) {
        return Err("操作命令必须是 1 到 128 个无控制字符的非空字符串".to_string());
    }
    if !matches!(operation.outcome, "ok" | "error" | "denied") {
        return Err("操作结果只能是 ok、error 或 denied".to_string());
    }
    let source_message_id = operation.source_message_id;
    if source_message_id.chars().count() > 256 || source_message_id.chars().any(char::is_control) {
        return Err("来源消息 ID 必须是不超过 256 个字符且不含控制字符的字符串".to_string());
    }
    let details_json = operation.details_json;
    if details_json.len() > 8_192 {
        return Err("操作日志 details_json 不能超过 8192 字节".to_string());
    }
    let details: serde_json::Value = serde_json::from_str(details_json)
        .map_err(|error| format!("操作日志 details_json 不是有效 JSON：{error}"))?;
    let object = details
        .as_object()
        .ok_or_else(|| "操作日志 details_json 必须是对象".to_string())?;
    for (key, value) in object {
        match key.as_str() {
            "context" => {
                let context = value
                    .as_str()
                    .ok_or_else(|| "操作日志 context 必须是字符串".to_string())?;
                if !matches!(context, "private" | "group" | "channel" | "dms" | "system") {
                    return Err("操作日志 context 值无效".to_string());
                }
            }
            "has_args" => {
                if !value.is_boolean() {
                    return Err("操作日志 has_args 必须是布尔值".to_string());
                }
            }
            "reason" => {
                let reason = value
                    .as_str()
                    .ok_or_else(|| "操作日志 reason 必须是字符串".to_string())?;
                if !valid_reason_code(reason) {
                    return Err("操作日志 reason 必须是不超过 64 个字符的代码".to_string());
                }
            }
            "duration_ms" => {
                let duration = value
                    .as_u64()
                    .ok_or_else(|| "操作日志 duration_ms 必须是非负整数".to_string())?;
                if duration > 600_000 {
                    return Err("操作日志 duration_ms 不能超过 600000".to_string());
                }
            }
            "target_kind" => {
                let target_kind = value
                    .as_str()
                    .ok_or_else(|| "操作日志 target_kind 必须是字符串".to_string())?;
                if !matches!(target_kind, "group" | "channel") {
                    return Err("操作日志 target_kind 只能是 group 或 channel".to_string());
                }
            }
            "target_id" => {
                let target_id = value
                    .as_str()
                    .ok_or_else(|| "操作日志 target_id 必须是字符串".to_string())?;
                if !valid_audit_value(target_id, 256) {
                    return Err(
                        "操作日志 target_id 必须是 1 到 256 个无控制字符的字符串".to_string()
                    );
                }
            }
            _ => return Err(format!("操作日志 details 字段不允许：{key}")),
        }
    }
    Ok(())
}

fn validate_context_fields(
    context_kind: &str,
    context_id: &str,
    label: &str,
    granted_by_subject_id: &str,
) -> Result<(), String> {
    if !matches!(context_kind, "group" | "channel") {
        return Err("授权上下文类型只能是 group 或 channel".to_string());
    }
    if !valid_audit_value(context_id, 256) {
        return Err("授权上下文 ID 必须是 1 到 256 个无控制字符的字符串".to_string());
    }
    if label.chars().count() > 80 || label.chars().any(char::is_control) {
        return Err("授权上下文标签必须是不超过 80 个字符且无控制字符的字符串".to_string());
    }
    if !valid_audit_value(granted_by_subject_id, 256) {
        return Err("授权操作者 ID 必须是 1 到 256 个无控制字符的字符串".to_string());
    }
    Ok(())
}

fn validate_context_operation(
    operation: &OperationLogInput<'_>,
    context_kind: &str,
    context_id: &str,
) -> Result<(), String> {
    validate_operation_input(operation)?;
    let details: serde_json::Value = serde_json::from_str(operation.details_json)
        .map_err(|error| format!("操作日志 details_json 不是有效 JSON：{error}"))?;
    let object = details
        .as_object()
        .ok_or_else(|| "操作日志 details_json 必须是对象".to_string())?;
    if object
        .get("target_kind")
        .and_then(serde_json::Value::as_str)
        != Some(context_kind)
        || object.get("target_id").and_then(serde_json::Value::as_str) != Some(context_id)
    {
        return Err("授权操作日志的 target_kind/target_id 与目标上下文不一致".to_string());
    }
    Ok(())
}

fn validate_context_page(after_id: Option<i64>, limit: usize) -> Result<(), String> {
    if !(1..=100).contains(&limit) {
        return Err("授权上下文分页数量必须在 1 到 100 之间".to_string());
    }
    if after_id.is_some_and(|after_id| after_id < 0) {
        return Err("授权上下文分页游标不能为负数".to_string());
    }
    Ok(())
}

fn valid_reason_code(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_claim_actor(actor: &LegacyClaimActor<'_>) -> Result<(), String> {
    validate_account_id(actor.account_id)?;
    for (label, value, max_chars) in [
        ("操作者 ID", actor.subject_id, 256),
        ("来源消息 ID", actor.message_id, 256),
        ("认领原因", actor.reason, 128),
    ] {
        if !valid_audit_value(value, max_chars) {
            return Err(format!(
                "{label} 必须是 1 到 {max_chars} 个无控制字符的字符串"
            ));
        }
    }
    Ok(())
}

fn valid_audit_value(value: &str, max_chars: usize) -> bool {
    let count = value.chars().count();
    (1..=max_chars).contains(&count) && !value.chars().any(char::is_control)
}

fn ensure_no_legacy_identity(connection: &Connection, key: &IdentityKey<'_>) -> Result<(), String> {
    if matching_legacy_identity_id(connection, key)?.is_some() {
        return Err(LEGACY_CLAIM_REQUIRED.to_string());
    }
    Ok(())
}

fn matching_legacy_identity_id(
    connection: &Connection,
    key: &IdentityKey<'_>,
) -> Result<Option<i64>, String> {
    connection
        .query_row(
            r#"
            SELECT id FROM identity
             WHERE protocol = ?1 AND account_id IS NULL AND namespace = ?2
               AND subject_kind = ?3 AND subject_id = ?4
            "#,
            params![
                key.protocol.as_str(),
                key.namespace,
                key.subject_kind,
                key.subject_id
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("检查旧身份存档失败：{error}"))
}

fn ensure_identity(transaction: &Transaction<'_>, key: &IdentityKey<'_>) -> Result<i64, String> {
    let timestamp = now_timestamp()?;
    transaction
        .execute(
            r#"
            INSERT OR IGNORE INTO identity(
                protocol, account_id, namespace, subject_kind, subject_id, created_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                key.protocol.as_str(),
                key.account_id,
                key.namespace,
                key.subject_kind,
                key.subject_id,
                timestamp
            ],
        )
        .map_err(|error| format!("创建玩家身份失败：{error}"))?;
    transaction
        .query_row(
            r#"
            SELECT id FROM identity
             WHERE protocol = ?1 AND account_id = ?2 AND namespace = ?3
               AND subject_kind = ?4 AND subject_id = ?5
            "#,
            params![
                key.protocol.as_str(),
                key.account_id,
                key.namespace,
                key.subject_kind,
                key.subject_id
            ],
            |row| row.get(0),
        )
        .map_err(|error| format!("读取玩家身份失败：{error}"))
}

fn now_timestamp() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|error| format!("系统时间早于 Unix epoch：{error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use tempfile::tempdir;

    use super::*;

    fn test_store() -> (tempfile::TempDir, Store) {
        let directory = tempdir().expect("应创建测试目录");
        let store = Store::initialize(directory.path(), &DatabaseConfig::default())
            .expect("应初始化数据库");
        (directory, store)
    }

    fn identity<'a>() -> IdentityKey<'a> {
        IdentityKey {
            protocol: Protocol::OneBot11,
            account_id: "10001",
            namespace: "test",
            subject_kind: "user",
            subject_id: "1875390189",
        }
    }

    fn create_v1_database(directory: &Path, subject_id: &str) -> PathBuf {
        let relative_path = &DatabaseConfig::default().relative_path;
        let path = directory.join(relative_path);
        fs::create_dir_all(path.parent().expect("数据库应有父目录")).expect("应创建目录");
        let connection = Connection::open(&path).expect("应创建 v1 数据库");
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .expect("应启用外键");
        connection
            .execute_batch(MIGRATION_V1)
            .expect("应创建 v1 结构");
        connection
            .execute(
                "INSERT OR IGNORE INTO schema_migration(version, applied_at) VALUES(1, 1)",
                [],
            )
            .expect("应记录 v1");
        connection
            .execute(
                "INSERT INTO identity(protocol, namespace, subject_kind, subject_id, created_at) VALUES('onebot11', 'test', 'user', ?1, 1)",
                [subject_id],
            )
            .expect("应写入旧身份");
        let identity_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO player(identity_id, name, gender, created_at, updated_at) VALUES(?1, '旧角色', '男', 1, 1)",
                [identity_id],
            )
            .expect("应写入旧角色");
        let player_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO player_wuhun(player_id, wuhun_id, slot, awaken_life, created_at) VALUES(?1, (SELECT id FROM wuhun WHERE name = '独狼'), 1, 1, 1)",
                [player_id],
            )
            .expect("应写入旧武魂");
        connection
            .execute(
                "UPDATE sqlite_sequence SET seq = 99 WHERE name = 'identity'",
                [],
            )
            .expect("应提高旧序列水位");
        path
    }

    fn actor<'a>(account_id: &'a str) -> LegacyClaimActor<'a> {
        LegacyClaimActor {
            account_id,
            subject_id: "owner-user",
            message_id: "message-1",
            reason: "owner-explicit-legacy-claim",
        }
    }

    #[test]
    fn register_and_query_player() {
        let (_directory, store) = test_store();
        let player = store
            .register_player(&identity(), "唐小三", "男")
            .expect("应创建角色");
        assert_eq!(player.name, "唐小三");
        assert_eq!(player.map_name, "圣魂村");
        assert!(player.wuhun_name.is_none());
        assert!(store.register_player(&identity(), "重复", "男").is_err());
    }

    #[test]
    fn awaken_once_and_persist_wuhun() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "小舞", "女")
            .expect("应创建角色");
        let awakened = store.awaken_wuhun(&identity()).expect("应觉醒武魂");
        assert!(["独狼", "萝卜", "镰刀"].contains(&awakened.name.as_str()));
        assert!(store.awaken_wuhun(&identity()).is_err());
        let player = store
            .player_status(&identity())
            .expect("查询不应失败")
            .expect("角色应存在");
        assert_eq!(player.wuhun_name.as_deref(), Some(awakened.name.as_str()));
    }

    #[test]
    fn separates_protocol_namespaces_and_bot_accounts() {
        let (_directory, store) = test_store();
        let mut official = identity();
        official.protocol = Protocol::QqOfficial;
        official.account_id = "qq-app-1";
        let mut second_bot = identity();
        second_bot.account_id = "10002";
        store
            .register_player(&identity(), "一号", "男")
            .expect("OneBot 角色应创建");
        store
            .register_player(&official, "二号", "女")
            .expect("官方 QQ 角色应独立创建");
        store
            .register_player(&second_bot, "三号", "男")
            .expect("第二个 OneBot 账号应独立创建");
        assert_eq!(
            store
                .player_status(&second_bot)
                .expect("查询应成功")
                .expect("第二账号角色应存在")
                .name,
            "三号"
        );
    }

    #[test]
    fn rejects_missing_or_malformed_account_id() {
        let (_directory, store) = test_store();
        for account_id in ["", " leading", "trailing ", "bad\nvalue"] {
            let mut key = identity();
            key.account_id = account_id;
            assert!(store.player_status(&key).is_err());
            assert!(store.register_player(&key, "无效", "男").is_err());
        }
    }

    #[test]
    fn migrates_v1_without_rekeying_relations_or_sequence() {
        let directory = tempdir().expect("应创建测试目录");
        let path = create_v1_database(directory.path(), "legacy-user");
        let store = Store::initialize(directory.path(), &DatabaseConfig::default())
            .expect("v1 应迁移到 v2");
        let connection = store.open().expect("应重开数据库");
        assert_eq!(
            connection
                .query_row(
                    "SELECT i.id FROM identity i JOIN player p ON p.identity_id = i.id WHERE i.subject_id = 'legacy-user'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("旧关系应保留"),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM player_wuhun", [], |row| row
                    .get::<_, i64>(0))
                .expect("旧武魂应保留"),
            1
        );
        ensure_no_foreign_key_violations(&connection).expect("迁移后外键应完整");
        assert_eq!(
            connection
                .query_row(
                    "SELECT seq FROM sqlite_sequence WHERE name = 'identity'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("身份序列应存在"),
            99
        );
        drop(connection);

        let key = IdentityKey {
            subject_id: "new-user",
            ..identity()
        };
        store
            .register_player(&key, "新角色", "女")
            .expect("应创建迁移后的新角色");
        let connection = Connection::open(path).expect("应检查数据库");
        let new_id = connection
            .query_row(
                "SELECT id FROM identity WHERE account_id = '10001' AND subject_id = 'new-user'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("应找到新身份");
        assert!(new_id > 99);
    }

    #[test]
    fn migration_is_idempotent_and_validates_recorded_schema() {
        let directory = tempdir().expect("应创建测试目录");
        create_v1_database(directory.path(), "legacy-user");
        Store::initialize(directory.path(), &DatabaseConfig::default()).expect("首次迁移应成功");
        let store = Store::initialize(directory.path(), &DatabaseConfig::default())
            .expect("重复初始化应成功");
        let connection = store.open().expect("应打开数据库");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM schema_migration WHERE version IN (2, 3, 4)",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("应读取迁移记录"),
            3
        );

        let broken_directory = tempdir().expect("应创建损坏测试目录");
        let path = create_v1_database(broken_directory.path(), "broken-user");
        let connection = Connection::open(path).expect("应打开损坏测试数据库");
        connection
            .execute(
                "INSERT INTO schema_migration(version, applied_at) VALUES(2, 2)",
                [],
            )
            .expect("应伪造 v2 记录");
        drop(connection);
        let error = Store::initialize(broken_directory.path(), &DatabaseConfig::default())
            .expect_err("已记录但结构错误的 v2 必须失败");
        assert!(error.contains("结构不匹配"));
    }

    #[test]
    fn concurrent_initialization_is_idempotent() {
        let directory = tempdir().expect("应创建测试目录");
        let path = directory.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                Store::initialize(&path, &DatabaseConfig::default()).map(|_| ())
            }));
        }
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("线程不应 panic"))
            .collect::<Vec<_>>();
        assert!(
            results.iter().all(Result::is_ok),
            "并发初始化结果：{results:?}"
        );
        let store = Store::initialize(&path, &DatabaseConfig::default()).expect("应可再次打开");
        let connection = store.open().expect("应检查迁移记录");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM schema_migration WHERE version IN (2, 3, 4)",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("应读取迁移记录"),
            3
        );
    }

    #[test]
    fn recorded_v2_with_malformed_audit_structure_fails_closed() {
        let directory = tempdir().expect("应创建测试目录");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("迁移应成功");
        let connection = store.open().expect("应打开数据库");
        connection
            .execute_batch(
                r#"
                DROP TABLE identity_claim_audit;
                CREATE TABLE identity_claim_audit(
                    id INTEGER PRIMARY KEY,
                    identity_id INTEGER NOT NULL,
                    protocol TEXT NOT NULL,
                    namespace TEXT NOT NULL,
                    subject_kind TEXT NOT NULL,
                    subject_id TEXT NOT NULL,
                    claimed_account_id TEXT NOT NULL,
                    actor_account_id TEXT NOT NULL,
                    actor_subject_id TEXT NOT NULL,
                    source_message_id TEXT NOT NULL,
                    reason TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                CREATE TABLE audit_dummy(id INTEGER);
                CREATE TRIGGER identity_claim_audit_no_update
                BEFORE UPDATE ON audit_dummy BEGIN SELECT RAISE(ABORT, 'dummy'); END;
                CREATE TRIGGER identity_claim_audit_no_delete
                BEFORE DELETE ON audit_dummy BEGIN SELECT RAISE(ABORT, 'dummy'); END;
                CREATE TRIGGER identity_claim_audit_no_reinsert
                BEFORE INSERT ON audit_dummy BEGIN SELECT RAISE(ABORT, 'dummy'); END;
                "#,
            )
            .expect("应构造损坏审计结构");
        drop(connection);
        let error = Store::initialize(directory.path(), &DatabaseConfig::default())
            .expect_err("记录 v2 但审计结构损坏必须失败");
        assert!(error.contains("审计"));
    }

    #[test]
    fn migration_failure_restores_foreign_key_enforcement() {
        let directory = tempdir().expect("应创建测试目录");
        let path = create_v1_database(directory.path(), "legacy-user");
        let store = Store {
            path,
            busy_timeout: Duration::from_millis(3_000),
        };
        let mut connection = store.open().expect("应打开数据库");
        connection
            .execute("CREATE TABLE identity_v2(blocker INTEGER)", [])
            .expect("应制造迁移冲突");
        assert!(store.migrate(&mut connection).is_err());
        verify_foreign_keys(&connection, true).expect("失败后必须恢复外键");
        assert!(!migration_applied(&connection, 2).expect("应读取迁移状态"));
    }

    #[test]
    fn legacy_rows_block_normal_access_until_explicit_claim() {
        let directory = tempdir().expect("应创建测试目录");
        create_v1_database(directory.path(), "legacy-user");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("迁移应成功");
        let key = IdentityKey {
            subject_id: "legacy-user",
            ..identity()
        };
        assert_eq!(
            store.inspect_legacy_identity(&key).expect("应检查旧档"),
            LegacyIdentityState::Legacy
        );
        let error = store
            .player_status(&key)
            .expect_err("旧行不得被普通查询自动接管");
        assert!(error.contains("旧档认领"));
        assert!(store.register_player(&key, "重复角色", "男").is_err());

        let result = store
            .claim_legacy_identity(&key, &actor(key.account_id))
            .expect("明确认领应成功");
        assert_eq!(result, LegacyClaimResult::Claimed { identity_id: 1 });
        assert_eq!(
            store.inspect_legacy_identity(&key).expect("应检查已认领档"),
            LegacyIdentityState::ClaimedToCurrent
        );
        assert_eq!(
            store
                .player_status(&key)
                .expect("认领后应可查询")
                .expect("旧角色应存在")
                .name,
            "旧角色"
        );
        assert_eq!(
            store
                .claim_legacy_identity(&key, &actor(key.account_id))
                .expect("重复认领应有幂等结果"),
            LegacyClaimResult::AlreadyClaimed { identity_id: 1 }
        );
    }

    #[test]
    fn claim_conflict_never_merges_or_deletes_known_identity() {
        let directory = tempdir().expect("应创建测试目录");
        create_v1_database(directory.path(), "legacy-user");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("迁移应成功");
        let key = IdentityKey {
            subject_id: "legacy-user",
            ..identity()
        };
        let connection = store.open().expect("应打开数据库");
        connection
            .execute(
                "INSERT INTO identity(protocol, account_id, namespace, subject_kind, subject_id, created_at) VALUES('onebot11', '10001', 'test', 'user', 'legacy-user', 2)",
                [],
            )
            .expect("应制造已知身份冲突");
        let known_id = connection.last_insert_rowid();
        drop(connection);
        assert_eq!(
            store
                .claim_legacy_identity(&key, &actor(key.account_id))
                .expect("冲突应作为业务结果返回"),
            LegacyClaimResult::Conflict
        );
        let connection = store.open().expect("应重开数据库");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM identity WHERE subject_id = 'legacy-user'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("应统计身份"),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT account_id FROM identity WHERE id = ?1",
                    [known_id],
                    |row| row.get::<_, String>(0)
                )
                .expect("已知身份应保留"),
            "10001"
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM identity_claim_audit", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .expect("不应写入审计"),
            0
        );
    }

    #[test]
    fn concurrent_claims_allow_exactly_one_account() {
        let directory = tempdir().expect("应创建测试目录");
        create_v1_database(directory.path(), "legacy-user");
        let store = Arc::new(
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("迁移应成功"),
        );
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for account_id in ["10001", "10002"] {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let key = IdentityKey {
                    protocol: Protocol::OneBot11,
                    account_id,
                    namespace: "test",
                    subject_kind: "user",
                    subject_id: "legacy-user",
                };
                store
                    .claim_legacy_identity(&key, &actor(account_id))
                    .expect("并发认领不应产生数据库错误")
            }));
        }
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("线程不应 panic"))
            .collect::<Vec<_>>();
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, LegacyClaimResult::Claimed { .. }))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| **result == LegacyClaimResult::Conflict)
                .count(),
            1
        );
    }

    #[test]
    fn claim_audit_is_atomic_and_immutable() {
        let directory = tempdir().expect("应创建测试目录");
        create_v1_database(directory.path(), "legacy-user");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("迁移应成功");
        let key = IdentityKey {
            subject_id: "legacy-user",
            ..identity()
        };
        store
            .claim_legacy_identity(&key, &actor(key.account_id))
            .expect("认领应成功");
        let connection = store.open().expect("应打开数据库");
        assert!(
            connection
                .execute("UPDATE identity_claim_audit SET reason = 'tampered'", [])
                .is_err()
        );
        assert!(
            connection
                .execute("DELETE FROM identity_claim_audit", [])
                .is_err()
        );
        assert!(
            connection
                .execute(
                    r#"
                    INSERT OR REPLACE INTO identity_claim_audit(
                        identity_id, protocol, namespace, subject_kind, subject_id,
                        claimed_account_id, actor_account_id, actor_subject_id,
                        source_message_id, reason, created_at
                    ) VALUES(1, 'onebot11', 'test', 'user', 'legacy-user',
                              '10001', '10001', 'tampered-owner', 'message-2',
                              'tampered', 2)
                    "#,
                    [],
                )
                .is_err()
        );
        assert_eq!(
            connection
                .query_row("SELECT reason FROM identity_claim_audit", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .expect("审计应保留"),
            "owner-explicit-legacy-claim"
        );
    }

    #[test]
    fn operation_log_appends_and_pages_within_stable_identity() {
        let (_directory, store) = test_store();
        let first_id = store
            .append_operation(&identity(), "状态", "ok", "", r#"{"duration_ms":1}"#)
            .expect("后台操作日志应允许空消息 ID");
        let second_id = store
            .append_operation(
                &identity(),
                "武魂觉醒",
                "error",
                "message-2",
                r#"{"reason":"already-awakened"}"#,
            )
            .expect("第二条日志应写入");
        let third_id = store
            .append_operation(
                &identity(),
                "位置",
                "denied",
                "message-3",
                r#"{"reason":"legacy-claim-required"}"#,
            )
            .expect("第三条日志应写入");
        let other_identity = IdentityKey {
            account_id: "10002",
            ..identity()
        };
        store
            .append_operation(&other_identity, "状态", "ok", "message-other", "{}")
            .expect("另一机器人账号日志应写入");

        let first_page = store
            .list_operation_logs(&identity(), None, 2)
            .expect("第一页应读取");
        assert_eq!(
            first_page
                .entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![first_id, second_id]
        );
        assert_eq!(first_page.next_after_id, Some(second_id));
        assert_eq!(first_page.entries[0].protocol, Protocol::OneBot11);
        assert_eq!(first_page.entries[0].account_id, "10001");
        assert_eq!(first_page.entries[0].details_json, r#"{"duration_ms":1}"#);

        let second_page = store
            .list_operation_logs(&identity(), first_page.next_after_id, 2)
            .expect("第二页应读取");
        assert_eq!(second_page.entries.len(), 1);
        assert_eq!(second_page.entries[0].id, third_id);
        assert_eq!(second_page.next_after_id, None);
        assert_eq!(
            store
                .list_operation_logs(&other_identity, None, 100)
                .expect("另一账号应可读取")
                .entries
                .len(),
            1
        );
    }

    #[test]
    fn operation_log_rejects_invalid_or_sensitive_input() {
        let (_directory, store) = test_store();
        for command in ["", " padded", "bad\ncommand"] {
            assert!(
                store
                    .append_operation(&identity(), command, "ok", "message", "{}")
                    .is_err()
            );
        }
        assert!(
            store
                .append_operation(&identity(), &"命".repeat(129), "ok", "message", "{}")
                .is_err()
        );
        for outcome in ["", "success", "OK"] {
            assert!(
                store
                    .append_operation(&identity(), "状态", outcome, "message", "{}")
                    .is_err()
            );
        }
        for source_message_id in ["bad\nmessage".to_string(), "m".repeat(257)] {
            assert!(
                store
                    .append_operation(&identity(), "状态", "ok", &source_message_id, "{}")
                    .is_err()
            );
        }
        for details in [
            "not-json".to_string(),
            r#"{"raw_event_json":{"message":"secret"}}"#.to_string(),
            r#"{"raw_json":{"message":"secret"}}"#.to_string(),
            r#"{"qimen_raw_event":{"message":"secret"}}"#.to_string(),
            r#"{"image":"base64://AAAA"}"#.to_string(),
            r#"{"image":"data:image/png;base64,AAAA"}"#.to_string(),
            serde_json::to_string(&"x".repeat(8_192)).expect("过长 JSON 应生成"),
            r#"{"password":"secret"}"#.to_string(),
            r#"{"reason":"contains spaces"}"#.to_string(),
            r#"{"duration_ms":600001}"#.to_string(),
        ] {
            assert!(
                store
                    .append_operation(&identity(), "状态", "ok", "message", &details)
                    .is_err(),
                "敏感或无效 details 不应写入：{}",
                details.len()
            );
        }
        for (cursor, limit) in [(None, 0), (None, 101), (Some(-1), 1)] {
            assert!(
                store
                    .list_operation_logs(&identity(), cursor, limit)
                    .is_err()
            );
        }
    }

    #[test]
    fn operation_log_is_append_only_including_replace() {
        let (_directory, store) = test_store();
        let id = store
            .append_operation(&identity(), "状态", "ok", "message-1", "{}")
            .expect("日志应写入");
        let connection = store.open().expect("应打开数据库");
        assert!(
            connection
                .execute(
                    "UPDATE operation_log SET outcome = 'error' WHERE id = ?1",
                    [id]
                )
                .is_err()
        );
        assert!(
            connection
                .execute("DELETE FROM operation_log WHERE id = ?1", [id])
                .is_err()
        );
        assert!(
            connection
                .execute(
                    r#"
                    INSERT OR REPLACE INTO operation_log(
                        id, protocol, account_id, namespace, subject_kind, subject_id,
                        command, outcome, source_message_id, details_json, created_at
                    ) VALUES(?1, 'onebot11', '10001', 'test', 'user', '1875390189',
                             'tampered', 'error', 'message-2', '{}', 2)
                    "#,
                    [id],
                )
                .is_err()
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT command FROM operation_log WHERE id = ?1",
                    [id],
                    |row| row.get::<_, String>(0)
                )
                .expect("原日志应保留"),
            "状态"
        );
    }

    #[test]
    fn recorded_v3_with_malformed_operation_log_schema_fails_closed() {
        let directory = tempdir().expect("应创建测试目录");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("迁移应成功");
        let connection = store.open().expect("应打开数据库");
        connection
            .execute("DROP TRIGGER operation_log_no_reinsert", [])
            .expect("应破坏 v3 结构");
        drop(connection);
        let error = Store::initialize(directory.path(), &DatabaseConfig::default())
            .expect_err("已记录 v3 但操作日志结构损坏必须失败");
        assert!(error.contains("v3") || error.contains("操作日志"));
    }

    #[test]
    fn recorded_v3_with_weakened_trigger_fails_behavior_probe() {
        let directory = tempdir().expect("应创建测试目录");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("迁移应成功");
        let connection = store.open().expect("应打开数据库");
        connection
            .execute_batch(
                r#"
                DROP TRIGGER operation_log_no_update;
                CREATE TRIGGER operation_log_no_update
                BEFORE UPDATE ON operation_log
                WHEN 0
                BEGIN
                    SELECT RAISE(ABORT, 'operation log is append-only');
                END;
                "#,
            )
            .expect("应弱化触发器但保留名称和关键字");
        drop(connection);
        let error = Store::initialize(directory.path(), &DatabaseConfig::default())
            .expect_err("弱化触发器必须被行为探针拒绝");
        assert!(error.contains("探针") || error.contains("UPDATE"));
    }

    #[test]
    fn authorized_context_crud_is_idempotent_paged_and_isolated() {
        let (_directory, store) = test_store();
        let grant_group = OperationLogInput {
            command: "授权上下文",
            outcome: "ok",
            source_message_id: "message-1",
            details_json: r#"{"target_kind":"group","target_id":"group-1"}"#,
        };
        let granted = store
            .grant_authorized_context(&identity(), "group", "group-1", "测试群", &grant_group)
            .expect("群授权应成功");
        let id = match granted {
            AuthorizedContextChange::Granted { id } => id,
            other => panic!("首次授权结果错误：{other:?}"),
        };
        assert_eq!(
            store
                .grant_authorized_context(&identity(), "group", "group-1", "新标签", &grant_group)
                .expect("重复授权应幂等"),
            AuthorizedContextChange::AlreadyGranted { id }
        );
        assert!(
            store
                .is_authorized(&identity(), "group", "group-1")
                .expect("应查询授权")
        );

        let grant_channel = OperationLogInput {
            command: "授权上下文",
            outcome: "ok",
            source_message_id: "message-2",
            details_json: r#"{"target_kind":"channel","target_id":"channel-1"}"#,
        };
        store
            .grant_authorized_context(
                &identity(),
                "channel",
                "channel-1",
                "测试频道",
                &grant_channel,
            )
            .expect("频道授权应成功");
        let first_page = store
            .list_authorized_contexts(&identity(), None, 1)
            .expect("第一页应读取");
        assert_eq!(first_page.entries.len(), 1);
        assert_eq!(first_page.entries[0].label, "测试群");
        assert_eq!(
            first_page.entries[0].granted_by_subject_id,
            identity().subject_id
        );
        assert!(first_page.next_after_id.is_some());
        let second_page = store
            .list_authorized_contexts(&identity(), first_page.next_after_id, 1)
            .expect("第二页应读取");
        assert_eq!(second_page.entries.len(), 1);
        assert_eq!(second_page.entries[0].context_kind, "channel");
        assert_eq!(second_page.next_after_id, None);

        let other_account = IdentityKey {
            account_id: "10002",
            ..identity()
        };
        let other_namespace = IdentityKey {
            namespace: "other",
            ..identity()
        };
        assert!(
            !store
                .is_authorized(&other_account, "group", "group-1")
                .unwrap()
        );
        assert!(
            !store
                .is_authorized(&other_namespace, "group", "group-1")
                .unwrap()
        );
        assert!(
            store
                .list_authorized_contexts(&other_account, None, 100)
                .expect("另一账号查询应成功")
                .entries
                .is_empty()
        );

        let revoke = OperationLogInput {
            command: "撤销授权上下文",
            outcome: "ok",
            source_message_id: "message-3",
            details_json: r#"{"target_kind":"group","target_id":"group-1"}"#,
        };
        assert_eq!(
            store
                .revoke_authorized_context(&identity(), "group", "group-1", &revoke)
                .expect("撤销应成功"),
            AuthorizedContextChange::Revoked { id }
        );
        assert_eq!(
            store
                .revoke_authorized_context(&identity(), "group", "group-1", &revoke)
                .expect("重复撤销应幂等"),
            AuthorizedContextChange::AlreadyRevoked
        );
        assert!(
            !store
                .is_authorized(&identity(), "group", "group-1")
                .unwrap()
        );
    }

    #[test]
    fn authorized_context_validates_fields_and_operation_targets() {
        let (_directory, store) = test_store();
        let valid = OperationLogInput {
            command: "授权上下文",
            outcome: "ok",
            source_message_id: "message",
            details_json: r#"{"target_kind":"group","target_id":"group-1"}"#,
        };
        for (kind, context_id, label) in [
            ("private", "group-1", "标签"),
            ("group", "", "标签"),
            ("group", "bad\nid", "标签"),
            ("group", "group-1", "bad\nlabel"),
        ] {
            assert!(
                store
                    .grant_authorized_context(&identity(), kind, context_id, label, &valid)
                    .is_err()
            );
        }
        assert!(
            store
                .grant_authorized_context(&identity(), "group", &"g".repeat(257), "标签", &valid,)
                .is_err()
        );
        assert!(
            store
                .grant_authorized_context(
                    &identity(),
                    "group",
                    "group-1",
                    &"标".repeat(81),
                    &valid,
                )
                .is_err()
        );
        let mismatched = OperationLogInput {
            details_json: r#"{"target_kind":"channel","target_id":"group-1"}"#,
            ..valid
        };
        assert!(
            store
                .grant_authorized_context(&identity(), "group", "group-1", "标签", &mismatched,)
                .is_err()
        );
        for details_json in [
            r#"{"target_kind":1,"target_id":"group-1"}"#,
            r#"{"target_kind":"group","target_id":1}"#,
            r#"{"target_kind":"group","target_id":"bad\nvalue"}"#,
        ] {
            assert!(
                store
                    .append_operation(&identity(), "授权上下文", "ok", "message", details_json)
                    .is_err()
            );
        }
        assert!(
            store
                .list_authorized_contexts(&identity(), None, 0)
                .is_err()
        );
        assert!(
            store
                .list_authorized_contexts(&identity(), None, 101)
                .is_err()
        );
        assert!(
            store
                .list_authorized_contexts(&identity(), Some(-1), 1)
                .is_err()
        );
    }

    #[test]
    fn authorized_context_and_operation_log_are_atomic() {
        let (_directory, store) = test_store();
        let connection = store.open().expect("应打开数据库");
        connection
            .execute_batch(
                r#"
                CREATE TRIGGER operation_log_test_abort
                BEFORE INSERT ON operation_log
                WHEN NEW.command IN ('授权上下文', '撤销授权上下文')
                BEGIN SELECT RAISE(ABORT, 'test operation failure'); END;
                "#,
            )
            .expect("应安装测试失败触发器");
        drop(connection);
        let grant = OperationLogInput {
            command: "授权上下文",
            outcome: "ok",
            source_message_id: "message-1",
            details_json: r#"{"target_kind":"group","target_id":"group-atomic"}"#,
        };
        assert!(
            store
                .grant_authorized_context(&identity(), "group", "group-atomic", "原子测试", &grant,)
                .is_err()
        );
        assert!(
            !store
                .is_authorized(&identity(), "group", "group-atomic")
                .unwrap()
        );

        let connection = store.open().expect("应重开数据库");
        connection
            .execute("DROP TRIGGER operation_log_test_abort", [])
            .expect("应移除测试失败触发器");
        drop(connection);
        store
            .grant_authorized_context(&identity(), "group", "group-atomic", "原子测试", &grant)
            .expect("移除失败触发器后应授权");
        let connection = store.open().expect("应重开数据库");
        connection
            .execute_batch(
                r#"
                CREATE TRIGGER operation_log_test_abort
                BEFORE INSERT ON operation_log
                WHEN NEW.command = '撤销授权上下文'
                BEGIN SELECT RAISE(ABORT, 'test operation failure'); END;
                "#,
            )
            .expect("应再次安装测试失败触发器");
        drop(connection);
        let revoke = OperationLogInput {
            command: "撤销授权上下文",
            outcome: "ok",
            source_message_id: "message-2",
            details_json: r#"{"target_kind":"group","target_id":"group-atomic"}"#,
        };
        assert!(
            store
                .revoke_authorized_context(&identity(), "group", "group-atomic", &revoke)
                .is_err()
        );
        assert!(
            store
                .is_authorized(&identity(), "group", "group-atomic")
                .unwrap()
        );
    }

    #[test]
    fn recorded_v4_with_bad_schema_or_safe_details_fails_closed() {
        let directory = tempdir().expect("应创建测试目录");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("迁移应成功");
        let connection = store.open().expect("应打开数据库");
        connection
            .execute("DROP INDEX authorized_context_bot_page", [])
            .expect("应破坏 v4 分页索引");
        drop(connection);
        assert!(
            Store::initialize(directory.path(), &DatabaseConfig::default())
                .expect_err("v4 坏 schema 必须拒绝")
                .contains("v4")
        );

        let trigger_directory = tempdir().expect("应创建第二测试目录");
        let trigger_store = Store::initialize(trigger_directory.path(), &DatabaseConfig::default())
            .expect("迁移应成功");
        let connection = trigger_store.open().expect("应打开数据库");
        connection
            .execute_batch(
                r#"
                DROP TRIGGER operation_log_safe_details;
                CREATE TRIGGER operation_log_safe_details
                BEFORE INSERT ON operation_log
                WHEN EXISTS(
                    SELECT 1 FROM json_each(NEW.details_json)
                     WHERE key NOT IN (
                         'context', 'has_args', 'reason', 'duration_ms',
                         'target_kind', 'target_id'
                     )
                )
                BEGIN SELECT RAISE(ABORT, 'operation log details field is not allowed'); END;
                "#,
            )
            .expect("应弱化 target 类型约束");
        drop(connection);
        assert!(Store::initialize(trigger_directory.path(), &DatabaseConfig::default()).is_err());
    }
}
