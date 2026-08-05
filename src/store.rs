use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

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

#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
    busy_timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityKey<'a> {
    pub protocol: Protocol,
    pub namespace: &'a str,
    pub subject_kind: &'a str,
    pub subject_id: &'a str,
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
        Ok(connection)
    }

    fn migrate(&self, connection: &mut Connection) -> Result<(), String> {
        let transaction = connection
            .transaction()
            .map_err(|error| format!("开始数据库迁移失败：{error}"))?;
        transaction
            .execute_batch(MIGRATION_V1)
            .map_err(|error| format!("执行数据库迁移 v1 失败：{error}"))?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO schema_migration(version, applied_at) VALUES(1, ?1)",
                [now_timestamp()?],
            )
            .map_err(|error| format!("记录数据库迁移失败：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("提交数据库迁移失败：{error}"))
    }

    pub fn register_player(
        &self,
        key: &IdentityKey<'_>,
        name: &str,
        gender: &str,
    ) -> Result<PlayerStatus, String> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("开始注册事务失败：{error}"))?;
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
        let connection = self.open()?;
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
                 WHERE i.protocol = ?1 AND i.namespace = ?2
                   AND i.subject_kind = ?3 AND i.subject_id = ?4
                "#,
                params![
                    key.protocol.as_str(),
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
        let mut connection = self.open()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("开始武魂觉醒事务失败：{error}"))?;
        let player = transaction
            .query_row(
                r#"
                SELECT p.id, p.life_count
                  FROM identity i
                  JOIN player p ON p.identity_id = i.id
                 WHERE i.protocol = ?1 AND i.namespace = ?2
                   AND i.subject_kind = ?3 AND i.subject_id = ?4
                "#,
                params![
                    key.protocol.as_str(),
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
}

fn ensure_identity(transaction: &Transaction<'_>, key: &IdentityKey<'_>) -> Result<i64, String> {
    let timestamp = now_timestamp()?;
    transaction
        .execute(
            r#"
            INSERT OR IGNORE INTO identity(protocol, namespace, subject_kind, subject_id, created_at)
            VALUES(?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                key.protocol.as_str(),
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
             WHERE protocol = ?1 AND namespace = ?2 AND subject_kind = ?3 AND subject_id = ?4
            "#,
            params![
                key.protocol.as_str(),
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
            namespace: "test",
            subject_kind: "user",
            subject_id: "1875390189",
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
    fn separates_protocol_namespaces() {
        let (_directory, store) = test_store();
        let mut official = identity();
        official.protocol = Protocol::QqOfficial;
        store
            .register_player(&identity(), "一号", "男")
            .expect("OneBot 角色应创建");
        store
            .register_player(&official, "二号", "女")
            .expect("官方 QQ 角色应独立创建");
    }
}
