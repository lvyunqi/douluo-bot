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
        let migration_result = (|| -> Result<bool, String> {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("开始数据库迁移 v2 失败：{error}"))?;
            // The check must happen after BEGIN IMMEDIATE. Otherwise two first-open
            // connections can both observe v2 as missing and one will fail creating
            // the already-created tables after the other commits.
            if migration_applied(&transaction, 2)? {
                transaction
                    .commit()
                    .map_err(|error| format!("确认数据库迁移 v2 失败：{error}"))?;
                return Ok(false);
            }
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
            transaction
                .execute(
                    "INSERT INTO schema_migration(version, applied_at) VALUES(2, ?1)",
                    [now_timestamp()?],
                )
                .map_err(|error| format!("记录数据库迁移 v2 失败：{error}"))?;
            ensure_no_foreign_key_violations(&transaction)?;
            transaction
                .commit()
                .map_err(|error| format!("提交数据库迁移 v2 失败：{error}"))?;
            Ok(true)
        })();
        let restore_result = set_foreign_keys(connection, true);

        match (migration_result, restore_result) {
            (Ok(_), Ok(())) => {
                validate_v2_schema(connection)?;
                Ok(())
            }
            (Err(migration_error), Ok(())) => Err(migration_error),
            (Ok(_), Err(restore_error)) => Err(restore_error),
            (Err(migration_error), Err(restore_error)) => Err(format!(
                "{migration_error}；同时恢复 SQLite 外键约束失败：{restore_error}"
            )),
        }
    }

    pub fn register_player(
        &self,
        key: &IdentityKey<'_>,
        name: &str,
        gender: &str,
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

    pub fn awaken_wuhun(&self, key: &IdentityKey<'_>) -> Result<AwakenedWuhun, String> {
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

    pub fn claim_legacy_identity(
        &self,
        key: &IdentityKey<'_>,
        actor: &LegacyClaimActor<'_>,
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
        transaction
            .commit()
            .map_err(|error| format!("提交旧档认领事务失败：{error}"))?;
        Ok(LegacyClaimResult::Claimed { identity_id })
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
                    "SELECT COUNT(*) FROM schema_migration WHERE version = 2",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("应读取迁移记录"),
            1
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
                    "SELECT COUNT(*) FROM schema_migration WHERE version = 2",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("应读取迁移记录"),
            1
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
}
