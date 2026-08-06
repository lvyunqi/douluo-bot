use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior, params,
};

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

const MIGRATION_V5: &str = r#"
CREATE TABLE wallet (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    player_id INTEGER NOT NULL REFERENCES player(id) ON DELETE CASCADE,
    currency_code TEXT NOT NULL CHECK(
        length(currency_code) BETWEEN 1 AND 32
        AND currency_code = trim(currency_code)
        AND currency_code GLOB '[A-Za-z0-9._-]*'
        AND currency_code NOT GLOB '*[^A-Za-z0-9._-]*'
    ),
    balance INTEGER NOT NULL DEFAULT 0 CHECK(balance >= 0),
    created_at INTEGER NOT NULL CHECK(created_at >= 0),
    updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
) STRICT;

CREATE UNIQUE INDEX wallet_player_currency
    ON wallet(player_id, currency_code);

CREATE TABLE daily_checkin_claim (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    player_id INTEGER NOT NULL REFERENCES player(id) ON DELETE CASCADE,
    game_day INTEGER NOT NULL CHECK(game_day >= 0),
    total_claims INTEGER NOT NULL CHECK(total_claims >= 1),
    streak_days INTEGER NOT NULL CHECK(streak_days BETWEEN 1 AND total_claims),
    cycle_day INTEGER NOT NULL CHECK(
        cycle_day BETWEEN 1 AND 7
        AND cycle_day = ((streak_days - 1) % 7) + 1
    ),
    exp_reward INTEGER NOT NULL CHECK(
        exp_reward = CASE cycle_day
            WHEN 1 THEN 60 WHEN 2 THEN 70 WHEN 3 THEN 80 WHEN 4 THEN 90
            WHEN 5 THEN 100 WHEN 6 THEN 110 WHEN 7 THEN 150
        END
    ),
    currency_code TEXT NOT NULL CHECK(
        length(currency_code) BETWEEN 1 AND 32
        AND currency_code = trim(currency_code)
        AND currency_code GLOB '[A-Za-z0-9._-]*'
        AND currency_code NOT GLOB '*[^A-Za-z0-9._-]*'
    ),
    currency_reward INTEGER NOT NULL CHECK(currency_reward BETWEEN 100 AND 199),
    exp_after INTEGER NOT NULL CHECK(exp_after >= 0),
    currency_balance_after INTEGER NOT NULL CHECK(currency_balance_after >= 0),
    created_at INTEGER NOT NULL CHECK(created_at >= 0)
) STRICT;

CREATE UNIQUE INDEX daily_checkin_claim_player_day
    ON daily_checkin_claim(player_id, game_day);

CREATE INDEX daily_checkin_claim_player_page
    ON daily_checkin_claim(player_id, id);
"#;

// v1 的 player.level 上限是 100；成长曲线沿用旧项目的神级 120 级上限。
// SQLite 不能直接修改 CHECK 约束，因此通过同一事务重建 player，保留主键和所有角色字段。
const MIGRATION_V6: &str = r#"
CREATE TABLE player_v6 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    identity_id INTEGER NOT NULL UNIQUE REFERENCES identity(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    gender TEXT NOT NULL CHECK(gender IN ('男', '女')),
    level INTEGER NOT NULL DEFAULT 1 CHECK(level BETWEEN 1 AND 120),
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

INSERT INTO player_v6(
    id, identity_id, name, gender, level, exp, hp, max_hp, soul_power,
    max_soul_power, strength, agility, spirit, endurance, perception, luck,
    life_count, state, map_name, created_at, updated_at
)
SELECT
    id, identity_id, name, gender, level, exp, hp, max_hp, soul_power,
    max_soul_power, strength, agility, spirit, endurance, perception, luck,
    life_count, state, map_name, created_at, updated_at
  FROM player;

DROP TABLE player;
ALTER TABLE player_v6 RENAME TO player;
"#;

// v7 将地图拓扑从图片 manifest 和玩家展示文本中分离出来。map_key 是稳定的
// 内容标识，map_edge 保存有向出口；player_map 以外键把玩家位置绑定到地图。
const MIGRATION_V7: &str = r#"
CREATE TABLE map (
    map_key TEXT PRIMARY KEY CHECK(
        length(map_key) BETWEEN 1 AND 96
        AND map_key = trim(map_key)
        AND map_key GLOB '[a-z0-9][a-z0-9._-]*'
        AND map_key NOT GLOB '*[^a-z0-9._-]*'
    ),
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 128),
    description TEXT NOT NULL DEFAULT '' CHECK(length(description) <= 2000),
    level_required INTEGER NOT NULL DEFAULT 1 CHECK(level_required BETWEEN 1 AND 120),
    safe INTEGER NOT NULL DEFAULT 0 CHECK(safe IN (0, 1)),
    pvp_enabled INTEGER NOT NULL DEFAULT 1 CHECK(pvp_enabled IN (0, 1)),
    teleport_enabled INTEGER NOT NULL DEFAULT 0 CHECK(teleport_enabled IN (0, 1)),
    sort_order INTEGER NOT NULL CHECK(sort_order >= 0),
    created_at INTEGER NOT NULL CHECK(created_at >= 0),
    updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
) STRICT;

CREATE UNIQUE INDEX map_name_unique ON map(name);
CREATE UNIQUE INDEX map_sort_order_unique ON map(sort_order);
CREATE INDEX map_page ON map(sort_order, map_key);

CREATE TABLE map_edge (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_map_key TEXT NOT NULL REFERENCES map(map_key) ON DELETE CASCADE,
    to_map_key TEXT NOT NULL REFERENCES map(map_key) ON DELETE RESTRICT,
    travel_kind TEXT NOT NULL CHECK(travel_kind IN ('walk', 'teleport')),
    direction TEXT CHECK(
        (travel_kind = 'walk' AND direction IN ('north', 'south', 'west', 'east'))
        OR (travel_kind = 'teleport' AND direction IS NULL)
    ),
    level_required INTEGER NOT NULL DEFAULT 1 CHECK(level_required BETWEEN 1 AND 120),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
    created_at INTEGER NOT NULL CHECK(created_at >= 0),
    CHECK(from_map_key <> to_map_key)
) STRICT;

CREATE UNIQUE INDEX map_edge_walk_direction
    ON map_edge(from_map_key, direction)
    WHERE travel_kind = 'walk';
CREATE UNIQUE INDEX map_edge_teleport_target
    ON map_edge(from_map_key, to_map_key)
    WHERE travel_kind = 'teleport';
CREATE INDEX map_edge_from_kind
    ON map_edge(from_map_key, travel_kind, enabled, id);

CREATE TABLE player_map (
    player_id INTEGER PRIMARY KEY REFERENCES player(id) ON DELETE CASCADE,
    map_key TEXT NOT NULL REFERENCES map(map_key) ON DELETE RESTRICT,
    updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
) STRICT;

INSERT INTO map(
    map_key, name, description, level_required, safe, pvp_enabled,
    teleport_enabled, sort_order, created_at, updated_at
) VALUES
    ('holy-soul-village', '圣魂村', '新手出生地，宁静而安全的小村庄。', 1, 1, 0, 1, 10, 0, 0),
    ('novice-village', '新手村', '魂师初次历练的村落。', 1, 1, 0, 1, 20, 0, 0),
    ('silves', '西尔维斯', '通往帝国西部的城镇。', 1, 1, 0, 1, 30, 0, 0),
    ('tiandou-imperial-city', '天斗帝国主城', '天斗帝国的繁华主城。', 1, 1, 0, 1, 40, 0, 0),
    ('sunset-forest', '落日森林', '森林深处有魂兽出没，行走需保持警惕。', 1, 0, 1, 1, 50, 0, 0),
    ('star-dou-outer', '星斗外围', '星斗大森林外围，低阶魂兽活动区域。', 1, 0, 1, 0, 60, 0, 0),
    ('star-dou-inner', '星斗中心', '星斗大森林核心区域，强大的魂兽守护着生命之湖。', 20, 0, 1, 0, 70, 0, 0);

-- 方向是有向数据，不通过图片文件名或 manifest 推断；首批种子保留旧版圣魂村出口。
INSERT INTO map_edge(
    from_map_key, to_map_key, travel_kind, direction, level_required, enabled, created_at
) VALUES
    ('holy-soul-village', 'tiandou-imperial-city', 'walk', 'north', 1, 1, 0),
    ('holy-soul-village', 'silves', 'walk', 'south', 1, 1, 0),
    ('holy-soul-village', 'sunset-forest', 'walk', 'west', 1, 1, 0),
    ('holy-soul-village', 'novice-village', 'walk', 'east', 1, 1, 0),
    ('tiandou-imperial-city', 'holy-soul-village', 'walk', 'south', 1, 1, 0),
    ('silves', 'holy-soul-village', 'walk', 'north', 1, 1, 0),
    ('sunset-forest', 'holy-soul-village', 'walk', 'east', 1, 1, 0),
    ('novice-village', 'holy-soul-village', 'walk', 'west', 1, 1, 0),
    ('novice-village', 'star-dou-outer', 'walk', 'south', 1, 1, 0),
    ('star-dou-outer', 'novice-village', 'walk', 'north', 1, 1, 0),
    ('star-dou-outer', 'star-dou-inner', 'walk', 'south', 20, 1, 0),
    ('star-dou-inner', 'star-dou-outer', 'walk', 'north', 20, 1, 0),
    ('holy-soul-village', 'novice-village', 'teleport', NULL, 1, 1, 0),
    ('holy-soul-village', 'silves', 'teleport', NULL, 1, 1, 0),
    ('holy-soul-village', 'tiandou-imperial-city', 'teleport', NULL, 1, 1, 0),
    ('holy-soul-village', 'sunset-forest', 'teleport', NULL, 1, 1, 0),
    ('novice-village', 'holy-soul-village', 'teleport', NULL, 1, 1, 0),
    ('novice-village', 'silves', 'teleport', NULL, 1, 1, 0),
    ('novice-village', 'tiandou-imperial-city', 'teleport', NULL, 1, 1, 0),
    ('novice-village', 'sunset-forest', 'teleport', NULL, 1, 1, 0),
    ('silves', 'holy-soul-village', 'teleport', NULL, 1, 1, 0),
    ('silves', 'novice-village', 'teleport', NULL, 1, 1, 0),
    ('silves', 'tiandou-imperial-city', 'teleport', NULL, 1, 1, 0),
    ('silves', 'sunset-forest', 'teleport', NULL, 1, 1, 0),
    ('tiandou-imperial-city', 'holy-soul-village', 'teleport', NULL, 1, 1, 0),
    ('tiandou-imperial-city', 'novice-village', 'teleport', NULL, 1, 1, 0),
    ('tiandou-imperial-city', 'silves', 'teleport', NULL, 1, 1, 0),
    ('tiandou-imperial-city', 'sunset-forest', 'teleport', NULL, 1, 1, 0),
    ('sunset-forest', 'holy-soul-village', 'teleport', NULL, 1, 1, 0),
    ('sunset-forest', 'novice-village', 'teleport', NULL, 1, 1, 0),
    ('sunset-forest', 'silves', 'teleport', NULL, 1, 1, 0),
    ('sunset-forest', 'tiandou-imperial-city', 'teleport', NULL, 1, 1, 0);

INSERT INTO player_map(player_id, map_key, updated_at)
SELECT p.id, m.map_key, p.updated_at
  FROM player p
  JOIN map m ON m.name = p.map_name;
"#;

// v8 恢复首批旧版物品、NPC 和商店数据。经济写入仍由 Store 在
// BEGIN IMMEDIATE 事务中执行；这里的触发器只负责跨表不变量，避免
// 管理导入或未来功能绕过 max_stack/复活物品不上架规则。
const MIGRATION_V8: &str = r#"
CREATE TABLE item (
    item_key TEXT PRIMARY KEY CHECK(
        length(item_key) BETWEEN 1 AND 96
        AND item_key = trim(item_key)
        AND item_key GLOB '[a-z0-9][a-z0-9._-]*'
        AND item_key NOT GLOB '*[^a-z0-9._-]*'
    ),
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 128),
    category TEXT NOT NULL CHECK(category IN ('revival', 'consumable')),
    quality INTEGER NOT NULL CHECK(quality BETWEEN 1 AND 5),
    stackable INTEGER NOT NULL CHECK(stackable IN (0, 1)),
    max_stack INTEGER NOT NULL CHECK(max_stack BETWEEN 1 AND 9999),
    buy_price INTEGER NOT NULL CHECK(buy_price >= 0),
    sell_price INTEGER NOT NULL CHECK(sell_price >= 0 AND sell_price <= buy_price),
    level_required INTEGER NOT NULL DEFAULT 1 CHECK(level_required BETWEEN 1 AND 120),
    effect_kind TEXT NOT NULL CHECK(effect_kind IN ('revive', 'restore_hp', 'restore_soul')),
    effect_amount INTEGER NOT NULL DEFAULT 0 CHECK(effect_amount >= 0),
    revive_hp_percent INTEGER NOT NULL DEFAULT 0 CHECK(revive_hp_percent BETWEEN 0 AND 100),
    purchasable INTEGER NOT NULL DEFAULT 1 CHECK(purchasable IN (0, 1)),
    sellable INTEGER NOT NULL DEFAULT 1 CHECK(sellable IN (0, 1)),
    usable INTEGER NOT NULL DEFAULT 1 CHECK(usable IN (0, 1)),
    description TEXT NOT NULL DEFAULT '' CHECK(length(description) <= 2000),
    created_at INTEGER NOT NULL CHECK(created_at >= 0),
    updated_at INTEGER NOT NULL CHECK(updated_at >= 0),
    CHECK(
        (stackable = 1 OR max_stack = 1)
        AND (
            (effect_kind = 'revive' AND effect_amount = 0 AND revive_hp_percent > 0)
            OR (effect_kind IN ('restore_hp', 'restore_soul')
                AND effect_amount > 0 AND revive_hp_percent = 0)
        )
    )
) STRICT;

CREATE UNIQUE INDEX item_name_unique ON item(name);

CREATE TABLE inventory (
    player_id INTEGER NOT NULL REFERENCES player(id) ON DELETE CASCADE,
    item_key TEXT NOT NULL REFERENCES item(item_key) ON DELETE RESTRICT,
    quantity INTEGER NOT NULL CHECK(quantity BETWEEN 1 AND 999999),
    updated_at INTEGER NOT NULL CHECK(updated_at >= 0),
    PRIMARY KEY(player_id, item_key)
) STRICT;

CREATE INDEX inventory_player_page ON inventory(player_id, item_key);

CREATE TABLE npc (
    npc_key TEXT PRIMARY KEY CHECK(
        length(npc_key) BETWEEN 1 AND 96
        AND npc_key = trim(npc_key)
        AND npc_key GLOB '[a-z0-9][a-z0-9._-]*'
        AND npc_key NOT GLOB '*[^a-z0-9._-]*'
    ),
    map_key TEXT NOT NULL REFERENCES map(map_key) ON DELETE RESTRICT,
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 128),
    npc_kind TEXT NOT NULL CHECK(npc_kind IN ('elder', 'merchant')),
    dialogue TEXT NOT NULL DEFAULT '' CHECK(length(dialogue) <= 2000),
    description TEXT NOT NULL DEFAULT '' CHECK(length(description) <= 2000),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
    sort_order INTEGER NOT NULL DEFAULT 0 CHECK(sort_order >= 0),
    created_at INTEGER NOT NULL CHECK(created_at >= 0),
    updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
) STRICT;

CREATE UNIQUE INDEX npc_map_name_unique ON npc(map_key, name);
CREATE INDEX npc_map_page ON npc(map_key, enabled, sort_order, npc_key);

CREATE TABLE shop_item (
    npc_key TEXT NOT NULL REFERENCES npc(npc_key) ON DELETE CASCADE,
    item_key TEXT NOT NULL REFERENCES item(item_key) ON DELETE RESTRICT,
    buy_price INTEGER NOT NULL CHECK(buy_price >= 0),
    stock INTEGER NOT NULL DEFAULT -1 CHECK(stock = -1 OR stock >= 0),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
    created_at INTEGER NOT NULL CHECK(created_at >= 0),
    updated_at INTEGER NOT NULL CHECK(updated_at >= 0),
    PRIMARY KEY(npc_key, item_key)
) STRICT;

CREATE INDEX shop_item_npc_page ON shop_item(npc_key, enabled, item_key);

CREATE TABLE player_npc (
    player_id INTEGER PRIMARY KEY REFERENCES player(id) ON DELETE CASCADE,
    npc_key TEXT NOT NULL REFERENCES npc(npc_key) ON DELETE RESTRICT,
    updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
) STRICT;

CREATE TRIGGER inventory_item_stack_insert
BEFORE INSERT ON inventory
WHEN NEW.quantity > COALESCE((SELECT max_stack FROM item WHERE item_key = NEW.item_key), 0)
BEGIN
    SELECT RAISE(ABORT, 'inventory quantity exceeds item max_stack');
END;

CREATE TRIGGER inventory_item_stack_update
BEFORE UPDATE OF item_key, quantity ON inventory
WHEN NEW.quantity > COALESCE((SELECT max_stack FROM item WHERE item_key = NEW.item_key), 0)
BEGIN
    SELECT RAISE(ABORT, 'inventory quantity exceeds item max_stack');
END;

CREATE TRIGGER shop_item_revival_insert
BEFORE INSERT ON shop_item
WHEN NOT EXISTS(SELECT 1 FROM item WHERE item_key = NEW.item_key)
  OR EXISTS(
      SELECT 1 FROM item
       WHERE item_key = NEW.item_key
         AND (
             category = 'revival' OR purchasable = 0 OR usable = 0
             OR NEW.buy_price < sell_price
         )
  )
BEGIN
    SELECT RAISE(ABORT, 'invalid item cannot be listed in a shop');
END;

CREATE TRIGGER shop_item_revival_update
BEFORE UPDATE OF item_key, buy_price ON shop_item
WHEN NOT EXISTS(SELECT 1 FROM item WHERE item_key = NEW.item_key)
  OR EXISTS(
      SELECT 1 FROM item
       WHERE item_key = NEW.item_key
         AND (
             category = 'revival' OR purchasable = 0 OR usable = 0
             OR NEW.buy_price < sell_price
         )
  )
BEGIN
    SELECT RAISE(ABORT, 'invalid item cannot be listed in a shop');
END;

CREATE TRIGGER item_shop_contract_update
BEFORE UPDATE OF category, purchasable, usable, sell_price ON item
WHEN EXISTS(
    SELECT 1 FROM shop_item
     WHERE shop_item.item_key = OLD.item_key
       AND (
           NEW.category = 'revival' OR NEW.purchasable = 0 OR NEW.usable = 0
           OR shop_item.buy_price < NEW.sell_price
       )
)
BEGIN
    SELECT RAISE(ABORT, 'item update would invalidate a shop listing');
END;

CREATE TRIGGER item_inventory_stack_update
BEFORE UPDATE OF max_stack ON item
WHEN EXISTS(
    SELECT 1 FROM inventory
     WHERE inventory.item_key = OLD.item_key
       AND inventory.quantity > NEW.max_stack
)
BEGIN
    SELECT RAISE(ABORT, 'item max_stack is below existing inventory');
END;

INSERT INTO item(
    item_key, name, category, quality, stackable, max_stack,
    buy_price, sell_price, level_required, effect_kind, effect_amount,
    revive_hp_percent, purchasable, sellable, usable, description,
    created_at, updated_at
) VALUES
    ('revival-grass', '复活草', 'revival', 2, 1, 10,
     1000, 500, 1, 'revive', 0, 30, 0, 0, 0,
     '使用后可以复活，恢复30%生命值', 0, 0),
    ('nine-leaf-zhi-grass', '九叶芝草', 'revival', 4, 1, 5,
     10000, 5000, 1, 'revive', 0, 100, 0, 0, 0,
     '传说中的仙草，使用后满血复活', 0, 0),
    ('small-healing-potion', '小回复药', 'consumable', 1, 1, 99,
     10, 2, 1, 'restore_hp', 50, 0, 1, 1, 1,
     '恢复50点生命值', 0, 0),
    ('medium-healing-potion', '中回复药', 'consumable', 2, 1, 99,
     50, 10, 10, 'restore_hp', 200, 0, 1, 1, 1,
     '恢复200点生命值', 0, 0),
    ('soul-power-potion', '魂力恢复药', 'consumable', 2, 1, 99,
     30, 6, 1, 'restore_soul', 100, 0, 1, 1, 1,
     '恢复100点魂力值', 0, 0);

INSERT INTO npc(
    npc_key, map_key, name, npc_kind, dialogue, description,
    enabled, sort_order, created_at, updated_at
) VALUES
    ('holy-soul-village-chief', 'holy-soul-village', '村长', 'elder',
     '年轻人，欢迎来到圣魂村。愿你在魂师之路上平安成长。',
     '圣魂村的村长，可以接待初来乍到的魂师。', 1, 10, 0, 0),
    ('holy-soul-village-grocer', 'holy-soul-village', '杂货商人', 'merchant',
     '看看吧，这里有旅途中用得上的药剂。',
     '出售基础恢复药剂。', 1, 20, 0, 0);

INSERT INTO shop_item(npc_key, item_key, buy_price, stock, enabled, created_at, updated_at)
VALUES
    ('holy-soul-village-grocer', 'small-healing-potion', 10, -1, 1, 0, 0),
    ('holy-soul-village-grocer', 'medium-healing-potion', 50, -1, 1, 0, 0),
    ('holy-soul-village-grocer', 'soul-power-potion', 30, -1, 1, 0, 0);
"#;

// v9 为玩家间资产转移建立显式赠送策略和不可变双边账本。账本触发器只校验
// 身份与审计快照，不直接读取钱包或背包，资产变更仍由 BEGIN IMMEDIATE 事务负责。
const MIGRATION_V9: &str = r#"
CREATE TABLE item_transfer_policy (
    item_key TEXT PRIMARY KEY REFERENCES item(item_key) ON DELETE CASCADE,
    transferable INTEGER NOT NULL DEFAULT 0 CHECK(transferable IN (0, 1)),
    created_at INTEGER NOT NULL CHECK(created_at >= 0),
    updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
) STRICT;

CREATE TABLE asset_transfer (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    protocol TEXT NOT NULL CHECK(protocol IN ('onebot11', 'qq-official')),
    account_id TEXT NOT NULL CHECK(
        length(account_id) BETWEEN 1 AND 128
        AND account_id = trim(account_id)
        AND instr(account_id, char(0)) = 0
        AND account_id NOT GLOB ('*[' || char(1) || '-' || char(31) || char(127) || ']*')
    ),
    namespace TEXT NOT NULL CHECK(
        length(namespace) BETWEEN 1 AND 64
        AND namespace GLOB '[A-Za-z0-9][A-Za-z0-9._-]*'
        AND namespace NOT GLOB '*[^A-Za-z0-9._-]*'
    ),
    sender_identity_id INTEGER NOT NULL REFERENCES identity(id) ON DELETE RESTRICT,
    recipient_identity_id INTEGER NOT NULL REFERENCES identity(id) ON DELETE RESTRICT,
    sender_subject_id TEXT NOT NULL CHECK(
        length(sender_subject_id) BETWEEN 1 AND 256
        AND instr(sender_subject_id, char(0)) = 0
        AND sender_subject_id NOT GLOB ('*[' || char(1) || '-' || char(31) || char(127) || ']*')
    ),
    recipient_subject_id TEXT NOT NULL CHECK(
        length(recipient_subject_id) BETWEEN 1 AND 256
        AND instr(recipient_subject_id, char(0)) = 0
        AND recipient_subject_id NOT GLOB ('*[' || char(1) || '-' || char(31) || char(127) || ']*')
    ),
    asset_kind TEXT NOT NULL CHECK(asset_kind IN ('currency', 'item')),
    currency_code TEXT,
    item_key TEXT REFERENCES item(item_key) ON DELETE RESTRICT,
    amount INTEGER NOT NULL CHECK(amount > 0),
    sender_before INTEGER NOT NULL CHECK(sender_before >= 0),
    sender_after INTEGER NOT NULL CHECK(sender_after >= 0),
    recipient_before INTEGER NOT NULL CHECK(recipient_before >= 0),
    recipient_after INTEGER NOT NULL CHECK(recipient_after >= 0),
    source_message_id TEXT NOT NULL CHECK(
        length(source_message_id) BETWEEN 1 AND 256
        AND instr(source_message_id, char(0)) = 0
        AND source_message_id NOT GLOB ('*[' || char(1) || '-' || char(31) || char(127) || ']*')
    ),
    operation_log_id INTEGER NOT NULL REFERENCES operation_log(id) ON DELETE RESTRICT,
    created_at INTEGER NOT NULL CHECK(created_at >= 0),
    CHECK(sender_identity_id <> recipient_identity_id),
    CHECK(sender_subject_id <> recipient_subject_id),
    CHECK(sender_before >= amount AND sender_after = sender_before - amount),
    CHECK(recipient_after > recipient_before AND recipient_after - recipient_before = amount),
    CHECK(
        (asset_kind = 'currency' AND currency_code = 'gold_soul_coin' AND item_key IS NULL)
        OR (asset_kind = 'item' AND currency_code IS NULL AND item_key IS NOT NULL)
    )
) STRICT;

CREATE UNIQUE INDEX asset_transfer_sender_message
    ON asset_transfer(sender_identity_id, source_message_id);
CREATE UNIQUE INDEX asset_transfer_operation_log
    ON asset_transfer(operation_log_id);
CREATE INDEX asset_transfer_sender_page
    ON asset_transfer(sender_identity_id, id);
CREATE INDEX asset_transfer_recipient_page
    ON asset_transfer(recipient_identity_id, id);

CREATE TRIGGER asset_transfer_no_update
BEFORE UPDATE ON asset_transfer
BEGIN
    SELECT RAISE(ABORT, 'asset transfer is immutable');
END;

CREATE TRIGGER asset_transfer_no_delete
BEFORE DELETE ON asset_transfer
BEGIN
    SELECT RAISE(ABORT, 'asset transfer is immutable');
END;

CREATE TRIGGER asset_transfer_no_reinsert
BEFORE INSERT ON asset_transfer
WHEN EXISTS(
    SELECT 1 FROM asset_transfer
     WHERE sender_identity_id = NEW.sender_identity_id
       AND source_message_id = NEW.source_message_id
)
OR EXISTS(
    SELECT 1 FROM asset_transfer
     WHERE operation_log_id = NEW.operation_log_id
)
BEGIN
    SELECT RAISE(ABORT, 'asset transfer is immutable');
END;

CREATE TRIGGER asset_transfer_scope_guard
BEFORE INSERT ON asset_transfer
WHEN NOT EXISTS(
    SELECT 1
      FROM identity sender
      JOIN identity recipient
      JOIN operation_log audit
     WHERE sender.id = NEW.sender_identity_id
       AND recipient.id = NEW.recipient_identity_id
       AND sender.protocol = NEW.protocol
       AND recipient.protocol = NEW.protocol
       AND sender.account_id = NEW.account_id
       AND recipient.account_id = NEW.account_id
       AND sender.namespace = NEW.namespace
       AND recipient.namespace = NEW.namespace
       AND sender.subject_kind = 'user'
       AND recipient.subject_kind = 'user'
       AND sender.subject_id = NEW.sender_subject_id
       AND recipient.subject_id = NEW.recipient_subject_id
       AND audit.id = NEW.operation_log_id
       AND audit.protocol = NEW.protocol
       AND audit.account_id = NEW.account_id
       AND audit.namespace = NEW.namespace
       AND audit.subject_kind = 'user'
       AND audit.subject_id = NEW.sender_subject_id
       AND (
           (NEW.asset_kind = 'currency' AND audit.command = '转账')
           OR (NEW.asset_kind = 'item' AND audit.command = '发送物品')
       )
       AND audit.outcome = 'ok'
       AND audit.source_message_id = NEW.source_message_id
)
BEGIN
    SELECT RAISE(ABORT, 'asset transfer scope or audit mismatch');
END;

INSERT INTO item_transfer_policy(item_key, transferable, created_at, updated_at) VALUES
    ('revival-grass', 0, 0, 0),
    ('nine-leaf-zhi-grass', 0, 0, 0),
    ('small-healing-potion', 1, 0, 0),
    ('medium-healing-potion', 1, 0, 0),
    ('soul-power-potion', 1, 0, 0);
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
pub struct MapRecord {
    pub map_key: String,
    pub name: String,
    pub description: String,
    pub level_required: i64,
    pub safe: bool,
    pub pvp_enabled: bool,
    pub teleport_enabled: bool,
    pub sort_order: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapExit {
    pub direction: Option<String>,
    pub travel_kind: String,
    pub target: MapRecord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapPage {
    pub entries: Vec<MapRecord>,
    pub page: usize,
    pub page_count: usize,
    pub total: usize,
    pub next_after_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapTravelReceipt {
    pub from: MapRecord,
    pub to: MapRecord,
    pub travel_kind: String,
    pub direction: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemRecord {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryEntry {
    pub item: ItemRecord,
    pub quantity: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryPage {
    pub entries: Vec<InventoryEntry>,
    pub page: usize,
    pub page_count: usize,
    pub total: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NpcRecord {
    pub npc_key: String,
    pub map_key: String,
    pub map_name: String,
    pub name: String,
    pub npc_kind: String,
    pub dialogue: String,
    pub description: String,
    pub has_shop: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NpcPage {
    pub entries: Vec<NpcRecord>,
    pub map_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShopItemEntry {
    pub npc_key: String,
    pub npc_name: String,
    pub item: ItemRecord,
    pub price: i64,
    pub stock: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShopPage {
    pub npc: NpcRecord,
    pub entries: Vec<ShopItemEntry>,
    pub page: usize,
    pub page_count: usize,
    pub total: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurchaseReceipt {
    pub npc_name: String,
    pub item: ItemRecord,
    pub quantity: i64,
    pub total_price: i64,
    pub balance_after: i64,
    pub inventory_after: i64,
    pub stock_after: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaleReceipt {
    pub npc_name: String,
    pub item: ItemRecord,
    pub quantity: i64,
    pub total_price: i64,
    pub balance_after: i64,
    pub inventory_after: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UseItemReceipt {
    pub item: ItemRecord,
    pub consumed: bool,
    pub inventory_after: i64,
    pub hp_before: i64,
    pub hp_after: i64,
    pub max_hp: i64,
    pub soul_power_before: i64,
    pub soul_power_after: i64,
    pub max_soul_power: i64,
    pub state_before: String,
    pub state_after: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrencyTransferReceipt {
    pub transfer_id: i64,
    pub recipient_subject_id: String,
    pub currency_code: String,
    pub amount: i64,
    pub sender_balance_after: i64,
    pub recipient_balance_after: i64,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemGiftReceipt {
    pub transfer_id: i64,
    pub recipient_subject_id: String,
    pub item: ItemRecord,
    pub quantity: i64,
    pub sender_inventory_after: i64,
    pub recipient_inventory_after: i64,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TransferParticipant {
    identity_id: i64,
    player_id: i64,
    subject_id: String,
    state: String,
    awakened: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AssetTransferRecord {
    id: i64,
    recipient_subject_id: String,
    asset_kind: String,
    currency_code: Option<String>,
    item_key: Option<String>,
    amount: i64,
    sender_after: i64,
    recipient_after: i64,
}

#[derive(Clone, Copy, Debug)]
struct AssetTransferInsert<'a> {
    asset_kind: &'a str,
    currency_code: Option<&'a str>,
    item_key: Option<&'a str>,
    amount: i64,
    sender_before: i64,
    sender_after: i64,
    recipient_before: i64,
    recipient_after: i64,
    source_message_id: &'a str,
    operation_log_id: i64,
    created_at: i64,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DailyCheckinInput<'a> {
    pub game_day: i64,
    pub currency_code: &'a str,
    pub currency_reward_override: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DailyCheckinReceipt {
    pub game_day: i64,
    pub total_claims: i64,
    pub streak_days: i64,
    pub cycle_day: i64,
    pub exp_reward: i64,
    pub level_before: i64,
    pub level_after: i64,
    pub levels_gained: i64,
    pub title_after: String,
    pub currency_code: String,
    pub currency_reward: i64,
    pub exp_after: i64,
    pub currency_balance_after: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DailyCheckinResult {
    Claimed(DailyCheckinReceipt),
    AlreadyClaimed(DailyCheckinReceipt),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExperienceGrantReceipt {
    pub amount: i64,
    pub exp_before: i64,
    pub exp_after: i64,
    pub level_before: i64,
    pub level_after: i64,
    pub levels_gained: i64,
    pub title_before: String,
    pub title_after: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExperienceProgress {
    pub level: i64,
    pub title: &'static str,
    pub total_exp: i64,
    pub exp_in_level: i64,
    pub exp_for_next: Option<i64>,
    pub total_exp_for_next: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LevelTier {
    min_level: i64,
    max_level: i64,
    title: &'static str,
    base_exp: i64,
}

pub const MAX_PLAYER_LEVEL: i64 = 120;
pub const GOLD_SOUL_COIN: &str = "gold_soul_coin";

const LEVEL_TIERS: [LevelTier; 14] = [
    LevelTier {
        min_level: 1,
        max_level: 10,
        title: "魂士",
        base_exp: 100,
    },
    LevelTier {
        min_level: 11,
        max_level: 20,
        title: "魂师",
        base_exp: 500,
    },
    LevelTier {
        min_level: 21,
        max_level: 30,
        title: "大魂师",
        base_exp: 1_500,
    },
    LevelTier {
        min_level: 31,
        max_level: 40,
        title: "魂尊",
        base_exp: 4_000,
    },
    LevelTier {
        min_level: 41,
        max_level: 50,
        title: "魂宗",
        base_exp: 10_000,
    },
    LevelTier {
        min_level: 51,
        max_level: 60,
        title: "魂王",
        base_exp: 25_000,
    },
    LevelTier {
        min_level: 61,
        max_level: 70,
        title: "魂帝",
        base_exp: 60_000,
    },
    LevelTier {
        min_level: 71,
        max_level: 80,
        title: "魂圣",
        base_exp: 150_000,
    },
    LevelTier {
        min_level: 81,
        max_level: 90,
        title: "魂斗罗",
        base_exp: 400_000,
    },
    LevelTier {
        min_level: 91,
        max_level: 99,
        title: "封号斗罗",
        base_exp: 1_000_000,
    },
    LevelTier {
        min_level: 100,
        max_level: 100,
        title: "半神",
        base_exp: 3_000_000,
    },
    LevelTier {
        min_level: 101,
        max_level: 105,
        title: "一级神祇",
        base_exp: 10_000_000,
    },
    LevelTier {
        min_level: 106,
        max_level: 110,
        title: "神王",
        base_exp: 50_000_000,
    },
    LevelTier {
        min_level: 111,
        max_level: 120,
        title: "至高神",
        base_exp: 200_000_000,
    },
];

fn level_tier(level: i64) -> Result<LevelTier, String> {
    LEVEL_TIERS
        .iter()
        .copied()
        .find(|tier| (tier.min_level..=tier.max_level).contains(&level))
        .ok_or_else(|| format!("角色等级必须在 1 到 {MAX_PLAYER_LEVEL} 之间"))
}

/// 返回某一级升到下一级所需的经验；经验字段采用累计经验语义。
pub fn level_exp_required(level: i64) -> Result<Option<i64>, String> {
    if level == MAX_PLAYER_LEVEL {
        return Ok(None);
    }
    let tier = level_tier(level)?;
    let offset = level
        .checked_sub(tier.min_level)
        .ok_or_else(|| "等级经验计算溢出".to_string())?;
    let multiplier = 10_i64
        .checked_add(offset)
        .ok_or_else(|| "等级经验计算溢出".to_string())?;
    let scaled = tier
        .base_exp
        .checked_mul(multiplier)
        .ok_or_else(|| "等级经验计算溢出".to_string())?;
    Ok(Some(scaled / 10))
}

/// 返回达到指定等级所需的累计经验（1 级角色为 0）。
pub fn total_exp_for_level(level: i64) -> Result<i64, String> {
    if !(1..=MAX_PLAYER_LEVEL).contains(&level) {
        return Err(format!("角色等级必须在 1 到 {MAX_PLAYER_LEVEL} 之间"));
    }
    let mut total = 0_i64;
    for current in 1..level {
        total = total
            .checked_add(
                level_exp_required(current)?.ok_or_else(|| "满级不应存在下一级经验".to_string())?,
            )
            .ok_or_else(|| "等级累计经验计算溢出".to_string())?;
    }
    Ok(total)
}

/// 根据累计经验计算最高可达到的等级；达到 120 级后不再继续升级。
pub fn level_for_total_exp(total_exp: i64) -> Result<i64, String> {
    if total_exp < 0 {
        return Err("累计经验不能为负数".to_string());
    }
    let mut level = 1_i64;
    while level < MAX_PLAYER_LEVEL {
        let next_total = total_exp_for_level(level + 1)?;
        if total_exp < next_total {
            break;
        }
        level += 1;
    }
    Ok(level)
}

pub fn level_title(level: i64) -> Result<&'static str, String> {
    Ok(level_tier(level)?.title)
}

pub fn full_level_title(level: i64) -> Result<String, String> {
    Ok(format!("{level}级{}", level_title(level)?))
}

pub fn experience_progress(level: i64, total_exp: i64) -> Result<ExperienceProgress, String> {
    if total_exp < 0 {
        return Err("累计经验不能为负数".to_string());
    }
    let derived_level = level_for_total_exp(total_exp)?;
    let effective_level = level.max(derived_level).min(MAX_PLAYER_LEVEL);
    let level_start = total_exp_for_level(effective_level)?;
    let exp_in_level = total_exp.saturating_sub(level_start);
    let exp_for_next = level_exp_required(effective_level)?;
    let total_exp_for_next = effective_level
        .checked_add(1)
        .filter(|next| *next <= MAX_PLAYER_LEVEL)
        .map(total_exp_for_level)
        .transpose()?;
    Ok(ExperienceProgress {
        level: effective_level,
        title: level_title(effective_level)?,
        total_exp,
        exp_in_level,
        exp_for_next,
        total_exp_for_next,
    })
}

fn apply_experience_in_transaction(
    transaction: &Transaction<'_>,
    player_id: i64,
    level_before: i64,
    exp_before: i64,
    amount: i64,
    timestamp: i64,
) -> Result<ExperienceGrantReceipt, String> {
    if amount < 0 {
        return Err("获得经验不能为负数".to_string());
    }
    if exp_before < 0 {
        return Err("角色累计经验不能为负数".to_string());
    }
    level_tier(level_before)?;
    let exp_after = exp_before
        .checked_add(amount)
        .ok_or_else(|| "经验累加溢出".to_string())?;
    let level_after = level_before.max(level_for_total_exp(exp_after)?);
    let levels_gained = level_after
        .checked_sub(level_before)
        .ok_or_else(|| "角色等级计算溢出".to_string())?;
    let player_updates = transaction
        .execute(
            "UPDATE player SET level = ?1, exp = ?2, updated_at = ?3 WHERE id = ?4",
            params![level_after, exp_after, timestamp, player_id],
        )
        .map_err(|error| format!("更新角色经验失败：{error}"))?;
    if player_updates != 1 {
        return Err("更新角色经验时角色状态发生变化".to_string());
    }
    Ok(ExperienceGrantReceipt {
        amount,
        exp_before,
        exp_after,
        level_before,
        level_after,
        levels_gained,
        title_before: full_level_title(level_before)?,
        title_after: full_level_title(level_after)?,
    })
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
        store.ensure_wal_mode(&connection)?;
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
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|error| format!("初始化 SQLite PRAGMA 失败：{error}"))?;
        verify_foreign_keys(&connection, true)?;
        Ok(connection)
    }

    fn ensure_wal_mode(&self, connection: &Connection) -> Result<(), String> {
        let deadline = Instant::now() + self.busy_timeout;
        loop {
            match connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
                row.get::<_, String>(0)
            }) {
                Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
                Ok(mode) => return Err(format!("SQLite 未进入 WAL 模式，当前模式为 {mode}")),
                Err(error) if sqlite_is_busy_or_locked(&error) && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(format!("启用 SQLite WAL 模式失败：{error}")),
            }
        }
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
                .map_err(|error| format!("开始数据库迁移 v2/v3/v4/v5/v6/v7/v8/v9 失败：{error}"))?;
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

            if !migration_applied(&transaction, 5)? {
                transaction
                    .execute_batch(MIGRATION_V5)
                    .map_err(|error| format!("执行数据库迁移 v5 失败：{error}"))?;
                validate_v5_schema(&transaction)?;
                transaction
                    .execute(
                        "INSERT INTO schema_migration(version, applied_at) VALUES(5, ?1)",
                        [now_timestamp()?],
                    )
                    .map_err(|error| format!("记录数据库迁移 v5 失败：{error}"))?;
            } else {
                validate_v5_schema(&transaction)?;
            }

            if !migration_applied(&transaction, 6)? {
                validate_v6_source_player_schema(&transaction)?;
                let old_player_sequence = transaction
                    .query_row(
                        "SELECT seq FROM sqlite_sequence WHERE name = 'player'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
                    .map_err(|error| format!("读取旧角色序列失败：{error}"))?;
                transaction
                    .execute_batch(MIGRATION_V6)
                    .map_err(|error| format!("执行数据库迁移 v6 失败：{error}"))?;
                if let Some(sequence) = old_player_sequence {
                    restore_player_sequence(&transaction, sequence)?;
                }
                validate_v6_schema(&transaction)?;
                transaction
                    .execute(
                        "INSERT INTO schema_migration(version, applied_at) VALUES(6, ?1)",
                        [now_timestamp()?],
                    )
                    .map_err(|error| format!("记录数据库迁移 v6 失败：{error}"))?;
            } else {
                validate_v6_schema(&transaction)?;
            }

            if !migration_applied(&transaction, 7)? {
                transaction
                    .execute_batch(MIGRATION_V7)
                    .map_err(|error| format!("执行数据库迁移 v7 失败：{error}"))?;
                validate_v7_schema(&transaction)?;
                transaction
                    .execute(
                        "INSERT INTO schema_migration(version, applied_at) VALUES(7, ?1)",
                        [now_timestamp()?],
                    )
                    .map_err(|error| format!("记录数据库迁移 v7 失败：{error}"))?;
            } else {
                validate_v7_schema(&transaction)?;
            }

            if !migration_applied(&transaction, 8)? {
                transaction
                    .execute_batch(MIGRATION_V8)
                    .map_err(|error| format!("执行数据库迁移 v8 失败：{error}"))?;
                validate_v8_schema(&transaction)?;
                transaction
                    .execute(
                        "INSERT INTO schema_migration(version, applied_at) VALUES(8, ?1)",
                        [now_timestamp()?],
                    )
                    .map_err(|error| format!("记录数据库迁移 v8 失败：{error}"))?;
            } else {
                validate_v8_schema(&transaction)?;
            }

            if !migration_applied(&transaction, 9)? {
                transaction
                    .execute_batch(MIGRATION_V9)
                    .map_err(|error| format!("执行数据库迁移 v9 失败：{error}"))?;
                validate_v9_schema(&transaction)?;
                transaction
                    .execute(
                        "INSERT INTO schema_migration(version, applied_at) VALUES(9, ?1)",
                        [now_timestamp()?],
                    )
                    .map_err(|error| format!("记录数据库迁移 v9 失败：{error}"))?;
            } else {
                validate_v9_schema(&transaction)?;
            }

            ensure_no_foreign_key_violations(&transaction)?;
            transaction
                .commit()
                .map_err(|error| format!("提交数据库迁移 v2/v3/v4/v5/v6/v7/v8/v9 失败：{error}"))?;
            Ok(())
        })();
        let restore_result = set_foreign_keys(connection, true);

        match (migration_result, restore_result) {
            (Ok(()), Ok(())) => {
                validate_v2_schema(connection)?;
                validate_v3_schema(connection)?;
                validate_v4_schema(connection)?;
                validate_v5_schema(connection)?;
                validate_v6_schema(connection)?;
                validate_v7_schema(connection)?;
                validate_v8_schema(connection)?;
                validate_v9_schema(connection)?;
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
        let player_id = transaction.last_insert_rowid();
        transaction
            .execute(
                "INSERT INTO player_map(player_id, map_key, updated_at) VALUES(?1, 'holy-soul-village', ?2)",
                params![player_id, timestamp],
            )
            .map_err(|error| format!("设置角色初始地图失败：{error}"))?;
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
                       p.soul_power, p.max_soul_power, COALESCE(m.name, p.map_name),
                       p.life_count, p.state,
                       w.name, w.category
                  FROM identity i
                  JOIN player p ON p.identity_id = i.id
             LEFT JOIN player_map pm ON pm.player_id = p.id
             LEFT JOIN map m ON m.map_key = pm.map_key
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

    pub fn current_map(&self, key: &IdentityKey<'_>) -> Result<Option<MapRecord>, String> {
        validate_identity_key(key)?;
        let connection = self.open()?;
        ensure_no_legacy_identity(&connection, key)?;
        connection
            .query_row(
                r#"
                SELECT m.map_key, m.name, m.description, m.level_required,
                       m.safe, m.pvp_enabled, m.teleport_enabled, m.sort_order
                  FROM identity i
                  JOIN player p ON p.identity_id = i.id
                  JOIN player_map pm ON pm.player_id = p.id
                  JOIN map m ON m.map_key = pm.map_key
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
                map_record_from_row,
            )
            .optional()
            .map_err(|error| format!("查询当前地图失败：{error}"))
    }

    pub fn map_exits(&self, key: &IdentityKey<'_>) -> Result<Vec<MapExit>, String> {
        validate_identity_key(key)?;
        let connection = self.open()?;
        ensure_no_legacy_identity(&connection, key)?;
        let mut statement = connection
            .prepare(
                r#"
                SELECT e.direction, e.travel_kind,
                       m.map_key, m.name, m.description, m.level_required,
                       m.safe, m.pvp_enabled, m.teleport_enabled, m.sort_order
                  FROM identity i
                  JOIN player p ON p.identity_id = i.id
                  JOIN player_map pm ON pm.player_id = p.id
                  JOIN map_edge e
                    ON e.from_map_key = pm.map_key AND e.enabled = 1
                  JOIN map m ON m.map_key = e.to_map_key
                 WHERE i.protocol = ?1 AND i.account_id = ?2 AND i.namespace = ?3
                   AND i.subject_kind = ?4 AND i.subject_id = ?5
                 ORDER BY CASE e.travel_kind WHEN 'walk' THEN 0 ELSE 1 END,
                          CASE e.direction
                              WHEN 'north' THEN 0 WHEN 'south' THEN 1
                              WHEN 'west' THEN 2 WHEN 'east' THEN 3 ELSE 4 END,
                          m.sort_order, m.map_key
                "#,
            )
            .map_err(|error| format!("准备地图出口查询失败：{error}"))?;
        statement
            .query_map(
                params![
                    key.protocol.as_str(),
                    key.account_id,
                    key.namespace,
                    key.subject_kind,
                    key.subject_id
                ],
                |row| {
                    Ok(MapExit {
                        direction: row.get(0)?,
                        travel_kind: row.get(1)?,
                        target: MapRecord {
                            map_key: row.get(2)?,
                            name: row.get(3)?,
                            description: row.get(4)?,
                            level_required: row.get(5)?,
                            safe: row.get(6)?,
                            pvp_enabled: row.get(7)?,
                            teleport_enabled: row.get(8)?,
                            sort_order: row.get(9)?,
                        },
                    })
                },
            )
            .map_err(|error| format!("查询地图出口失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("解析地图出口失败：{error}"))
    }

    pub fn list_maps_page(&self, page: usize, limit: usize) -> Result<MapPage, String> {
        validate_map_page(page, limit)?;
        let connection = self.open()?;
        let total = connection
            .query_row("SELECT COUNT(*) FROM map", [], |row| row.get::<_, i64>(0))
            .map_err(|error| format!("统计地图数量失败：{error}"))?;
        let total = usize::try_from(total).map_err(|_| "地图数量超出可分页范围".to_string())?;
        let page_count = total.div_ceil(limit).max(1);
        if page > page_count {
            return Err(format!("地图页码必须在 1 到 {page_count} 之间"));
        }
        let offset = page
            .checked_sub(1)
            .and_then(|page| page.checked_mul(limit))
            .ok_or_else(|| "地图分页偏移量溢出".to_string())?;
        let fetch_limit = i64::try_from(limit).map_err(|_| "地图分页数量无法转换".to_string())?;
        let offset = i64::try_from(offset).map_err(|_| "地图分页偏移量无法转换".to_string())?;
        let mut statement = connection
            .prepare(
                r#"
                SELECT map_key, name, description, level_required,
                       safe, pvp_enabled, teleport_enabled, sort_order
                  FROM map
                 ORDER BY sort_order, map_key
                 LIMIT ?1 OFFSET ?2
                "#,
            )
            .map_err(|error| format!("准备地图分页查询失败：{error}"))?;
        let entries = statement
            .query_map(params![fetch_limit, offset], map_record_from_row)
            .map_err(|error| format!("查询地图分页失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("解析地图分页失败：{error}"))?;
        let next_after_key = (page < page_count)
            .then(|| entries.last().map(|entry| entry.map_key.clone()))
            .flatten();
        Ok(MapPage {
            entries,
            page,
            page_count,
            total,
            next_after_key,
        })
    }

    pub fn move_direction_with_operation(
        &self,
        key: &IdentityKey<'_>,
        direction: &str,
        operation: &OperationLogInput<'_>,
    ) -> Result<MapTravelReceipt, String> {
        let direction = normalize_map_direction(direction)?;
        validate_identity_key(key)?;
        validate_operation_input(operation)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始移动事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        let (player_id, level, from) = load_player_map_for_identity(&transaction, key)?;
        let (to, edge_level) = transaction
            .query_row(
                r#"
                SELECT m.map_key, m.name, m.description, m.level_required,
                       m.safe, m.pvp_enabled, m.teleport_enabled, m.sort_order,
                       e.level_required
                  FROM map_edge e
                  JOIN map m ON m.map_key = e.to_map_key
                 WHERE e.from_map_key = ?1 AND e.travel_kind = 'walk'
                   AND e.direction = ?2 AND e.enabled = 1
                "#,
                params![from.map_key, direction],
                |row| {
                    Ok((
                        MapRecord {
                            map_key: row.get(0)?,
                            name: row.get(1)?,
                            description: row.get(2)?,
                            level_required: row.get(3)?,
                            safe: row.get(4)?,
                            pvp_enabled: row.get(5)?,
                            teleport_enabled: row.get(6)?,
                            sort_order: row.get(7)?,
                        },
                        row.get::<_, i64>(8)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("查询地图方向出口失败：{error}"))?
            .ok_or_else(|| format!("当前地图没有向{}的道路", direction_name(direction)))?;
        let required_level = to.level_required.max(edge_level);
        if level < required_level {
            return Err(format!(
                "前往{}需要{}级，当前为{}级",
                to.name, required_level, level
            ));
        }
        update_player_map(&transaction, player_id, &to)?;
        insert_operation_log(&transaction, key, operation)?;
        transaction
            .commit()
            .map_err(|error| format!("提交移动事务失败：{error}"))?;
        Ok(MapTravelReceipt {
            from,
            to,
            travel_kind: "walk".to_string(),
            direction: Some(direction.to_string()),
        })
    }

    pub fn teleport_with_operation(
        &self,
        key: &IdentityKey<'_>,
        target_name: Option<&str>,
        operation: &OperationLogInput<'_>,
    ) -> Result<MapTravelReceipt, String> {
        validate_identity_key(key)?;
        validate_operation_input(operation)?;
        let target_name = target_name.map(str::trim).filter(|name| !name.is_empty());
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始传送事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        let (player_id, level, from) = load_player_map_for_identity(&transaction, key)?;
        if !from.teleport_enabled {
            return Err("当前地图没有传送阵，无法传送".to_string());
        }
        let (to, edge_level) = if let Some(target_name) = target_name {
            transaction
                .query_row(
                    r#"
                    SELECT m.map_key, m.name, m.description, m.level_required,
                           m.safe, m.pvp_enabled, m.teleport_enabled, m.sort_order,
                           e.level_required
                      FROM map_edge e
                     JOIN map m ON m.map_key = e.to_map_key
                     WHERE e.from_map_key = ?1 AND e.travel_kind = 'teleport'
                       AND e.enabled = 1 AND m.teleport_enabled = 1
                       AND (m.name = ?2 OR m.map_key = ?2)
                    "#,
                    params![from.map_key, target_name],
                    |row| Ok((map_record_from_row(row)?, row.get::<_, i64>(8)?)),
                )
                .optional()
                .map_err(|error| format!("查询传送目标失败：{error}"))?
                .ok_or_else(|| "目标地图不在当前传送阵范围内".to_string())?
        } else {
            transaction
                .query_row(
                    r#"
                    SELECT m.map_key, m.name, m.description, m.level_required,
                           m.safe, m.pvp_enabled, m.teleport_enabled, m.sort_order,
                           e.level_required
                      FROM map_edge e
                     JOIN map m ON m.map_key = e.to_map_key
                     WHERE e.from_map_key = ?1 AND e.travel_kind = 'teleport'
                       AND e.enabled = 1 AND m.teleport_enabled = 1
                     ORDER BY random()
                     LIMIT 1
                    "#,
                    [from.map_key.as_str()],
                    |row| Ok((map_record_from_row(row)?, row.get::<_, i64>(8)?)),
                )
                .optional()
                .map_err(|error| format!("选择随机传送目标失败：{error}"))?
                .ok_or_else(|| "当前传送阵没有可用目标".to_string())?
        };
        let required_level = to.level_required.max(edge_level);
        if level < required_level {
            return Err(format!(
                "前往{}需要{}级，当前为{}级",
                to.name, required_level, level
            ));
        }
        update_player_map(&transaction, player_id, &to)?;
        insert_operation_log(&transaction, key, operation)?;
        transaction
            .commit()
            .map_err(|error| format!("提交传送事务失败：{error}"))?;
        Ok(MapTravelReceipt {
            from,
            to,
            travel_kind: "teleport".to_string(),
            direction: None,
        })
    }

    pub fn wallet_balance(
        &self,
        key: &IdentityKey<'_>,
        currency_code: &str,
    ) -> Result<Option<i64>, String> {
        validate_identity_key(key)?;
        validate_currency_code(currency_code)?;
        let connection = self.open()?;
        ensure_no_legacy_identity(&connection, key)?;
        connection
            .query_row(
                r#"
                SELECT COALESCE(w.balance, 0)
                  FROM identity i
                  JOIN player p ON p.identity_id = i.id
             LEFT JOIN wallet w
                    ON w.player_id = p.id AND w.currency_code = ?6
                 WHERE i.protocol = ?1 AND i.account_id = ?2 AND i.namespace = ?3
                   AND i.subject_kind = ?4 AND i.subject_id = ?5
                "#,
                params![
                    key.protocol.as_str(),
                    key.account_id,
                    key.namespace,
                    key.subject_kind,
                    key.subject_id,
                    currency_code
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("查询钱包余额失败：{error}"))
    }

    pub fn transfer_gold_with_operation(
        &self,
        key: &IdentityKey<'_>,
        recipient_subject_id: &str,
        amount: i64,
        operation: &OperationLogInput<'_>,
    ) -> Result<CurrencyTransferReceipt, String> {
        validate_identity_key(key)?;
        validate_transfer_recipient(key, recipient_subject_id)?;
        validate_transfer_operation(operation, "转账")?;
        if amount <= 0 {
            return Err("转账金额必须大于 0".to_string());
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始钱包转账事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        let sender = load_transfer_sender(&transaction, key)?;
        if let Some(existing) = load_asset_transfer_by_message(
            &transaction,
            sender.identity_id,
            operation.source_message_id,
        )? {
            if existing.asset_kind != "currency"
                || existing.currency_code.as_deref() != Some(GOLD_SOUL_COIN)
                || existing.item_key.is_some()
                || existing.recipient_subject_id != recipient_subject_id
                || existing.amount != amount
            {
                return Err("该消息 ID 已用于不同的资产转移请求，拒绝重复执行".to_string());
            }
            return Ok(CurrencyTransferReceipt {
                transfer_id: existing.id,
                recipient_subject_id: existing.recipient_subject_id,
                currency_code: GOLD_SOUL_COIN.to_string(),
                amount: existing.amount,
                sender_balance_after: existing.sender_after,
                recipient_balance_after: existing.recipient_after,
                replayed: true,
            });
        }

        ensure_transfer_participant_eligible(&sender, "你")?;
        let recipient = load_transfer_recipient(&transaction, key, recipient_subject_id)?;
        ensure_transfer_participant_eligible(&recipient, "对方")?;
        let timestamp = now_timestamp()?;
        ensure_wallet(&transaction, sender.player_id, GOLD_SOUL_COIN, timestamp)?;
        ensure_wallet(&transaction, recipient.player_id, GOLD_SOUL_COIN, timestamp)?;
        let sender_before =
            wallet_balance_in_transaction(&transaction, sender.player_id, GOLD_SOUL_COIN)?;
        if sender_before < amount {
            return Err(format!(
                "金魂币余额不足：需要 {amount}，当前 {sender_before}"
            ));
        }
        let recipient_before =
            wallet_balance_in_transaction(&transaction, recipient.player_id, GOLD_SOUL_COIN)?;
        let sender_after = sender_before
            .checked_sub(amount)
            .ok_or_else(|| "转账后发送方余额下溢".to_string())?;
        let recipient_after = recipient_before
            .checked_add(amount)
            .ok_or_else(|| "转账后接收方余额溢出".to_string())?;
        update_wallet_balance(
            &transaction,
            sender.player_id,
            GOLD_SOUL_COIN,
            sender_after,
            timestamp,
        )?;
        update_wallet_balance(
            &transaction,
            recipient.player_id,
            GOLD_SOUL_COIN,
            recipient_after,
            timestamp,
        )?;
        let operation_log_id = insert_operation_log(&transaction, key, operation)?;
        let transfer_id = insert_asset_transfer(
            &transaction,
            key,
            &sender,
            &recipient,
            AssetTransferInsert {
                asset_kind: "currency",
                currency_code: Some(GOLD_SOUL_COIN),
                item_key: None,
                amount,
                sender_before,
                sender_after,
                recipient_before,
                recipient_after,
                source_message_id: operation.source_message_id,
                operation_log_id,
                created_at: timestamp,
            },
        )?;
        transaction
            .commit()
            .map_err(|error| format!("提交钱包转账事务失败：{error}"))?;
        Ok(CurrencyTransferReceipt {
            transfer_id,
            recipient_subject_id: recipient.subject_id,
            currency_code: GOLD_SOUL_COIN.to_string(),
            amount,
            sender_balance_after: sender_after,
            recipient_balance_after: recipient_after,
            replayed: false,
        })
    }

    pub fn gift_item_with_operation(
        &self,
        key: &IdentityKey<'_>,
        recipient_subject_id: &str,
        item_name_or_key: &str,
        quantity: i64,
        operation: &OperationLogInput<'_>,
    ) -> Result<ItemGiftReceipt, String> {
        validate_identity_key(key)?;
        validate_transfer_recipient(key, recipient_subject_id)?;
        validate_catalog_lookup(item_name_or_key, "物品名称")?;
        validate_trade_quantity(quantity)?;
        validate_transfer_operation(operation, "发送物品")?;

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始物品赠送事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        let sender = load_transfer_sender(&transaction, key)?;
        let item = load_transferable_item(&transaction, item_name_or_key)?;
        if let Some(existing) = load_asset_transfer_by_message(
            &transaction,
            sender.identity_id,
            operation.source_message_id,
        )? {
            if existing.asset_kind != "item"
                || existing.currency_code.is_some()
                || existing.item_key.as_deref() != Some(item.item_key.as_str())
                || existing.recipient_subject_id != recipient_subject_id
                || existing.amount != quantity
            {
                return Err("该消息 ID 已用于不同的资产转移请求，拒绝重复执行".to_string());
            }
            return Ok(ItemGiftReceipt {
                transfer_id: existing.id,
                recipient_subject_id: existing.recipient_subject_id,
                item,
                quantity: existing.amount,
                sender_inventory_after: existing.sender_after,
                recipient_inventory_after: existing.recipient_after,
                replayed: true,
            });
        }

        ensure_transfer_participant_eligible(&sender, "你")?;
        let recipient = load_transfer_recipient(&transaction, key, recipient_subject_id)?;
        ensure_transfer_participant_eligible(&recipient, "对方")?;
        let sender_before = inventory_quantity(&transaction, sender.player_id, &item.item_key)?;
        if sender_before < quantity {
            return Err(format!(
                "背包中的{}不足：需要{}件，当前{}件",
                item.name, quantity, sender_before
            ));
        }
        let recipient_before =
            inventory_quantity(&transaction, recipient.player_id, &item.item_key)?;
        let sender_after = sender_before
            .checked_sub(quantity)
            .ok_or_else(|| "赠送后发送方背包数量下溢".to_string())?;
        let recipient_after = recipient_before
            .checked_add(quantity)
            .ok_or_else(|| "赠送后接收方背包数量溢出".to_string())?;
        if recipient_after > item.max_stack {
            return Err(format!(
                "对方背包中的{}最多堆叠{}件，当前已有{}件",
                item.name, item.max_stack, recipient_before
            ));
        }
        let timestamp = now_timestamp()?;
        set_inventory_quantity(
            &transaction,
            sender.player_id,
            &item.item_key,
            sender_after,
            timestamp,
        )?;
        set_inventory_quantity(
            &transaction,
            recipient.player_id,
            &item.item_key,
            recipient_after,
            timestamp,
        )?;
        let operation_log_id = insert_operation_log(&transaction, key, operation)?;
        let transfer_id = insert_asset_transfer(
            &transaction,
            key,
            &sender,
            &recipient,
            AssetTransferInsert {
                asset_kind: "item",
                currency_code: None,
                item_key: Some(&item.item_key),
                amount: quantity,
                sender_before,
                sender_after,
                recipient_before,
                recipient_after,
                source_message_id: operation.source_message_id,
                operation_log_id,
                created_at: timestamp,
            },
        )?;
        transaction
            .commit()
            .map_err(|error| format!("提交物品赠送事务失败：{error}"))?;
        Ok(ItemGiftReceipt {
            transfer_id,
            recipient_subject_id: recipient.subject_id,
            item,
            quantity,
            sender_inventory_after: sender_after,
            recipient_inventory_after: recipient_after,
            replayed: false,
        })
    }

    pub fn talk_to_npc_with_operation(
        &self,
        key: &IdentityKey<'_>,
        npc_name_or_key: &str,
        operation: &OperationLogInput<'_>,
    ) -> Result<NpcRecord, String> {
        validate_identity_key(key)?;
        validate_catalog_lookup(npc_name_or_key, "NPC 名称")?;
        validate_operation_input(operation)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始 NPC 对话事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        reject_replayed_operation(&transaction, key, operation)?;
        let (player_id, _, map) = load_player_map_for_identity(&transaction, key)?;
        let npc = transaction
            .query_row(
                r#"
                SELECT n.npc_key, n.map_key, m.name, n.name, n.npc_kind,
                       n.dialogue, n.description,
                       EXISTS(
                           SELECT 1 FROM shop_item si
                            WHERE si.npc_key = n.npc_key AND si.enabled = 1
                       )
                  FROM npc n
                  JOIN map m ON m.map_key = n.map_key
                 WHERE n.map_key = ?1 AND n.enabled = 1
                   AND (n.name = ?2 OR n.npc_key = ?2)
                 LIMIT 1
                "#,
                params![map.map_key, npc_name_or_key],
                |row| npc_record_from_row(row, 0),
            )
            .optional()
            .map_err(|error| format!("查询对话 NPC 失败：{error}"))?
            .ok_or_else(|| "当前地图不存在这位 NPC".to_string())?;
        let timestamp = now_timestamp()?;
        transaction
            .execute(
                r#"
                INSERT INTO player_npc(player_id, npc_key, updated_at)
                VALUES(?1, ?2, ?3)
                ON CONFLICT(player_id) DO UPDATE SET
                    npc_key = excluded.npc_key,
                    updated_at = excluded.updated_at
                "#,
                params![player_id, npc.npc_key, timestamp],
            )
            .map_err(|error| format!("保存当前对话 NPC 失败：{error}"))?;
        insert_operation_log(&transaction, key, operation)?;
        transaction
            .commit()
            .map_err(|error| format!("提交 NPC 对话事务失败：{error}"))?;
        Ok(npc)
    }

    pub fn npcs_at_current_map(&self, key: &IdentityKey<'_>) -> Result<NpcPage, String> {
        validate_identity_key(key)?;
        let connection = self.open()?;
        ensure_no_legacy_identity(&connection, key)?;
        let (_, _, map) = load_player_map_for_identity(&connection, key)?;
        let mut statement = connection
            .prepare(
                r#"
                SELECT n.npc_key, n.map_key, m.name, n.name, n.npc_kind,
                       n.dialogue, n.description,
                       EXISTS(
                           SELECT 1 FROM shop_item si
                            WHERE si.npc_key = n.npc_key AND si.enabled = 1
                       )
                  FROM npc n
                  JOIN map m ON m.map_key = n.map_key
                 WHERE n.map_key = ?1 AND n.enabled = 1
                 ORDER BY n.sort_order, n.npc_key
                "#,
            )
            .map_err(|error| format!("准备当前地图 NPC 查询失败：{error}"))?;
        let entries = statement
            .query_map([map.map_key.as_str()], |row| npc_record_from_row(row, 0))
            .map_err(|error| format!("查询当前地图 NPC 失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("解析当前地图 NPC 失败：{error}"))?;
        Ok(NpcPage {
            entries,
            map_name: map.name,
        })
    }

    pub fn shop_items_page(
        &self,
        key: &IdentityKey<'_>,
        npc_name_or_key: Option<&str>,
        page: usize,
        limit: usize,
    ) -> Result<ShopPage, String> {
        validate_identity_key(key)?;
        validate_catalog_page(page, limit, "商店")?;
        if let Some(npc_name_or_key) = npc_name_or_key {
            validate_catalog_lookup(npc_name_or_key, "NPC 名称")?;
        }
        let connection = self.open()?;
        ensure_no_legacy_identity(&connection, key)?;
        let (player_id, _, map) = load_player_map_for_identity(&connection, key)?;
        let npc = load_bound_npc_for_player(&connection, player_id, &map.map_key, npc_name_or_key)?;
        if npc.npc_kind != "merchant" || !npc.has_shop {
            return Err(format!("当前对话 NPC“{}”没有可用商店", npc.name));
        }
        let total = connection
            .query_row(
                "SELECT COUNT(*) FROM shop_item WHERE npc_key = ?1 AND enabled = 1",
                [npc.npc_key.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("统计商店商品失败：{error}"))?;
        let total = usize::try_from(total).map_err(|_| "商店商品数量超出分页范围".to_string())?;
        let page_count = total.div_ceil(limit).max(1);
        if page > page_count {
            return Err(format!("商店页码必须在 1 到 {page_count} 之间"));
        }
        let offset = page
            .checked_sub(1)
            .and_then(|page| page.checked_mul(limit))
            .ok_or_else(|| "商店分页偏移量溢出".to_string())?;
        let fetch_limit = i64::try_from(limit).map_err(|_| "商店分页数量无法转换".to_string())?;
        let offset = i64::try_from(offset).map_err(|_| "商店分页偏移量无法转换".to_string())?;
        let mut statement = connection
            .prepare(
                r#"
                SELECT si.npc_key, n.name,
                       i.item_key, i.name, i.category, i.quality, i.stackable,
                       i.max_stack, i.buy_price, i.sell_price, i.level_required,
                       i.effect_kind, i.effect_amount, i.revive_hp_percent,
                       i.purchasable, i.sellable, i.usable, i.description,
                       si.buy_price, si.stock
                  FROM shop_item si
                  JOIN npc n ON n.npc_key = si.npc_key
                  JOIN item i ON i.item_key = si.item_key
                 WHERE si.npc_key = ?1 AND si.enabled = 1
                 ORDER BY i.level_required, i.name, i.item_key
                 LIMIT ?2 OFFSET ?3
                "#,
            )
            .map_err(|error| format!("准备商店商品分页失败：{error}"))?;
        let entries = statement
            .query_map(params![npc.npc_key, fetch_limit, offset], |row| {
                let stock = row.get::<_, i64>(19)?;
                Ok(ShopItemEntry {
                    npc_key: row.get(0)?,
                    npc_name: row.get(1)?,
                    item: item_record_from_row(row, 2)?,
                    price: row.get(18)?,
                    stock: (stock >= 0).then_some(stock),
                })
            })
            .map_err(|error| format!("查询商店商品分页失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("解析商店商品分页失败：{error}"))?;
        Ok(ShopPage {
            npc,
            entries,
            page,
            page_count,
            total,
        })
    }

    pub fn inventory_page(
        &self,
        key: &IdentityKey<'_>,
        page: usize,
        limit: usize,
    ) -> Result<InventoryPage, String> {
        validate_identity_key(key)?;
        validate_catalog_page(page, limit, "背包")?;
        let connection = self.open()?;
        ensure_no_legacy_identity(&connection, key)?;
        let (player_id, _, _) = load_player_map_for_identity(&connection, key)?;
        let total = connection
            .query_row(
                "SELECT COUNT(*) FROM inventory WHERE player_id = ?1",
                [player_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("统计背包物品失败：{error}"))?;
        let total = usize::try_from(total).map_err(|_| "背包物品数量超出分页范围".to_string())?;
        let page_count = total.div_ceil(limit).max(1);
        if page > page_count {
            return Err(format!("背包页码必须在 1 到 {page_count} 之间"));
        }
        let offset = page
            .checked_sub(1)
            .and_then(|page| page.checked_mul(limit))
            .ok_or_else(|| "背包分页偏移量溢出".to_string())?;
        let fetch_limit = i64::try_from(limit).map_err(|_| "背包分页数量无法转换".to_string())?;
        let offset = i64::try_from(offset).map_err(|_| "背包分页偏移量无法转换".to_string())?;
        let mut statement = connection
            .prepare(
                r#"
                SELECT i.item_key, i.name, i.category, i.quality, i.stackable,
                       i.max_stack, i.buy_price, i.sell_price, i.level_required,
                       i.effect_kind, i.effect_amount, i.revive_hp_percent,
                       i.purchasable, i.sellable, i.usable, i.description,
                       inv.quantity
                  FROM inventory inv
                  JOIN item i ON i.item_key = inv.item_key
                 WHERE inv.player_id = ?1
                 ORDER BY i.quality DESC, i.name, i.item_key
                 LIMIT ?2 OFFSET ?3
                "#,
            )
            .map_err(|error| format!("准备背包分页查询失败：{error}"))?;
        let entries = statement
            .query_map(params![player_id, fetch_limit, offset], |row| {
                Ok(InventoryEntry {
                    item: item_record_from_row(row, 0)?,
                    quantity: row.get(16)?,
                })
            })
            .map_err(|error| format!("查询背包分页失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("解析背包分页失败：{error}"))?;
        Ok(InventoryPage {
            entries,
            page,
            page_count,
            total,
        })
    }

    pub fn buy_item_with_operation(
        &self,
        key: &IdentityKey<'_>,
        item_name_or_key: &str,
        quantity: i64,
        operation: &OperationLogInput<'_>,
    ) -> Result<PurchaseReceipt, String> {
        validate_identity_key(key)?;
        validate_catalog_lookup(item_name_or_key, "物品名称")?;
        validate_trade_quantity(quantity)?;
        validate_operation_input(operation)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始购买事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        reject_replayed_operation(&transaction, key, operation)?;
        let (player_id, level, map) = load_player_map_for_identity(&transaction, key)?;
        let bound_npc = load_bound_npc_for_player(&transaction, player_id, &map.map_key, None)?;
        if bound_npc.npc_kind != "merchant" || !bound_npc.has_shop {
            return Err(format!("当前对话 NPC“{}”没有可用商店", bound_npc.name));
        }
        let (npc_name, stock, unit_price, item) = transaction
            .query_row(
                r#"
                SELECT n.name, si.stock, si.buy_price,
                       i.item_key, i.name, i.category, i.quality, i.stackable,
                       i.max_stack, i.buy_price, i.sell_price, i.level_required,
                       i.effect_kind, i.effect_amount, i.revive_hp_percent,
                       i.purchasable, i.sellable, i.usable, i.description
                  FROM npc n
                  JOIN shop_item si ON si.npc_key = n.npc_key
                  JOIN item i ON i.item_key = si.item_key
                 WHERE n.npc_key = ?1 AND n.enabled = 1 AND n.npc_kind = 'merchant'
                   AND si.enabled = 1 AND (i.name = ?2 OR i.item_key = ?2)
                 LIMIT 1
                "#,
                params![bound_npc.npc_key, item_name_or_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        item_record_from_row(row, 3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("查询可购买物品失败：{error}"))?
            .ok_or_else(|| "当前地图的商店没有该物品".to_string())?;
        if !item.purchasable || item.category == "revival" {
            return Err("该物品不可购买".to_string());
        }
        if level < item.level_required {
            return Err(format!(
                "购买{}需要{}级，当前为{}级",
                item.name, item.level_required, level
            ));
        }
        if stock >= 0 && stock < quantity {
            return Err(format!("{}库存不足，当前剩余{}件", item.name, stock));
        }
        let inventory_before = inventory_quantity(&transaction, player_id, &item.item_key)?;
        let inventory_after = inventory_before
            .checked_add(quantity)
            .ok_or_else(|| "购买后背包数量溢出".to_string())?;
        if inventory_after > item.max_stack {
            return Err(format!(
                "{}最多堆叠{}件，背包已有{}件",
                item.name, item.max_stack, inventory_before
            ));
        }
        let total_price = unit_price
            .checked_mul(quantity)
            .ok_or_else(|| "购买总价溢出".to_string())?;
        let timestamp = now_timestamp()?;
        ensure_wallet(&transaction, player_id, "gold_soul_coin", timestamp)?;
        let balance_before =
            wallet_balance_in_transaction(&transaction, player_id, "gold_soul_coin")?;
        if balance_before < total_price {
            return Err(format!(
                "金魂币不足：需要{}，当前{}",
                total_price, balance_before
            ));
        }
        let balance_after = balance_before
            .checked_sub(total_price)
            .ok_or_else(|| "购买扣款下溢".to_string())?;
        update_wallet_balance(
            &transaction,
            player_id,
            "gold_soul_coin",
            balance_after,
            timestamp,
        )?;
        set_inventory_quantity(
            &transaction,
            player_id,
            &item.item_key,
            inventory_after,
            timestamp,
        )?;
        let stock_after = if stock < 0 {
            None
        } else {
            let stock_after = stock
                .checked_sub(quantity)
                .ok_or_else(|| "商店库存扣减下溢".to_string())?;
            let updated = transaction
                .execute(
                    r#"
                    UPDATE shop_item
                       SET stock = ?1, updated_at = ?2
                     WHERE npc_key = ?3 AND item_key = ?4 AND stock = ?5 AND enabled = 1
                    "#,
                    params![
                        stock_after,
                        timestamp,
                        bound_npc.npc_key,
                        item.item_key,
                        stock
                    ],
                )
                .map_err(|error| format!("更新商店库存失败：{error}"))?;
            if updated != 1 {
                return Err("购买时商店库存状态发生变化".to_string());
            }
            Some(stock_after)
        };
        insert_operation_log(&transaction, key, operation)?;
        transaction
            .commit()
            .map_err(|error| format!("提交购买事务失败：{error}"))?;
        Ok(PurchaseReceipt {
            npc_name,
            item,
            quantity,
            total_price,
            balance_after,
            inventory_after,
            stock_after,
        })
    }

    pub fn sell_item_with_operation(
        &self,
        key: &IdentityKey<'_>,
        item_name_or_key: &str,
        quantity: i64,
        operation: &OperationLogInput<'_>,
    ) -> Result<SaleReceipt, String> {
        validate_identity_key(key)?;
        validate_catalog_lookup(item_name_or_key, "物品名称")?;
        validate_trade_quantity(quantity)?;
        validate_operation_input(operation)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始出售事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        reject_replayed_operation(&transaction, key, operation)?;
        let (player_id, level, map) = load_player_map_for_identity(&transaction, key)?;
        let bound_npc = load_bound_npc_for_player(&transaction, player_id, &map.map_key, None)?;
        if bound_npc.npc_kind != "merchant" || !bound_npc.has_shop {
            return Err(format!("当前对话 NPC“{}”没有可用商店", bound_npc.name));
        }
        let (npc_key, npc_name, stock, item) = transaction
            .query_row(
                r#"
                SELECT n.npc_key, n.name, si.stock,
                       i.item_key, i.name, i.category, i.quality, i.stackable,
                       i.max_stack, i.buy_price, i.sell_price, i.level_required,
                       i.effect_kind, i.effect_amount, i.revive_hp_percent,
                       i.purchasable, i.sellable, i.usable, i.description
                  FROM npc n
                  JOIN shop_item si ON si.npc_key = n.npc_key
                  JOIN item i ON i.item_key = si.item_key
                 WHERE n.npc_key = ?1 AND n.enabled = 1 AND n.npc_kind = 'merchant'
                   AND si.enabled = 1 AND (i.name = ?2 OR i.item_key = ?2)
                 LIMIT 1
                "#,
                params![bound_npc.npc_key, item_name_or_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        item_record_from_row(row, 3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("查询可出售物品失败：{error}"))?
            .ok_or_else(|| "当前地图的商店不收购该物品".to_string())?;
        if !item.sellable || item.sell_price <= 0 {
            return Err("该物品不可出售".to_string());
        }
        if level < item.level_required {
            return Err(format!(
                "出售{}需要{}级，当前为{}级",
                item.name, item.level_required, level
            ));
        }
        let inventory_before = inventory_quantity(&transaction, player_id, &item.item_key)?;
        if inventory_before < quantity {
            return Err(format!(
                "背包中的{}不足：需要{}件，当前{}件",
                item.name, quantity, inventory_before
            ));
        }
        let inventory_after = inventory_before
            .checked_sub(quantity)
            .ok_or_else(|| "出售后背包数量下溢".to_string())?;
        let total_price = item
            .sell_price
            .checked_mul(quantity)
            .ok_or_else(|| "出售总价溢出".to_string())?;
        let timestamp = now_timestamp()?;
        ensure_wallet(&transaction, player_id, "gold_soul_coin", timestamp)?;
        let balance_before =
            wallet_balance_in_transaction(&transaction, player_id, "gold_soul_coin")?;
        let balance_after = balance_before
            .checked_add(total_price)
            .ok_or_else(|| "出售入账后钱包余额溢出".to_string())?;
        set_inventory_quantity(
            &transaction,
            player_id,
            &item.item_key,
            inventory_after,
            timestamp,
        )?;
        update_wallet_balance(
            &transaction,
            player_id,
            "gold_soul_coin",
            balance_after,
            timestamp,
        )?;
        if stock >= 0 {
            let stock_after = stock
                .checked_add(quantity)
                .ok_or_else(|| "回收后商店库存溢出".to_string())?;
            let updated = transaction
                .execute(
                    "UPDATE shop_item SET stock = ?1, updated_at = ?2 WHERE npc_key = ?3 AND item_key = ?4 AND stock = ?5 AND enabled = 1",
                    params![stock_after, timestamp, npc_key, item.item_key, stock],
                )
                .map_err(|error| format!("更新回收库存失败：{error}"))?;
            if updated != 1 {
                return Err("出售时商店库存状态发生变化".to_string());
            }
        }
        insert_operation_log(&transaction, key, operation)?;
        transaction
            .commit()
            .map_err(|error| format!("提交出售事务失败：{error}"))?;
        Ok(SaleReceipt {
            npc_name,
            item,
            quantity,
            total_price,
            balance_after,
            inventory_after,
        })
    }

    pub fn use_item_with_operation(
        &self,
        key: &IdentityKey<'_>,
        item_name_or_key: &str,
        operation: &OperationLogInput<'_>,
    ) -> Result<UseItemReceipt, String> {
        validate_identity_key(key)?;
        validate_catalog_lookup(item_name_or_key, "物品名称")?;
        validate_operation_input(operation)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始使用物品事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        reject_replayed_operation(&transaction, key, operation)?;
        let (player_id, level, hp_before, max_hp, soul_power_before, max_soul_power, state_before) =
            transaction
                .query_row(
                    r#"
                    SELECT p.id, p.level, p.hp, p.max_hp,
                           p.soul_power, p.max_soul_power, p.state
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
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, String>(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| format!("读取使用物品角色失败：{error}"))?
                .ok_or_else(|| "你还没有角色，请先使用“开始穿越 角色名 性别”".to_string())?;
        if state_before == "deleted" {
            return Err("角色已封存，不能使用物品".to_string());
        }
        let item = transaction
            .query_row(
                r#"
                SELECT item_key, name, category, quality, stackable, max_stack,
                       buy_price, sell_price, level_required, effect_kind,
                       effect_amount, revive_hp_percent, purchasable, sellable,
                       usable, description
                  FROM item
                 WHERE name = ?1 OR item_key = ?1
                 LIMIT 1
                "#,
                [item_name_or_key],
                |row| item_record_from_row(row, 0),
            )
            .optional()
            .map_err(|error| format!("查询使用物品定义失败：{error}"))?
            .ok_or_else(|| "当前世界不存在该物品".to_string())?;
        if !item.usable {
            return Err("该物品不能直接使用".to_string());
        }
        if level < item.level_required {
            return Err(format!(
                "使用{}需要{}级，当前为{}级",
                item.name, item.level_required, level
            ));
        }
        let inventory_before = inventory_quantity(&transaction, player_id, &item.item_key)?;
        if inventory_before < 1 {
            return Err(format!("你的背包中没有{}", item.name));
        }

        let mut hp_after = hp_before;
        let mut soul_power_after = soul_power_before;
        let mut state_after = state_before.clone();
        let consumed = match item.effect_kind.as_str() {
            "restore_hp" => {
                if state_before != "alive" {
                    return Err("当前状态无法使用生命恢复药".to_string());
                }
                if hp_before >= max_hp {
                    false
                } else {
                    hp_after = hp_before
                        .checked_add(item.effect_amount)
                        .ok_or_else(|| "生命恢复计算溢出".to_string())?
                        .min(max_hp);
                    true
                }
            }
            "restore_soul" => {
                if state_before != "alive" {
                    return Err("当前状态无法使用魂力恢复药".to_string());
                }
                if soul_power_before >= max_soul_power {
                    false
                } else {
                    soul_power_after = soul_power_before
                        .checked_add(item.effect_amount)
                        .ok_or_else(|| "魂力恢复计算溢出".to_string())?
                        .min(max_soul_power);
                    true
                }
            }
            "revive" => {
                if state_before != "dead" {
                    return Err("复活物品只能在角色死亡时使用".to_string());
                }
                hp_after = max_hp
                    .checked_mul(item.revive_hp_percent)
                    .ok_or_else(|| "复活生命计算溢出".to_string())?
                    .div_euclid(100)
                    .max(1)
                    .min(max_hp);
                state_after = "alive".to_string();
                true
            }
            _ => return Err("该物品的使用效果不受当前版本支持".to_string()),
        };

        let inventory_after = if consumed {
            inventory_before
                .checked_sub(1)
                .ok_or_else(|| "消耗物品后背包数量下溢".to_string())?
        } else {
            inventory_before
        };
        let timestamp = now_timestamp()?;
        if consumed {
            let updated = transaction
                .execute(
                    r#"
                    UPDATE player
                       SET hp = ?1, soul_power = ?2, state = ?3, updated_at = ?4
                     WHERE id = ?5
                    "#,
                    params![
                        hp_after,
                        soul_power_after,
                        state_after,
                        timestamp,
                        player_id
                    ],
                )
                .map_err(|error| format!("应用物品效果失败：{error}"))?;
            if updated != 1 {
                return Err("使用物品时角色状态发生变化".to_string());
            }
            set_inventory_quantity(
                &transaction,
                player_id,
                &item.item_key,
                inventory_after,
                timestamp,
            )?;
        }
        insert_operation_log(&transaction, key, operation)?;
        transaction
            .commit()
            .map_err(|error| format!("提交使用物品事务失败：{error}"))?;
        Ok(UseItemReceipt {
            item,
            consumed,
            inventory_after,
            hp_before,
            hp_after,
            max_hp,
            soul_power_before,
            soul_power_after,
            max_soul_power,
            state_before,
            state_after,
        })
    }

    /// 在单个 SQLite 事务内增加累计经验并推进等级，供签到和后续 PVE/任务奖励复用。
    #[allow(dead_code)]
    pub fn grant_experience(
        &self,
        key: &IdentityKey<'_>,
        amount: i64,
        operation: &OperationLogInput<'_>,
    ) -> Result<ExperienceGrantReceipt, String> {
        validate_identity_key(key)?;
        validate_operation_input(operation)?;
        if amount < 0 {
            return Err("获得经验不能为负数".to_string());
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始经验事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        let (player_id, level_before, exp_before) = transaction
            .query_row(
                r#"
                SELECT p.id, p.level, p.exp
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
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("读取经验角色失败：{error}"))?
            .ok_or_else(|| "你还没有角色，请先使用“开始穿越 <角色名> <性别>”".to_string())?;
        let receipt = apply_experience_in_transaction(
            &transaction,
            player_id,
            level_before,
            exp_before,
            amount,
            now_timestamp()?,
        )?;
        insert_operation_log(&transaction, key, operation)?;
        transaction
            .commit()
            .map_err(|error| format!("提交经验事务失败：{error}"))?;
        Ok(receipt)
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

    #[allow(dead_code)] // 为后续只读管理入口保留直接追加接口。
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

    #[allow(dead_code)] // 为后续只读管理入口保留操作日志分页接口。
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
    pub fn daily_checkin(
        &self,
        key: &IdentityKey<'_>,
        input: &DailyCheckinInput<'_>,
        operation: &OperationLogInput<'_>,
    ) -> Result<DailyCheckinResult, String> {
        validate_identity_key(key)?;
        validate_daily_checkin_input(input)?;
        validate_operation_input(operation)?;

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("开始签到事务失败：{error}"))?;
        ensure_no_legacy_identity(&transaction, key)?;
        let (player_id, current_level, current_exp) = transaction
            .query_row(
                r#"
                SELECT p.id, p.level, p.exp
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
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("读取签到角色失败：{error}"))?
            .ok_or_else(|| "你还没有角色，请先使用“开始穿越 角色名 性别”".to_string())?;

        if let Some(receipt) = load_daily_checkin_receipt(&transaction, player_id, input.game_day)?
        {
            return Ok(DailyCheckinResult::AlreadyClaimed(receipt));
        }

        let previous = latest_daily_checkin(&transaction, player_id)?;
        if previous.is_some_and(|claim| claim.game_day > input.game_day) {
            return Err("签到游戏日不能早于已有记录".to_string());
        }

        let total_claims = previous
            .map(|claim| claim.total_claims)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| "累计签到次数溢出".to_string())?;
        let streak_days = match previous {
            Some(claim) if claim.game_day.checked_add(1) == Some(input.game_day) => claim
                .streak_days
                .checked_add(1)
                .ok_or_else(|| "连续签到天数溢出".to_string())?,
            _ => 1,
        };
        let cycle_day = ((streak_days - 1) % 7) + 1;
        let exp_reward = expected_daily_checkin_exp(cycle_day)?;
        let currency_reward = match input.currency_reward_override {
            Some(reward) => reward,
            None => transaction
                .query_row(
                    "SELECT 100 + ((random() & 9223372036854775807) % 100)",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("生成签到货币奖励失败：{error}"))?,
        };

        let timestamp = now_timestamp()?;
        transaction
            .execute(
                r#"
                INSERT OR IGNORE INTO wallet(
                    player_id, currency_code, balance, created_at, updated_at
                ) VALUES(?1, ?2, 0, ?3, ?3)
                "#,
                params![player_id, input.currency_code, timestamp],
            )
            .map_err(|error| format!("创建签到钱包失败：{error}"))?;
        let current_balance = transaction
            .query_row(
                "SELECT balance FROM wallet WHERE player_id = ?1 AND currency_code = ?2",
                params![player_id, input.currency_code],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("读取签到钱包余额失败：{error}"))?;
        let currency_balance_after = current_balance
            .checked_add(currency_reward)
            .ok_or_else(|| "签到钱包余额累加溢出".to_string())?;
        let experience = apply_experience_in_transaction(
            &transaction,
            player_id,
            current_level,
            current_exp,
            exp_reward,
            timestamp,
        )?;
        let wallet_updates = transaction
            .execute(
                r#"
                UPDATE wallet
                   SET balance = ?1, updated_at = ?2
                 WHERE player_id = ?3 AND currency_code = ?4
                "#,
                params![
                    currency_balance_after,
                    timestamp,
                    player_id,
                    input.currency_code
                ],
            )
            .map_err(|error| format!("更新签到钱包余额失败：{error}"))?;
        if wallet_updates != 1 {
            return Err("更新签到钱包时记录状态发生变化".to_string());
        }
        transaction
            .execute(
                r#"
                INSERT INTO daily_checkin_claim(
                    player_id, game_day, total_claims, streak_days, cycle_day,
                    exp_reward, currency_code, currency_reward, exp_after,
                    currency_balance_after, created_at
                ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                "#,
                params![
                    player_id,
                    input.game_day,
                    total_claims,
                    streak_days,
                    cycle_day,
                    exp_reward,
                    input.currency_code,
                    currency_reward,
                    experience.exp_after,
                    currency_balance_after,
                    timestamp
                ],
            )
            .map_err(|error| format!("写入签到领取记录失败：{error}"))?;
        insert_operation_log(&transaction, key, operation)?;
        transaction
            .commit()
            .map_err(|error| format!("提交签到事务失败：{error}"))?;

        Ok(DailyCheckinResult::Claimed(DailyCheckinReceipt {
            game_day: input.game_day,
            total_claims,
            streak_days,
            cycle_day,
            exp_reward,
            level_before: experience.level_before,
            level_after: experience.level_after,
            levels_gained: experience.levels_gained,
            title_after: experience.title_after,
            currency_code: input.currency_code.to_string(),
            currency_reward,
            exp_after: experience.exp_after,
            currency_balance_after,
        }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LatestDailyCheckin {
    game_day: i64,
    total_claims: i64,
    streak_days: i64,
}

fn validate_daily_checkin_input(input: &DailyCheckinInput<'_>) -> Result<(), String> {
    if input.game_day < 0 {
        return Err("签到游戏日不能为负数".to_string());
    }
    validate_currency_code(input.currency_code)?;
    if input
        .currency_reward_override
        .is_some_and(|reward| !(100..=199).contains(&reward))
    {
        return Err("签到货币奖励覆盖值必须在 100 到 199 之间".to_string());
    }
    Ok(())
}

fn map_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MapRecord> {
    Ok(MapRecord {
        map_key: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        level_required: row.get(3)?,
        safe: row.get(4)?,
        pvp_enabled: row.get(5)?,
        teleport_enabled: row.get(6)?,
        sort_order: row.get(7)?,
    })
}

fn load_player_map_for_identity(
    connection: &Connection,
    key: &IdentityKey<'_>,
) -> Result<(i64, i64, MapRecord), String> {
    connection
        .query_row(
            r#"
            SELECT p.id, p.level,
                   m.map_key, m.name, m.description, m.level_required,
                   m.safe, m.pvp_enabled, m.teleport_enabled, m.sort_order
              FROM identity i
              JOIN player p ON p.identity_id = i.id
              JOIN player_map pm ON pm.player_id = p.id
              JOIN map m ON m.map_key = pm.map_key
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
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    MapRecord {
                        map_key: row.get(2)?,
                        name: row.get(3)?,
                        description: row.get(4)?,
                        level_required: row.get(5)?,
                        safe: row.get(6)?,
                        pvp_enabled: row.get(7)?,
                        teleport_enabled: row.get(8)?,
                        sort_order: row.get(9)?,
                    },
                ))
            },
        )
        .optional()
        .map_err(|error| format!("读取角色地图失败：{error}"))?
        .ok_or_else(|| "你还没有角色，请先使用“开始穿越 角色名 性别”".to_string())
}

fn load_transfer_sender(
    connection: &Connection,
    key: &IdentityKey<'_>,
) -> Result<TransferParticipant, String> {
    connection
        .query_row(
            r#"
            SELECT i.id, p.id, i.subject_id, p.state,
                   EXISTS(SELECT 1 FROM player_wuhun pw WHERE pw.player_id = p.id)
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
            |row| {
                Ok(TransferParticipant {
                    identity_id: row.get(0)?,
                    player_id: row.get(1)?,
                    subject_id: row.get(2)?,
                    state: row.get(3)?,
                    awakened: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("读取资产转移发送方失败：{error}"))?
        .ok_or_else(|| "你还没有角色，请先使用“开始穿越 角色名 性别”".to_string())
}

fn load_transfer_recipient(
    connection: &Connection,
    sender_key: &IdentityKey<'_>,
    recipient_subject_id: &str,
) -> Result<TransferParticipant, String> {
    connection
        .query_row(
            r#"
            SELECT i.id, p.id, i.subject_id, p.state,
                   EXISTS(SELECT 1 FROM player_wuhun pw WHERE pw.player_id = p.id)
              FROM identity i
              JOIN player p ON p.identity_id = i.id
             WHERE i.protocol = ?1 AND i.account_id = ?2 AND i.namespace = ?3
               AND i.subject_kind = 'user' AND i.subject_id = ?4
            "#,
            params![
                sender_key.protocol.as_str(),
                sender_key.account_id,
                sender_key.namespace,
                recipient_subject_id
            ],
            |row| {
                Ok(TransferParticipant {
                    identity_id: row.get(0)?,
                    player_id: row.get(1)?,
                    subject_id: row.get(2)?,
                    state: row.get(3)?,
                    awakened: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("读取资产转移接收方失败：{error}"))?
        .ok_or_else(|| "当前身份范围内不存在该接收玩家，或对方尚未创建角色".to_string())
}

fn ensure_transfer_participant_eligible(
    participant: &TransferParticipant,
    label: &str,
) -> Result<(), String> {
    if participant.state != "alive" {
        return Err(format!("{label}的角色当前状态不能进行资产转移"));
    }
    if !participant.awakened {
        return Err(format!("{label}的角色尚未完成武魂觉醒"));
    }
    Ok(())
}

fn load_asset_transfer_by_message(
    connection: &Connection,
    sender_identity_id: i64,
    source_message_id: &str,
) -> Result<Option<AssetTransferRecord>, String> {
    connection
        .query_row(
            r#"
            SELECT id, recipient_subject_id, asset_kind, currency_code, item_key,
                   amount, sender_after, recipient_after
              FROM asset_transfer
             WHERE sender_identity_id = ?1 AND source_message_id = ?2
            "#,
            params![sender_identity_id, source_message_id],
            |row| {
                Ok(AssetTransferRecord {
                    id: row.get(0)?,
                    recipient_subject_id: row.get(1)?,
                    asset_kind: row.get(2)?,
                    currency_code: row.get(3)?,
                    item_key: row.get(4)?,
                    amount: row.get(5)?,
                    sender_after: row.get(6)?,
                    recipient_after: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("读取资产转移幂等回执失败：{error}"))
}

fn load_transferable_item(
    connection: &Connection,
    item_name_or_key: &str,
) -> Result<ItemRecord, String> {
    let (item, transferable) = connection
        .query_row(
            r#"
            SELECT i.item_key, i.name, i.category, i.quality, i.stackable,
                   i.max_stack, i.buy_price, i.sell_price, i.level_required,
                   i.effect_kind, i.effect_amount, i.revive_hp_percent,
                   i.purchasable, i.sellable, i.usable, i.description,
                   COALESCE(policy.transferable, 0)
              FROM item i
         LEFT JOIN item_transfer_policy policy ON policy.item_key = i.item_key
             WHERE i.name = ?1 OR i.item_key = ?1
             LIMIT 1
            "#,
            [item_name_or_key],
            |row| Ok((item_record_from_row(row, 0)?, row.get::<_, bool>(16)?)),
        )
        .optional()
        .map_err(|error| format!("读取赠送物品定义失败：{error}"))?
        .ok_or_else(|| "当前世界不存在该物品".to_string())?;
    if !transferable || item.category != "consumable" || !item.stackable || !item.usable {
        return Err(format!("{}当前不可赠送", item.name));
    }
    Ok(item)
}

fn insert_asset_transfer(
    connection: &Connection,
    key: &IdentityKey<'_>,
    sender: &TransferParticipant,
    recipient: &TransferParticipant,
    input: AssetTransferInsert<'_>,
) -> Result<i64, String> {
    connection
        .execute(
            r#"
            INSERT INTO asset_transfer(
                protocol, account_id, namespace,
                sender_identity_id, recipient_identity_id,
                sender_subject_id, recipient_subject_id,
                asset_kind, currency_code, item_key, amount,
                sender_before, sender_after, recipient_before, recipient_after,
                source_message_id, operation_log_id, created_at
            ) VALUES(
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
            )
            "#,
            params![
                key.protocol.as_str(),
                key.account_id,
                key.namespace,
                sender.identity_id,
                recipient.identity_id,
                sender.subject_id,
                recipient.subject_id,
                input.asset_kind,
                input.currency_code,
                input.item_key,
                input.amount,
                input.sender_before,
                input.sender_after,
                input.recipient_before,
                input.recipient_after,
                input.source_message_id,
                input.operation_log_id,
                input.created_at
            ],
        )
        .map_err(|error| format!("写入不可变资产转移账本失败：{error}"))?;
    Ok(connection.last_insert_rowid())
}

fn load_bound_npc_for_player(
    connection: &Connection,
    player_id: i64,
    map_key: &str,
    requested_name_or_key: Option<&str>,
) -> Result<NpcRecord, String> {
    connection
        .query_row(
            r#"
            SELECT n.npc_key, n.map_key, m.name, n.name, n.npc_kind,
                   n.dialogue, n.description,
                   EXISTS(
                       SELECT 1 FROM shop_item si
                        WHERE si.npc_key = n.npc_key AND si.enabled = 1
                   )
              FROM player_npc pn
              JOIN npc n ON n.npc_key = pn.npc_key
              JOIN map m ON m.map_key = n.map_key
             WHERE pn.player_id = ?1 AND n.map_key = ?2 AND n.enabled = 1
               AND (?3 IS NULL OR n.name = ?3 OR n.npc_key = ?3)
             LIMIT 1
            "#,
            params![player_id, map_key, requested_name_or_key],
            |row| npc_record_from_row(row, 0),
        )
        .optional()
        .map_err(|error| format!("查询当前对话 NPC 失败：{error}"))?
        .ok_or_else(|| match requested_name_or_key {
            Some(name) => format!("尚未与当前地图的“{name}”对话，请先使用“对话 {name}”"),
            None => "请先与当前地图的商人对话，再使用“商店”".to_string(),
        })
}

fn item_record_from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<ItemRecord> {
    Ok(ItemRecord {
        item_key: row.get(offset)?,
        name: row.get(offset + 1)?,
        category: row.get(offset + 2)?,
        quality: row.get(offset + 3)?,
        stackable: row.get(offset + 4)?,
        max_stack: row.get(offset + 5)?,
        buy_price: row.get(offset + 6)?,
        sell_price: row.get(offset + 7)?,
        level_required: row.get(offset + 8)?,
        effect_kind: row.get(offset + 9)?,
        effect_amount: row.get(offset + 10)?,
        revive_hp_percent: row.get(offset + 11)?,
        purchasable: row.get(offset + 12)?,
        sellable: row.get(offset + 13)?,
        usable: row.get(offset + 14)?,
        description: row.get(offset + 15)?,
    })
}

fn npc_record_from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<NpcRecord> {
    Ok(NpcRecord {
        npc_key: row.get(offset)?,
        map_key: row.get(offset + 1)?,
        map_name: row.get(offset + 2)?,
        name: row.get(offset + 3)?,
        npc_kind: row.get(offset + 4)?,
        dialogue: row.get(offset + 5)?,
        description: row.get(offset + 6)?,
        has_shop: row.get(offset + 7)?,
    })
}

fn inventory_quantity(
    connection: &Connection,
    player_id: i64,
    item_key: &str,
) -> Result<i64, String> {
    connection
        .query_row(
            "SELECT COALESCE(quantity, 0) FROM inventory WHERE player_id = ?1 AND item_key = ?2",
            params![player_id, item_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|value| value.unwrap_or(0))
        .map_err(|error| format!("读取背包数量失败：{error}"))
}

fn set_inventory_quantity(
    connection: &Connection,
    player_id: i64,
    item_key: &str,
    quantity: i64,
    timestamp: i64,
) -> Result<(), String> {
    if quantity < 0 {
        return Err("背包数量不能为负数".to_string());
    }
    if quantity == 0 {
        connection
            .execute(
                "DELETE FROM inventory WHERE player_id = ?1 AND item_key = ?2",
                params![player_id, item_key],
            )
            .map_err(|error| format!("清理背包物品失败：{error}"))?;
        return Ok(());
    }
    connection
        .execute(
            r#"
            INSERT INTO inventory(player_id, item_key, quantity, updated_at)
            VALUES(?1, ?2, ?3, ?4)
            ON CONFLICT(player_id, item_key) DO UPDATE SET
                quantity = excluded.quantity,
                updated_at = excluded.updated_at
            "#,
            params![player_id, item_key, quantity, timestamp],
        )
        .map_err(|error| format!("更新背包物品失败：{error}"))?;
    Ok(())
}

fn ensure_wallet(
    connection: &Connection,
    player_id: i64,
    currency_code: &str,
    timestamp: i64,
) -> Result<(), String> {
    connection
        .execute(
            r#"
            INSERT OR IGNORE INTO wallet(
                player_id, currency_code, balance, created_at, updated_at
            ) VALUES(?1, ?2, 0, ?3, ?3)
            "#,
            params![player_id, currency_code, timestamp],
        )
        .map_err(|error| format!("创建经济钱包失败：{error}"))?;
    Ok(())
}

fn wallet_balance_in_transaction(
    connection: &Connection,
    player_id: i64,
    currency_code: &str,
) -> Result<i64, String> {
    connection
        .query_row(
            "SELECT balance FROM wallet WHERE player_id = ?1 AND currency_code = ?2",
            params![player_id, currency_code],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("读取经济钱包余额失败：{error}"))
}

fn update_wallet_balance(
    connection: &Connection,
    player_id: i64,
    currency_code: &str,
    balance: i64,
    timestamp: i64,
) -> Result<(), String> {
    let updated = connection
        .execute(
            "UPDATE wallet SET balance = ?1, updated_at = ?2 WHERE player_id = ?3 AND currency_code = ?4",
            params![balance, timestamp, player_id, currency_code],
        )
        .map_err(|error| format!("更新经济钱包余额失败：{error}"))?;
    if updated != 1 {
        return Err("更新经济钱包时记录状态发生变化".to_string());
    }
    Ok(())
}

fn update_player_map(
    connection: &Connection,
    player_id: i64,
    target: &MapRecord,
) -> Result<(), String> {
    let timestamp = now_timestamp()?;
    let map_updates = connection
        .execute(
            "UPDATE player_map SET map_key = ?1, updated_at = ?2 WHERE player_id = ?3",
            params![target.map_key, timestamp, player_id],
        )
        .map_err(|error| format!("更新角色地图失败：{error}"))?;
    if map_updates != 1 {
        return Err("更新角色地图时角色状态发生变化".to_string());
    }
    let player_updates = connection
        .execute(
            "UPDATE player SET map_name = ?1, updated_at = ?2 WHERE id = ?3",
            params![target.name, timestamp, player_id],
        )
        .map_err(|error| format!("同步角色地图名称失败：{error}"))?;
    if player_updates != 1 {
        return Err("同步角色地图名称时角色状态发生变化".to_string());
    }
    // 对话只对当前停留阶段有效；移动或传送后必须重新与目标地图 NPC 对话。
    connection
        .execute("DELETE FROM player_npc WHERE player_id = ?1", [player_id])
        .map_err(|error| format!("移动后清理 NPC 对话绑定失败：{error}"))?;
    Ok(())
}

fn normalize_map_direction(direction: &str) -> Result<&'static str, String> {
    match direction.trim() {
        "上" | "北" | "north" => Ok("north"),
        "下" | "南" | "south" => Ok("south"),
        "左" | "西" | "west" => Ok("west"),
        "右" | "东" | "east" => Ok("east"),
        _ => Err("方向只能是上、下、左或右".to_string()),
    }
}

fn direction_name(direction: &str) -> &'static str {
    match direction {
        "north" => "上",
        "south" => "下",
        "west" => "左",
        "east" => "右",
        _ => "未知",
    }
}

fn validate_map_page(page: usize, limit: usize) -> Result<(), String> {
    if page == 0 || page > 100 {
        return Err("地图页码必须在 1 到 100 之间".to_string());
    }
    if !(1..=50).contains(&limit) {
        return Err("地图分页数量必须在 1 到 50 之间".to_string());
    }
    Ok(())
}

fn validate_catalog_page(page: usize, limit: usize, label: &str) -> Result<(), String> {
    if page == 0 || page > 100 {
        return Err(format!("{label}页码必须在 1 到 100 之间"));
    }
    if !(1..=50).contains(&limit) {
        return Err(format!("{label}分页数量必须在 1 到 50 之间"));
    }
    Ok(())
}

fn validate_catalog_lookup(value: &str, label: &str) -> Result<(), String> {
    if value != value.trim()
        || value.is_empty()
        || value.chars().count() > 128
        || value.chars().any(char::is_control)
    {
        return Err(format!("{label}必须是 1 到 128 个无控制字符的非空字符串"));
    }
    Ok(())
}

fn validate_trade_quantity(quantity: i64) -> Result<(), String> {
    if !(1..=9999).contains(&quantity) {
        return Err("交易数量必须在 1 到 9999 之间".to_string());
    }
    Ok(())
}

fn validate_transfer_recipient(
    sender: &IdentityKey<'_>,
    recipient_subject_id: &str,
) -> Result<(), String> {
    if recipient_subject_id != recipient_subject_id.trim()
        || !valid_audit_value(recipient_subject_id, 256)
    {
        return Err("目标用户 ID 必须是 1 到 256 个无控制字符且无首尾空白的字符串".to_string());
    }
    if recipient_subject_id == "all" {
        return Err("不能把 @全体 作为资产接收人".to_string());
    }
    if recipient_subject_id == sender.subject_id {
        return Err("不能向自己转移资产".to_string());
    }
    Ok(())
}

fn validate_transfer_operation(
    operation: &OperationLogInput<'_>,
    expected_command: &str,
) -> Result<(), String> {
    validate_operation_input(operation)?;
    if operation.command != expected_command || operation.outcome != "ok" {
        return Err(format!(
            "资产转移成功审计必须使用规范命令“{expected_command}”和 ok 结果"
        ));
    }
    if !valid_audit_value(operation.source_message_id, 256) {
        return Err("资产转移要求 1 到 256 个无控制字符的非空消息 ID".to_string());
    }
    Ok(())
}

fn validate_currency_code(currency_code: &str) -> Result<(), String> {
    if currency_code != currency_code.trim()
        || currency_code.is_empty()
        || currency_code.len() > 32
        || !currency_code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("币种代码必须是 1 到 32 个字母、数字、点、下划线或横线".to_string());
    }
    Ok(())
}

fn expected_daily_checkin_exp(cycle_day: i64) -> Result<i64, String> {
    [60, 70, 80, 90, 100, 110, 150]
        .get(usize::try_from(cycle_day - 1).map_err(|_| "签到轮次必须在 1 到 7 之间".to_string())?)
        .copied()
        .ok_or_else(|| "签到轮次必须在 1 到 7 之间".to_string())
}

fn latest_daily_checkin(
    connection: &Connection,
    player_id: i64,
) -> Result<Option<LatestDailyCheckin>, String> {
    connection
        .query_row(
            r#"
            SELECT game_day, total_claims, streak_days
              FROM daily_checkin_claim
             WHERE player_id = ?1
             ORDER BY game_day DESC
             LIMIT 1
            "#,
            [player_id],
            |row| {
                Ok(LatestDailyCheckin {
                    game_day: row.get(0)?,
                    total_claims: row.get(1)?,
                    streak_days: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("读取最近签到记录失败：{error}"))
}

fn load_daily_checkin_receipt(
    connection: &Connection,
    player_id: i64,
    game_day: i64,
) -> Result<Option<DailyCheckinReceipt>, String> {
    let receipt = connection
        .query_row(
            r#"
            SELECT game_day, total_claims, streak_days, cycle_day, exp_reward,
                   currency_code, currency_reward, exp_after, currency_balance_after
              FROM daily_checkin_claim
             WHERE player_id = ?1 AND game_day = ?2
            "#,
            params![player_id, game_day],
            |row| {
                Ok(DailyCheckinReceipt {
                    game_day: row.get(0)?,
                    total_claims: row.get(1)?,
                    streak_days: row.get(2)?,
                    cycle_day: row.get(3)?,
                    exp_reward: row.get(4)?,
                    level_before: 1,
                    level_after: 1,
                    levels_gained: 0,
                    title_after: "1级魂士".to_string(),
                    currency_code: row.get(5)?,
                    currency_reward: row.get(6)?,
                    exp_after: row.get(7)?,
                    currency_balance_after: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("读取当日签到记录失败：{error}"))?;
    receipt
        .map(|mut receipt| {
            let level_after = level_for_total_exp(receipt.exp_after)?;
            receipt.level_before = level_after;
            receipt.level_after = level_after;
            receipt.title_after = full_level_title(level_after)?;
            Ok(receipt)
        })
        .transpose()
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

fn sqlite_is_busy_or_locked(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(failure.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
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

fn restore_player_sequence(transaction: &Transaction<'_>, sequence: i64) -> Result<(), String> {
    let current = transaction
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'player'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| format!("读取迁移后角色序列失败：{error}"))?;
    let sequence = current.map_or(sequence, |current| current.max(sequence));
    let changed = transaction
        .execute(
            "UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'player'",
            [sequence],
        )
        .map_err(|error| format!("恢复角色序列失败：{error}"))?;
    if changed == 0 {
        transaction
            .execute(
                "INSERT INTO sqlite_sequence(name, seq) VALUES('player', ?1)",
                [sequence],
            )
            .map_err(|error| format!("写入角色序列失败：{error}"))?;
    }
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

fn validate_v5_schema(connection: &Connection) -> Result<(), String> {
    let wallet_columns = table_columns_with_type(connection, "wallet")?;
    let expected_wallet_columns = vec![
        TableColumnInfo::new("id", "INTEGER", false, true, None, 0),
        TableColumnInfo::new("player_id", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("currency_code", "TEXT", true, false, None, 0),
        TableColumnInfo::new("balance", "INTEGER", true, false, Some("0"), 0),
        TableColumnInfo::new("created_at", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("updated_at", "INTEGER", true, false, None, 0),
    ];
    if wallet_columns != expected_wallet_columns {
        return Err(format!(
            "数据库已标记迁移 v5，但钱包字段不匹配：{wallet_columns:?}"
        ));
    }

    let claim_columns = table_columns_with_type(connection, "daily_checkin_claim")?;
    let expected_claim_columns = vec![
        TableColumnInfo::new("id", "INTEGER", false, true, None, 0),
        TableColumnInfo::new("player_id", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("game_day", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("total_claims", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("streak_days", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("cycle_day", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("exp_reward", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("currency_code", "TEXT", true, false, None, 0),
        TableColumnInfo::new("currency_reward", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("exp_after", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("currency_balance_after", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("created_at", "INTEGER", true, false, None, 0),
    ];
    if claim_columns != expected_claim_columns {
        return Err(format!(
            "数据库已标记迁移 v5，但签到领取字段不匹配：{claim_columns:?}"
        ));
    }

    validate_player_foreign_key(connection, "wallet")?;
    validate_player_foreign_key(connection, "daily_checkin_claim")?;
    validate_v5_index_set(connection, "wallet", &["wallet_player_currency"])?;
    validate_v5_index_set(
        connection,
        "daily_checkin_claim",
        &[
            "daily_checkin_claim_player_day",
            "daily_checkin_claim_player_page",
        ],
    )?;
    validate_named_index(
        connection,
        "wallet",
        "wallet_player_currency",
        true,
        &["player_id", "currency_code"],
    )?;
    validate_named_index(
        connection,
        "daily_checkin_claim",
        "daily_checkin_claim_player_day",
        true,
        &["player_id", "game_day"],
    )?;
    validate_named_index(
        connection,
        "daily_checkin_claim",
        "daily_checkin_claim_player_page",
        false,
        &["player_id", "id"],
    )?;

    validate_v5_table_sql(
        connection,
        "wallet",
        &[
            "AUTOINCREMENT",
            ") STRICT",
            "REFERENCES PLAYER(ID) ON DELETE CASCADE",
            "LENGTH(CURRENCY_CODE) BETWEEN 1 AND 32",
            "CURRENCY_CODE = TRIM(CURRENCY_CODE)",
            "CURRENCY_CODE NOT GLOB",
            "BALANCE INTEGER NOT NULL DEFAULT 0",
            "BALANCE >= 0",
            "CREATED_AT >= 0",
            "UPDATED_AT >= 0",
        ],
    )?;
    validate_v5_table_sql(
        connection,
        "daily_checkin_claim",
        &[
            "AUTOINCREMENT",
            ") STRICT",
            "REFERENCES PLAYER(ID) ON DELETE CASCADE",
            "GAME_DAY >= 0",
            "TOTAL_CLAIMS >= 1",
            "STREAK_DAYS BETWEEN 1 AND TOTAL_CLAIMS",
            "CYCLE_DAY = ((STREAK_DAYS - 1) % 7) + 1",
            "WHEN 7 THEN 150",
            "LENGTH(CURRENCY_CODE) BETWEEN 1 AND 32",
            "CURRENCY_CODE = TRIM(CURRENCY_CODE)",
            "CURRENCY_CODE NOT GLOB",
            "CURRENCY_REWARD BETWEEN 100 AND 199",
            "EXP_AFTER >= 0",
            "CURRENCY_BALANCE_AFTER >= 0",
            "CREATED_AT >= 0",
        ],
    )?;

    validate_v5_triggers(connection)?;
    probe_v5_guards(connection)
}

fn probe_v5_guards(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch("SAVEPOINT qimen_v5_guard_probe;")
        .map_err(|error| format!("开始 v5 经济约束探针失败：{error}"))?;
    let probe_result = (|| -> Result<(), String> {
        let token = connection
            .query_row("SELECT lower(hex(randomblob(16)))", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("生成 v5 经济探针标识失败：{error}"))?;
        let account_id = format!("v5-probe-{token}");
        connection
            .execute(
                r#"
                INSERT INTO identity(
                    protocol, account_id, namespace, subject_kind, subject_id, created_at
                ) VALUES('onebot11', ?1, 'v5-probe', 'user', ?1, 0)
                "#,
                [&account_id],
            )
            .map_err(|error| format!("v5 经济探针无法创建临时身份：{error}"))?;
        let identity_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO player(identity_id, name, gender, created_at, updated_at) VALUES(?1, 'v5-probe', '男', 0, 0)",
                [identity_id],
            )
            .map_err(|error| format!("v5 经济探针无法创建临时角色：{error}"))?;
        let player_id = connection.last_insert_rowid();

        connection
            .execute(
                "INSERT INTO wallet(player_id, currency_code, balance, created_at, updated_at) VALUES(?1, 'valid', 0, 0, 0)",
                [player_id],
            )
            .map_err(|error| format!("v5 经济探针无法插入合法钱包：{error}"))?;
        if connection
            .execute(
                "INSERT INTO wallet(player_id, currency_code, balance, created_at, updated_at) VALUES(?1, 'valid', 0, 0, 0)",
                [player_id],
            )
            .is_ok()
        {
            return Err("v5 钱包唯一约束探针被绕过".to_string());
        }
        for (label, sql) in [
            (
                "负余额",
                "INSERT INTO wallet(player_id, currency_code, balance, created_at, updated_at) VALUES(?1, 'negative', -1, 0, 0)",
            ),
            (
                "非法币种",
                "INSERT INTO wallet(player_id, currency_code, balance, created_at, updated_at) VALUES(?1, 'bad code', 0, 0, 0)",
            ),
            (
                "REAL 余额",
                "INSERT INTO wallet(player_id, currency_code, balance, created_at, updated_at) VALUES(?1, 'real', 1.5, 0, 0)",
            ),
            (
                "TEXT 余额",
                "INSERT INTO wallet(player_id, currency_code, balance, created_at, updated_at) VALUES(?1, 'text', 'abc', 0, 0)",
            ),
        ] {
            if connection.execute(sql, [player_id]).is_ok() {
                return Err(format!("v5 钱包 {label} 约束探针被绕过"));
            }
        }

        let valid_claim = r#"
            INSERT INTO daily_checkin_claim(
                player_id, game_day, total_claims, streak_days, cycle_day,
                exp_reward, currency_code, currency_reward, exp_after,
                currency_balance_after, created_at
            ) VALUES(?1, 1, 1, 1, 1, 60, 'valid', 100, 60, 100, 0)
        "#;
        connection
            .execute(valid_claim, [player_id])
            .map_err(|error| format!("v5 经济探针无法插入合法签到领取：{error}"))?;
        if connection.execute(valid_claim, [player_id]).is_ok() {
            return Err("v5 签到领取唯一约束探针被绕过".to_string());
        }
        for (label, sql) in [
            (
                "负游戏日",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, -1, 1, 1, 1, 60, 'valid', 100, 60, 100, 0)",
            ),
            (
                "零累计",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 2, 0, 1, 1, 60, 'valid', 100, 60, 100, 0)",
            ),
            (
                "连签超过累计",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 3, 1, 2, 2, 70, 'valid', 100, 70, 100, 0)",
            ),
            (
                "轮次不匹配",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 4, 1, 1, 2, 70, 'valid', 100, 70, 100, 0)",
            ),
            (
                "经验奖励不匹配",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 5, 1, 1, 1, 70, 'valid', 100, 70, 100, 0)",
            ),
            (
                "货币奖励过小",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 6, 1, 1, 1, 60, 'valid', 99, 60, 99, 0)",
            ),
            (
                "货币奖励过大",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 7, 1, 1, 1, 60, 'valid', 200, 60, 200, 0)",
            ),
            (
                "负经验余额",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 8, 1, 1, 1, 60, 'valid', 100, -1, 100, 0)",
            ),
            (
                "负货币余额",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 9, 1, 1, 1, 60, 'valid', 100, 60, -1, 0)",
            ),
            (
                "负创建时间",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 10, 1, 1, 1, 60, 'valid', 100, 60, 100, -1)",
            ),
            (
                "REAL 游戏日",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 11.5, 1, 1, 1, 60, 'valid', 100, 60, 100, 0)",
            ),
            (
                "TEXT 累计",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 12, 'abc', 1, 1, 60, 'valid', 100, 60, 100, 0)",
            ),
            (
                "非法签到币种",
                "INSERT INTO daily_checkin_claim VALUES(NULL, ?1, 13, 1, 1, 1, 60, 'bad code', 100, 60, 100, 0)",
            ),
        ] {
            if connection.execute(sql, [player_id]).is_ok() {
                return Err(format!("v5 签到领取 {label} 约束探针被绕过"));
            }
        }
        Ok(())
    })();
    let rollback_result = connection
        .execute_batch("ROLLBACK TO qimen_v5_guard_probe; RELEASE qimen_v5_guard_probe;")
        .map_err(|error| format!("回滚 v5 经济约束探针失败：{error}"));
    match (probe_result, rollback_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(probe_error), Err(rollback_error)) => Err(format!("{probe_error}；{rollback_error}")),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TableColumnInfo {
    name: String,
    sql_type: String,
    not_null: bool,
    primary_key: bool,
    default_value: Option<String>,
    hidden: i64,
}

impl TableColumnInfo {
    fn new(
        name: &str,
        sql_type: &str,
        not_null: bool,
        primary_key: bool,
        default_value: Option<&str>,
        hidden: i64,
    ) -> Self {
        Self {
            name: name.to_string(),
            sql_type: sql_type.to_string(),
            not_null,
            primary_key,
            default_value: default_value.map(str::to_string),
            hidden,
        }
    }
}

fn table_columns_with_type(
    connection: &Connection,
    table: &str,
) -> Result<Vec<TableColumnInfo>, String> {
    let escaped_table = table.replace('"', "\"\"");
    connection
        .prepare(&format!("PRAGMA table_xinfo(\"{escaped_table}\")"))
        .map_err(|error| format!("读取表 {table} 字段失败：{error}"))?
        .query_map([], |row| {
            Ok(TableColumnInfo {
                name: row.get(1)?,
                sql_type: row.get(2)?,
                not_null: row.get(3)?,
                default_value: row.get(4)?,
                primary_key: row.get(5)?,
                hidden: row.get(6)?,
            })
        })
        .map_err(|error| format!("查询表 {table} 字段失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析表 {table} 字段失败：{error}"))
}

fn validate_player_foreign_key(connection: &Connection, table: &str) -> Result<(), String> {
    let escaped_table = table.replace('"', "\"\"");
    let mut foreign_keys = connection
        .prepare(&format!("PRAGMA foreign_key_list(\"{escaped_table}\")"))
        .map_err(|error| format!("读取表 {table} 外键失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        .map_err(|error| format!("查询表 {table} 外键失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析表 {table} 外键失败：{error}"))?;
    foreign_keys.sort();
    let expected = [(
        "player".to_string(),
        "player_id".to_string(),
        "id".to_string(),
        "NO ACTION".to_string(),
        "CASCADE".to_string(),
        "NONE".to_string(),
    )];
    if foreign_keys != expected {
        return Err(format!(
            "数据库已标记迁移 v5，但表 {table} 的玩家外键不匹配：{foreign_keys:?}"
        ));
    }
    let table_sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取表 {table} 外键声明失败：{error}"))?
        .ok_or_else(|| format!("数据库已标记迁移 v5，但缺少表 {table}"))?
        .to_ascii_uppercase();
    if table_sql.contains("DEFERRABLE") || table_sql.contains("INITIALLY") {
        return Err(format!(
            "数据库已标记迁移 v5，但表 {table} 的玩家外键不得延迟约束"
        ));
    }
    Ok(())
}

fn validate_v5_index_set(
    connection: &Connection,
    table: &str,
    expected_names: &[&str],
) -> Result<(), String> {
    let escaped_table = table.replace('"', "\"\"");
    let mut actual_names = connection
        .prepare(&format!("PRAGMA index_list(\"{escaped_table}\")"))
        .map_err(|error| format!("读取表 {table} 索引集合失败：{error}"))?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("查询表 {table} 索引集合失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析表 {table} 索引集合失败：{error}"))?;
    actual_names.sort();
    let mut expected_names = expected_names
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    expected_names.sort();
    if actual_names != expected_names {
        return Err(format!(
            "数据库已标记迁移 v5，但表 {table} 的索引集合不匹配：实际 {actual_names:?}，期望 {expected_names:?}"
        ));
    }
    Ok(())
}

fn validate_v5_triggers(connection: &Connection) -> Result<(), String> {
    let triggers = connection
        .prepare(
            "SELECT name, tbl_name, sql FROM sqlite_master WHERE type = 'trigger' ORDER BY name",
        )
        .map_err(|error| format!("读取 v5 触发器集合失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| format!("查询 v5 触发器集合失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v5 触发器集合失败：{error}"))?;
    for (name, table, sql) in triggers {
        if sql_mentions_identifier(&sql, "wallet")
            || sql_mentions_identifier(&sql, "daily_checkin_claim")
        {
            return Err(format!(
                "数据库已标记迁移 v5，但触发器 {name}（目标表 {table}）引用了 v5 经济表"
            ));
        }
    }
    Ok(())
}

fn sql_mentions_identifier(sql: &str, identifier: &str) -> bool {
    let sql = sql.to_ascii_uppercase();
    let identifier = identifier.to_ascii_uppercase();
    let mut offset = 0;
    while let Some(relative) = sql[offset..].find(&identifier) {
        let start = offset + relative;
        let end = start + identifier.len();
        let is_boundary = |byte: Option<u8>| {
            byte.is_none_or(|byte| !(byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'))
        };
        if is_boundary(sql.as_bytes().get(start.wrapping_sub(1)).copied())
            && is_boundary(sql.as_bytes().get(end).copied())
        {
            return true;
        }
        offset = end;
    }
    false
}

fn validate_named_index(
    connection: &Connection,
    table: &str,
    index_name: &str,
    expected_unique: bool,
    expected_columns: &[&str],
) -> Result<(), String> {
    let escaped_table = table.replace('"', "\"\"");
    let indexes = connection
        .prepare(&format!("PRAGMA index_list(\"{escaped_table}\")"))
        .map_err(|error| format!("读取表 {table} 索引失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .map_err(|error| format!("查询表 {table} 索引失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析表 {table} 索引失败：{error}"))?;
    let (_, unique, origin, partial) = indexes
        .into_iter()
        .find(|(name, _, _, _)| name == index_name)
        .ok_or_else(|| format!("数据库已标记迁移 v5，但缺少索引 {index_name}"))?;
    if unique != expected_unique || origin != "c" || partial {
        return Err(format!(
            "数据库已标记迁移 v5，但索引 {index_name} 的 unique/origin/partial 不匹配"
        ));
    }
    let escaped_index = index_name.replace('"', "\"\"");
    let mut index_columns = connection
        .prepare(&format!("PRAGMA index_xinfo(\"{escaped_index}\")"))
        .map_err(|error| format!("读取索引 {index_name} 字段失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, bool>(5)?,
            ))
        })
        .map_err(|error| format!("查询索引 {index_name} 字段失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析索引 {index_name} 字段失败：{error}"))?;
    index_columns.sort_by_key(|(seqno, ..)| *seqno);
    let key_columns = index_columns
        .iter()
        .filter(|(_, _, _, _, _, key)| *key)
        .map(|(_, _, name, desc, collation, _)| (name.clone(), *desc, collation.clone()))
        .collect::<Vec<_>>();
    let expected_key_columns = expected_columns
        .iter()
        .map(|column| (Some((*column).to_string()), false, "BINARY".to_string()))
        .collect::<Vec<_>>();
    if key_columns != expected_key_columns {
        return Err(format!(
            "数据库已标记迁移 v5，但索引 {index_name} 字段不匹配：{key_columns:?}"
        ));
    }
    let auxiliary_columns = index_columns
        .iter()
        .filter(|(_, _, _, _, _, key)| !*key)
        .collect::<Vec<_>>();
    if auxiliary_columns.len() != 1
        || auxiliary_columns[0].1 != -1
        || auxiliary_columns[0].2.is_some()
        || auxiliary_columns[0].3
        || auxiliary_columns[0].4 != "BINARY"
    {
        return Err(format!(
            "数据库已标记迁移 v5，但索引 {index_name} 存在异常辅助字段"
        ));
    }
    Ok(())
}

fn validate_v5_table_sql(
    connection: &Connection,
    table: &str,
    markers: &[&str],
) -> Result<(), String> {
    let sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取 v5 表 {table} 建表语句失败：{error}"))?
        .ok_or_else(|| format!("数据库已标记迁移 v5，但缺少表 {table}"))?
        .to_ascii_uppercase();
    for marker in markers {
        if !sql.contains(marker) {
            return Err(format!(
                "数据库已标记迁移 v5，但表 {table} 缺少约束：{marker}"
            ));
        }
    }
    Ok(())
}

// v6 会重建 player，迁移前必须确认旧表结构完全来自受支持的 v1 schema。
// 任何额外列、索引或触发器都拒绝迁移，避免重建时静默丢失用户数据结构。
fn validate_v6_source_player_schema(connection: &Connection) -> Result<(), String> {
    let columns = table_columns_with_type(connection, "player")?;
    let expected_columns = vec![
        TableColumnInfo::new("id", "INTEGER", false, true, None, 0),
        TableColumnInfo::new("identity_id", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("name", "TEXT", true, false, None, 0),
        TableColumnInfo::new("gender", "TEXT", true, false, None, 0),
        TableColumnInfo::new("level", "INTEGER", true, false, Some("1"), 0),
        TableColumnInfo::new("exp", "INTEGER", true, false, Some("0"), 0),
        TableColumnInfo::new("hp", "INTEGER", true, false, Some("100"), 0),
        TableColumnInfo::new("max_hp", "INTEGER", true, false, Some("100"), 0),
        TableColumnInfo::new("soul_power", "INTEGER", true, false, Some("50"), 0),
        TableColumnInfo::new("max_soul_power", "INTEGER", true, false, Some("50"), 0),
        TableColumnInfo::new("strength", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("agility", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("spirit", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("endurance", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("perception", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("luck", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("life_count", "INTEGER", true, false, Some("1"), 0),
        TableColumnInfo::new("state", "TEXT", true, false, Some("'alive'"), 0),
        TableColumnInfo::new("map_name", "TEXT", true, false, Some("'圣魂村'"), 0),
        TableColumnInfo::new("created_at", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("updated_at", "INTEGER", true, false, None, 0),
    ];
    if columns != expected_columns {
        return Err(format!("v6 迁移前角色字段不匹配，已拒绝重建：{columns:?}"));
    }

    let table_sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'player'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取 v6 迁移前角色建表语句失败：{error}"))?
        .ok_or_else(|| "v6 迁移前缺少 player 表，已拒绝重建".to_string())?
        .to_ascii_uppercase();
    for marker in [
        "AUTOINCREMENT",
        "IDENTITY_ID INTEGER NOT NULL UNIQUE REFERENCES IDENTITY(ID) ON DELETE CASCADE",
        "GENDER TEXT NOT NULL CHECK(GENDER IN ('男', '女'))",
        "LEVEL INTEGER NOT NULL DEFAULT 1 CHECK(LEVEL BETWEEN 1 AND 100)",
        "EXP INTEGER NOT NULL DEFAULT 0 CHECK(EXP >= 0)",
        "LIFE_COUNT INTEGER NOT NULL DEFAULT 1 CHECK(LIFE_COUNT BETWEEN 1 AND 3)",
        "STATE TEXT NOT NULL DEFAULT 'ALIVE' CHECK(STATE IN ('ALIVE', 'DEAD', 'REVIVING', 'DELETED'))",
    ] {
        if !table_sql.contains(marker) {
            return Err(format!("v6 迁移前角色表缺少旧版约束，已拒绝重建：{marker}"));
        }
    }

    let foreign_keys = connection
        .prepare("PRAGMA foreign_key_list(player)")
        .map_err(|error| format!("读取 v6 迁移前角色外键失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        .map_err(|error| format!("查询 v6 迁移前角色外键失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v6 迁移前角色外键失败：{error}"))?;
    if foreign_keys
        != [(
            "identity".to_string(),
            "identity_id".to_string(),
            "id".to_string(),
            "NO ACTION".to_string(),
            "CASCADE".to_string(),
            "NONE".to_string(),
        )]
    {
        return Err(format!(
            "v6 迁移前角色外键不匹配，已拒绝重建：{foreign_keys:?}"
        ));
    }

    let indexes = connection
        .prepare("PRAGMA index_list(player)")
        .map_err(|error| format!("读取 v6 迁移前角色索引失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .map_err(|error| format!("查询 v6 迁移前角色索引失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v6 迁移前角色索引失败：{error}"))?;
    if indexes.len() != 1 || !indexes[0].1 || indexes[0].3 || indexes[0].2 != "u" {
        return Err(format!(
            "v6 迁移前角色索引集合不匹配，已拒绝重建：{indexes:?}"
        ));
    }
    let index_name = &indexes[0].0;
    let escaped_index = index_name.replace('"', "\"\"");
    let index_columns = connection
        .prepare(&format!("PRAGMA index_info(\"{escaped_index}\")"))
        .map_err(|error| format!("读取 v6 迁移前角色索引字段失败：{error}"))?
        .query_map([], |row| row.get::<_, String>(2))
        .map_err(|error| format!("查询 v6 迁移前角色索引字段失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v6 迁移前角色索引字段失败：{error}"))?;
    if index_columns != ["identity_id"] {
        return Err(format!(
            "v6 迁移前角色唯一索引字段不匹配，已拒绝重建：{index_columns:?}"
        ));
    }

    let trigger_count = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'player'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("检查 v6 迁移前角色触发器失败：{error}"))?;
    if trigger_count != 0 {
        return Err("v6 迁移前角色存在未声明触发器，已拒绝重建".to_string());
    }
    Ok(())
}

fn validate_v6_schema(connection: &Connection) -> Result<(), String> {
    let columns = table_columns_with_type(connection, "player")?;
    let expected_columns = vec![
        TableColumnInfo::new("id", "INTEGER", false, true, None, 0),
        TableColumnInfo::new("identity_id", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("name", "TEXT", true, false, None, 0),
        TableColumnInfo::new("gender", "TEXT", true, false, None, 0),
        TableColumnInfo::new("level", "INTEGER", true, false, Some("1"), 0),
        TableColumnInfo::new("exp", "INTEGER", true, false, Some("0"), 0),
        TableColumnInfo::new("hp", "INTEGER", true, false, Some("100"), 0),
        TableColumnInfo::new("max_hp", "INTEGER", true, false, Some("100"), 0),
        TableColumnInfo::new("soul_power", "INTEGER", true, false, Some("50"), 0),
        TableColumnInfo::new("max_soul_power", "INTEGER", true, false, Some("50"), 0),
        TableColumnInfo::new("strength", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("agility", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("spirit", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("endurance", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("perception", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("luck", "INTEGER", true, false, Some("10"), 0),
        TableColumnInfo::new("life_count", "INTEGER", true, false, Some("1"), 0),
        TableColumnInfo::new("state", "TEXT", true, false, Some("'alive'"), 0),
        TableColumnInfo::new("map_name", "TEXT", true, false, Some("'圣魂村'"), 0),
        TableColumnInfo::new("created_at", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("updated_at", "INTEGER", true, false, None, 0),
    ];
    if columns != expected_columns {
        return Err(format!(
            "数据库已标记迁移 v6，但角色字段不匹配：{columns:?}"
        ));
    }

    let table_sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'player'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取 v6 角色建表语句失败：{error}"))?
        .ok_or_else(|| "数据库已标记迁移 v6，但缺少 player 表".to_string())?
        .to_ascii_uppercase();
    for marker in [
        "AUTOINCREMENT",
        "IDENTITY_ID INTEGER NOT NULL UNIQUE REFERENCES IDENTITY(ID) ON DELETE CASCADE",
        "GENDER TEXT NOT NULL CHECK(GENDER IN ('男', '女'))",
        "LEVEL INTEGER NOT NULL DEFAULT 1 CHECK(LEVEL BETWEEN 1 AND 120)",
        "EXP INTEGER NOT NULL DEFAULT 0 CHECK(EXP >= 0)",
        "LIFE_COUNT INTEGER NOT NULL DEFAULT 1 CHECK(LIFE_COUNT BETWEEN 1 AND 3)",
        "STATE TEXT NOT NULL DEFAULT 'ALIVE' CHECK(STATE IN ('ALIVE', 'DEAD', 'REVIVING', 'DELETED'))",
    ] {
        if !table_sql.contains(marker) {
            return Err(format!("数据库已标记迁移 v6，但角色表缺少约束：{marker}"));
        }
    }

    let mut foreign_keys = connection
        .prepare("PRAGMA foreign_key_list(player)")
        .map_err(|error| format!("读取 v6 角色外键失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        .map_err(|error| format!("查询 v6 角色外键失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v6 角色外键失败：{error}"))?;
    foreign_keys.sort();
    if foreign_keys
        != [(
            "identity".to_string(),
            "identity_id".to_string(),
            "id".to_string(),
            "NO ACTION".to_string(),
            "CASCADE".to_string(),
            "NONE".to_string(),
        )]
    {
        return Err(format!(
            "数据库已标记迁移 v6，但角色外键不匹配：{foreign_keys:?}"
        ));
    }

    let indexes = connection
        .prepare("PRAGMA index_list(player)")
        .map_err(|error| format!("读取 v6 角色索引失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .map_err(|error| format!("查询 v6 角色索引失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v6 角色索引失败：{error}"))?;
    if indexes.len() != 1 || !indexes[0].1 || indexes[0].3 || indexes[0].2 != "u" {
        return Err(format!(
            "数据库已标记迁移 v6，但角色索引集合不匹配：{indexes:?}"
        ));
    }
    let index_name = &indexes[0].0;
    let escaped_index = index_name.replace('"', "\"\"");
    let index_columns = connection
        .prepare(&format!("PRAGMA index_info(\"{escaped_index}\")"))
        .map_err(|error| format!("读取 v6 角色索引字段失败：{error}"))?
        .query_map([], |row| row.get::<_, String>(2))
        .map_err(|error| format!("查询 v6 角色索引字段失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v6 角色索引字段失败：{error}"))?;
    if index_columns != ["identity_id"] {
        return Err(format!(
            "数据库已标记迁移 v6，但角色唯一索引字段不匹配：{index_columns:?}"
        ));
    }

    let trigger_count = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'player'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("检查 v6 角色触发器失败：{error}"))?;
    if trigger_count != 0 {
        return Err("数据库已标记迁移 v6，但角色表存在未声明触发器".to_string());
    }
    let stale_table = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'player_v6')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| format!("检查 v6 临时角色表失败：{error}"))?;
    if stale_table {
        return Err("数据库已标记迁移 v6，但残留 player_v6 临时表".to_string());
    }

    probe_v6_player_level_guard(connection)
}

fn probe_v6_player_level_guard(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch("SAVEPOINT qimen_v6_level_probe;")
        .map_err(|error| format!("开始 v6 等级约束探针失败：{error}"))?;
    let probe_result = (|| -> Result<(), String> {
        let token = connection
            .query_row("SELECT lower(hex(randomblob(16)))", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("生成 v6 等级探针标识失败：{error}"))?;
        let account_id = format!("v6-level-probe-{token}");
        connection
            .execute(
                r#"
                INSERT INTO identity(
                    protocol, account_id, namespace, subject_kind, subject_id, created_at
                ) VALUES('onebot11', ?1, 'v6-level-probe', 'user', ?1, 0)
                "#,
                [&account_id],
            )
            .map_err(|error| format!("v6 等级探针无法创建临时身份：{error}"))?;
        let identity_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO player(identity_id, name, gender, level, created_at, updated_at) VALUES(?1, 'v6-level-probe', '男', 120, 0, 0)",
                [identity_id],
            )
            .map_err(|error| format!("v6 等级探针无法插入 120 级角色：{error}"))?;

        let invalid_account = format!("{account_id}-invalid");
        connection
            .execute(
                r#"
                INSERT INTO identity(
                    protocol, account_id, namespace, subject_kind, subject_id, created_at
                ) VALUES('onebot11', ?1, 'v6-level-probe', 'user', ?1, 0)
                "#,
                [&invalid_account],
            )
            .map_err(|error| format!("v6 等级探针无法创建边界身份：{error}"))?;
        let invalid_identity_id = connection.last_insert_rowid();
        if connection
            .execute(
                "INSERT INTO player(identity_id, name, gender, level, created_at, updated_at) VALUES(?1, 'v6-level-invalid', '男', 121, 0, 0)",
                [invalid_identity_id],
            )
            .is_ok()
        {
            return Err("v6 角色等级上限约束探针被绕过".to_string());
        }
        Ok(())
    })();
    let rollback_result = connection
        .execute_batch("ROLLBACK TO qimen_v6_level_probe; RELEASE qimen_v6_level_probe;")
        .map_err(|error| format!("回滚 v6 等级约束探针失败：{error}"));
    match (probe_result, rollback_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(probe_error), Err(rollback_error)) => Err(format!("{probe_error}；{rollback_error}")),
    }
}

fn validate_v7_schema(connection: &Connection) -> Result<(), String> {
    let map_columns = table_columns_with_type(connection, "map")?;
    let expected_map_columns = [
        ("map_key", "TEXT"),
        ("name", "TEXT"),
        ("description", "TEXT"),
        ("level_required", "INTEGER"),
        ("safe", "INTEGER"),
        ("pvp_enabled", "INTEGER"),
        ("teleport_enabled", "INTEGER"),
        ("sort_order", "INTEGER"),
        ("created_at", "INTEGER"),
        ("updated_at", "INTEGER"),
    ];
    validate_column_names_and_types("map", &map_columns, &expected_map_columns)?;
    let edge_columns = table_columns_with_type(connection, "map_edge")?;
    let expected_edge_columns = [
        ("id", "INTEGER"),
        ("from_map_key", "TEXT"),
        ("to_map_key", "TEXT"),
        ("travel_kind", "TEXT"),
        ("direction", "TEXT"),
        ("level_required", "INTEGER"),
        ("enabled", "INTEGER"),
        ("created_at", "INTEGER"),
    ];
    validate_column_names_and_types("map_edge", &edge_columns, &expected_edge_columns)?;
    let player_map_columns = table_columns_with_type(connection, "player_map")?;
    let expected_player_map_columns = [
        ("player_id", "INTEGER"),
        ("map_key", "TEXT"),
        ("updated_at", "INTEGER"),
    ];
    validate_column_names_and_types(
        "player_map",
        &player_map_columns,
        &expected_player_map_columns,
    )?;

    validate_v7_table_sql(
        connection,
        "map",
        &[
            ") STRICT",
            "MAP_KEY TEXT PRIMARY KEY",
            "LENGTH(MAP_KEY) BETWEEN 1 AND 96",
            "MAP_KEY = TRIM(MAP_KEY)",
            "MAP_KEY GLOB",
            "LEVEL_REQUIRED BETWEEN 1 AND 120",
            "SAFE IN (0, 1)",
            "PVP_ENABLED IN (0, 1)",
            "TELEPORT_ENABLED IN (0, 1)",
            "SORT_ORDER >= 0",
        ],
    )?;
    validate_v7_table_sql(
        connection,
        "map_edge",
        &[
            ") STRICT",
            "FROM_MAP_KEY TEXT NOT NULL REFERENCES MAP(MAP_KEY) ON DELETE CASCADE",
            "TO_MAP_KEY TEXT NOT NULL REFERENCES MAP(MAP_KEY) ON DELETE RESTRICT",
            "TRAVEL_KIND IN ('WALK', 'TELEPORT')",
            "TRAVEL_KIND = 'WALK' AND DIRECTION IN",
            "TRAVEL_KIND = 'TELEPORT' AND DIRECTION IS NULL",
            "FROM_MAP_KEY <> TO_MAP_KEY",
        ],
    )?;
    validate_v7_table_sql(
        connection,
        "player_map",
        &[
            ") STRICT",
            "PLAYER_ID INTEGER PRIMARY KEY REFERENCES PLAYER(ID) ON DELETE CASCADE",
            "MAP_KEY TEXT NOT NULL REFERENCES MAP(MAP_KEY) ON DELETE RESTRICT",
            "UPDATED_AT >= 0",
        ],
    )?;

    validate_v7_foreign_keys(
        connection,
        "map_edge",
        &[
            ("map", "from_map_key", "map_key", "NO ACTION", "CASCADE"),
            ("map", "to_map_key", "map_key", "NO ACTION", "RESTRICT"),
        ],
    )?;
    validate_v7_foreign_keys(
        connection,
        "player_map",
        &[
            ("map", "map_key", "map_key", "NO ACTION", "RESTRICT"),
            ("player", "player_id", "id", "NO ACTION", "CASCADE"),
        ],
    )?;
    validate_v7_index(connection, "map_name_unique", true, &["name"], false)?;
    validate_v7_index(
        connection,
        "map_sort_order_unique",
        true,
        &["sort_order"],
        false,
    )?;
    validate_v7_index(
        connection,
        "map_page",
        false,
        &["sort_order", "map_key"],
        false,
    )?;
    validate_v7_index(
        connection,
        "map_edge_walk_direction",
        true,
        &["from_map_key", "direction"],
        true,
    )?;
    validate_v7_index(
        connection,
        "map_edge_teleport_target",
        true,
        &["from_map_key", "to_map_key"],
        true,
    )?;
    validate_v7_index(
        connection,
        "map_edge_from_kind",
        false,
        &["from_map_key", "travel_kind", "enabled", "id"],
        false,
    )?;
    validate_v7_custom_index_set(
        connection,
        "map",
        &["map_name_unique", "map_page", "map_sort_order_unique"],
    )?;
    validate_v7_custom_index_set(
        connection,
        "map_edge",
        &[
            "map_edge_from_kind",
            "map_edge_teleport_target",
            "map_edge_walk_direction",
        ],
    )?;
    validate_v7_custom_index_set(connection, "player_map", &[])?;
    validate_v7_triggers(connection)?;

    let map_count = connection
        .query_row("SELECT COUNT(*) FROM map", [], |row| row.get::<_, i64>(0))
        .map_err(|error| format!("统计 v7 地图种子失败：{error}"))?;
    if map_count < 7 {
        return Err(format!(
            "数据库已标记迁移 v7，但地图种子数量不足：{map_count}"
        ));
    }
    let mismatched_player_maps = connection
        .query_row(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM player p
                 LEFT JOIN player_map pm ON pm.player_id = p.id
                 LEFT JOIN map m ON m.map_key = pm.map_key
                WHERE pm.player_id IS NULL OR m.name IS NULL OR m.name <> p.map_name
            )
            "#,
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| format!("检查角色地图绑定失败：{error}"))?;
    if mismatched_player_maps {
        return Err("数据库已标记迁移 v7，但存在未绑定或名称不一致的角色地图".to_string());
    }
    probe_v7_map_guards(connection)
}

// v8 独立校验经济表元数据、固定种子和代表性约束行为，损坏时拒绝启动。
fn validate_v8_schema(connection: &Connection) -> Result<(), String> {
    validate_column_names_and_types(
        "item",
        &table_columns_with_type(connection, "item")?,
        &[
            ("item_key", "TEXT"),
            ("name", "TEXT"),
            ("category", "TEXT"),
            ("quality", "INTEGER"),
            ("stackable", "INTEGER"),
            ("max_stack", "INTEGER"),
            ("buy_price", "INTEGER"),
            ("sell_price", "INTEGER"),
            ("level_required", "INTEGER"),
            ("effect_kind", "TEXT"),
            ("effect_amount", "INTEGER"),
            ("revive_hp_percent", "INTEGER"),
            ("purchasable", "INTEGER"),
            ("sellable", "INTEGER"),
            ("usable", "INTEGER"),
            ("description", "TEXT"),
            ("created_at", "INTEGER"),
            ("updated_at", "INTEGER"),
        ],
    )?;
    validate_column_names_and_types(
        "inventory",
        &table_columns_with_type(connection, "inventory")?,
        &[
            ("player_id", "INTEGER"),
            ("item_key", "TEXT"),
            ("quantity", "INTEGER"),
            ("updated_at", "INTEGER"),
        ],
    )?;
    validate_column_names_and_types(
        "npc",
        &table_columns_with_type(connection, "npc")?,
        &[
            ("npc_key", "TEXT"),
            ("map_key", "TEXT"),
            ("name", "TEXT"),
            ("npc_kind", "TEXT"),
            ("dialogue", "TEXT"),
            ("description", "TEXT"),
            ("enabled", "INTEGER"),
            ("sort_order", "INTEGER"),
            ("created_at", "INTEGER"),
            ("updated_at", "INTEGER"),
        ],
    )?;
    validate_column_names_and_types(
        "shop_item",
        &table_columns_with_type(connection, "shop_item")?,
        &[
            ("npc_key", "TEXT"),
            ("item_key", "TEXT"),
            ("buy_price", "INTEGER"),
            ("stock", "INTEGER"),
            ("enabled", "INTEGER"),
            ("created_at", "INTEGER"),
            ("updated_at", "INTEGER"),
        ],
    )?;
    validate_column_names_and_types(
        "player_npc",
        &table_columns_with_type(connection, "player_npc")?,
        &[
            ("player_id", "INTEGER"),
            ("npc_key", "TEXT"),
            ("updated_at", "INTEGER"),
        ],
    )?;

    validate_v8_table_sql(
        connection,
        "item",
        &[
            ") STRICT",
            "ITEM_KEY TEXT PRIMARY KEY",
            "LENGTH(ITEM_KEY) BETWEEN 1 AND 96",
            "ITEM_KEY = TRIM(ITEM_KEY)",
            "ITEM_KEY GLOB",
            "NAME TEXT NOT NULL CHECK(LENGTH(NAME) BETWEEN 1 AND 128)",
            "CATEGORY TEXT NOT NULL CHECK(CATEGORY IN ('REVIVAL', 'CONSUMABLE'))",
            "QUALITY INTEGER NOT NULL CHECK(QUALITY BETWEEN 1 AND 5)",
            "STACKABLE INTEGER NOT NULL CHECK(STACKABLE IN (0, 1))",
            "MAX_STACK INTEGER NOT NULL CHECK(MAX_STACK BETWEEN 1 AND 9999)",
            "BUY_PRICE INTEGER NOT NULL CHECK(BUY_PRICE >= 0)",
            "SELL_PRICE INTEGER NOT NULL CHECK(SELL_PRICE >= 0 AND SELL_PRICE <= BUY_PRICE)",
            "LEVEL_REQUIRED INTEGER NOT NULL DEFAULT 1 CHECK(LEVEL_REQUIRED BETWEEN 1 AND 120)",
            "EFFECT_KIND TEXT NOT NULL CHECK(EFFECT_KIND IN ('REVIVE', 'RESTORE_HP', 'RESTORE_SOUL'))",
            "EFFECT_AMOUNT INTEGER NOT NULL DEFAULT 0 CHECK(EFFECT_AMOUNT >= 0)",
            "REVIVE_HP_PERCENT INTEGER NOT NULL DEFAULT 0 CHECK(REVIVE_HP_PERCENT BETWEEN 0 AND 100)",
            "PURCHASABLE INTEGER NOT NULL DEFAULT 1 CHECK(PURCHASABLE IN (0, 1))",
            "SELLABLE INTEGER NOT NULL DEFAULT 1 CHECK(SELLABLE IN (0, 1))",
            "USABLE INTEGER NOT NULL DEFAULT 1 CHECK(USABLE IN (0, 1))",
            "DESCRIPTION TEXT NOT NULL DEFAULT '' CHECK(LENGTH(DESCRIPTION) <= 2000)",
            "CREATED_AT INTEGER NOT NULL CHECK(CREATED_AT >= 0)",
            "UPDATED_AT INTEGER NOT NULL CHECK(UPDATED_AT >= 0)",
            "STACKABLE = 1 OR MAX_STACK = 1",
            "EFFECT_KIND = 'REVIVE' AND EFFECT_AMOUNT = 0 AND REVIVE_HP_PERCENT > 0",
            "EFFECT_KIND IN ('RESTORE_HP', 'RESTORE_SOUL')",
        ],
    )?;
    validate_v8_table_sql(
        connection,
        "inventory",
        &[
            ") STRICT",
            "PLAYER_ID INTEGER NOT NULL REFERENCES PLAYER(ID) ON DELETE CASCADE",
            "ITEM_KEY TEXT NOT NULL REFERENCES ITEM(ITEM_KEY) ON DELETE RESTRICT",
            "QUANTITY INTEGER NOT NULL CHECK(QUANTITY BETWEEN 1 AND 999999)",
            "UPDATED_AT INTEGER NOT NULL CHECK(UPDATED_AT >= 0)",
            "PRIMARY KEY(PLAYER_ID, ITEM_KEY)",
        ],
    )?;
    validate_v8_table_sql(
        connection,
        "npc",
        &[
            ") STRICT",
            "NPC_KEY TEXT PRIMARY KEY",
            "LENGTH(NPC_KEY) BETWEEN 1 AND 96",
            "NPC_KEY = TRIM(NPC_KEY)",
            "NPC_KEY GLOB",
            "MAP_KEY TEXT NOT NULL REFERENCES MAP(MAP_KEY) ON DELETE RESTRICT",
            "NAME TEXT NOT NULL CHECK(LENGTH(NAME) BETWEEN 1 AND 128)",
            "NPC_KIND TEXT NOT NULL CHECK(NPC_KIND IN ('ELDER', 'MERCHANT'))",
            "DIALOGUE TEXT NOT NULL DEFAULT '' CHECK(LENGTH(DIALOGUE) <= 2000)",
            "DESCRIPTION TEXT NOT NULL DEFAULT '' CHECK(LENGTH(DESCRIPTION) <= 2000)",
            "ENABLED INTEGER NOT NULL DEFAULT 1 CHECK(ENABLED IN (0, 1))",
            "SORT_ORDER INTEGER NOT NULL DEFAULT 0 CHECK(SORT_ORDER >= 0)",
            "CREATED_AT INTEGER NOT NULL CHECK(CREATED_AT >= 0)",
            "UPDATED_AT INTEGER NOT NULL CHECK(UPDATED_AT >= 0)",
        ],
    )?;
    validate_v8_table_sql(
        connection,
        "shop_item",
        &[
            ") STRICT",
            "NPC_KEY TEXT NOT NULL REFERENCES NPC(NPC_KEY) ON DELETE CASCADE",
            "ITEM_KEY TEXT NOT NULL REFERENCES ITEM(ITEM_KEY) ON DELETE RESTRICT",
            "BUY_PRICE INTEGER NOT NULL CHECK(BUY_PRICE >= 0)",
            "STOCK INTEGER NOT NULL DEFAULT -1 CHECK(STOCK = -1 OR STOCK >= 0)",
            "ENABLED INTEGER NOT NULL DEFAULT 1 CHECK(ENABLED IN (0, 1))",
            "CREATED_AT INTEGER NOT NULL CHECK(CREATED_AT >= 0)",
            "UPDATED_AT INTEGER NOT NULL CHECK(UPDATED_AT >= 0)",
            "PRIMARY KEY(NPC_KEY, ITEM_KEY)",
        ],
    )?;
    validate_v8_table_sql(
        connection,
        "player_npc",
        &[
            ") STRICT",
            "PLAYER_ID INTEGER PRIMARY KEY REFERENCES PLAYER(ID) ON DELETE CASCADE",
            "NPC_KEY TEXT NOT NULL REFERENCES NPC(NPC_KEY) ON DELETE RESTRICT",
            "UPDATED_AT INTEGER NOT NULL CHECK(UPDATED_AT >= 0)",
        ],
    )?;

    validate_v8_foreign_keys(
        connection,
        "inventory",
        &[
            ("item", "item_key", "item_key", "NO ACTION", "RESTRICT"),
            ("player", "player_id", "id", "NO ACTION", "CASCADE"),
        ],
    )?;
    validate_v8_foreign_keys(
        connection,
        "npc",
        &[("map", "map_key", "map_key", "NO ACTION", "RESTRICT")],
    )?;
    validate_v8_foreign_keys(
        connection,
        "shop_item",
        &[
            ("item", "item_key", "item_key", "NO ACTION", "RESTRICT"),
            ("npc", "npc_key", "npc_key", "NO ACTION", "CASCADE"),
        ],
    )?;
    validate_v8_foreign_keys(
        connection,
        "player_npc",
        &[
            ("npc", "npc_key", "npc_key", "NO ACTION", "RESTRICT"),
            ("player", "player_id", "id", "NO ACTION", "CASCADE"),
        ],
    )?;

    validate_named_index(connection, "item", "item_name_unique", true, &["name"])?;
    validate_named_index(
        connection,
        "inventory",
        "inventory_player_page",
        false,
        &["player_id", "item_key"],
    )?;
    validate_named_index(
        connection,
        "npc",
        "npc_map_name_unique",
        true,
        &["map_key", "name"],
    )?;
    validate_named_index(
        connection,
        "npc",
        "npc_map_page",
        false,
        &["map_key", "enabled", "sort_order", "npc_key"],
    )?;
    validate_named_index(
        connection,
        "shop_item",
        "shop_item_npc_page",
        false,
        &["npc_key", "enabled", "item_key"],
    )?;
    validate_v8_custom_index_set(connection, "item", &["item_name_unique"])?;
    validate_v8_custom_index_set(connection, "inventory", &["inventory_player_page"])?;
    validate_v8_custom_index_set(connection, "npc", &["npc_map_name_unique", "npc_map_page"])?;
    validate_v8_custom_index_set(connection, "shop_item", &["shop_item_npc_page"])?;
    validate_v8_custom_index_set(connection, "player_npc", &[])?;
    validate_v8_triggers(connection)?;

    validate_v8_seed_rows(connection)?;
    probe_v8_economy_guards(connection)
}

fn validate_v8_seed_rows(connection: &Connection) -> Result<(), String> {
    let expected_items = [
        (
            "revival-grass",
            "复活草",
            "revival",
            2,
            1,
            1000,
            500,
            10,
            1,
            "revive",
            0,
            30,
            0,
            0,
            0,
            "使用后可以复活，恢复30%生命值",
        ),
        (
            "nine-leaf-zhi-grass",
            "九叶芝草",
            "revival",
            4,
            1,
            10000,
            5000,
            5,
            1,
            "revive",
            0,
            100,
            0,
            0,
            0,
            "传说中的仙草，使用后满血复活",
        ),
        (
            "small-healing-potion",
            "小回复药",
            "consumable",
            1,
            1,
            10,
            2,
            99,
            1,
            "restore_hp",
            50,
            0,
            1,
            1,
            1,
            "恢复50点生命值",
        ),
        (
            "medium-healing-potion",
            "中回复药",
            "consumable",
            2,
            1,
            50,
            10,
            99,
            10,
            "restore_hp",
            200,
            0,
            1,
            1,
            1,
            "恢复200点生命值",
        ),
        (
            "soul-power-potion",
            "魂力恢复药",
            "consumable",
            2,
            1,
            30,
            6,
            99,
            1,
            "restore_soul",
            100,
            0,
            1,
            1,
            1,
            "恢复100点魂力值",
        ),
    ];
    for (
        key,
        name,
        category,
        quality,
        stackable,
        buy,
        sell,
        max_stack,
        level,
        effect,
        amount,
        percent,
        purchasable,
        sellable,
        usable,
        description,
    ) in expected_items
    {
        let matches_contract = connection
            .query_row(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM item
                     WHERE item_key = ?1 AND name = ?2 AND category = ?3
                       AND quality = ?4 AND stackable = ?5 AND buy_price = ?6
                       AND sell_price = ?7 AND max_stack = ?8 AND level_required = ?9
                       AND effect_kind = ?10 AND effect_amount = ?11
                       AND revive_hp_percent = ?12 AND purchasable = ?13
                       AND sellable = ?14 AND usable = ?15 AND description = ?16
                )
                "#,
                params![
                    key,
                    name,
                    category,
                    quality,
                    stackable,
                    buy,
                    sell,
                    max_stack,
                    level,
                    effect,
                    amount,
                    percent,
                    purchasable,
                    sellable,
                    usable,
                    description
                ],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("读取 v8 物品种子失败：{error}"))?;
        if !matches_contract {
            return Err(format!("数据库已标记迁移 v8，但物品种子 {name} 不匹配"));
        }
    }

    let expected_npcs = [
        (
            "holy-soul-village-chief",
            "holy-soul-village",
            "村长",
            "elder",
            "年轻人，欢迎来到圣魂村。愿你在魂师之路上平安成长。",
            "圣魂村的村长，可以接待初来乍到的魂师。",
            1,
            10,
        ),
        (
            "holy-soul-village-grocer",
            "holy-soul-village",
            "杂货商人",
            "merchant",
            "看看吧，这里有旅途中用得上的药剂。",
            "出售基础恢复药剂。",
            1,
            20,
        ),
    ];
    for (key, map_key, name, kind, dialogue, description, enabled, sort_order) in expected_npcs {
        let matches_contract = connection
            .query_row(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM npc
                     WHERE npc_key = ?1 AND map_key = ?2 AND name = ?3
                       AND npc_kind = ?4 AND dialogue = ?5 AND description = ?6
                       AND enabled = ?7 AND sort_order = ?8
                )
                "#,
                params![
                    key,
                    map_key,
                    name,
                    kind,
                    dialogue,
                    description,
                    enabled,
                    sort_order
                ],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("读取 v8 NPC 种子失败：{error}"))?;
        if !matches_contract {
            return Err(format!("数据库已标记迁移 v8，但 NPC 种子 {name} 不匹配"));
        }
    }

    let expected_shop_items = [
        ("small-healing-potion", 10, -1, 1),
        ("medium-healing-potion", 50, -1, 1),
        ("soul-power-potion", 30, -1, 1),
    ];
    for (item_key, buy_price, stock, enabled) in expected_shop_items {
        let matches_contract = connection
            .query_row(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM shop_item
                     WHERE npc_key = 'holy-soul-village-grocer'
                       AND item_key = ?1 AND buy_price = ?2
                       AND stock = ?3 AND enabled = ?4
                )
                "#,
                params![item_key, buy_price, stock, enabled],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("读取 v8 商店种子失败：{error}"))?;
        if !matches_contract {
            return Err(format!(
                "数据库已标记迁移 v8，但杂货商人商品种子 {item_key} 不匹配"
            ));
        }
    }
    let revival_listed = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM shop_item si JOIN item i ON i.item_key = si.item_key WHERE i.category = 'revival')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| format!("检查 v8 复活物品上架状态失败：{error}"))?;
    if revival_listed {
        return Err("数据库已标记迁移 v8，但复活物品被错误上架".to_string());
    }
    Ok(())
}

fn validate_v8_table_sql(
    connection: &Connection,
    table: &str,
    markers: &[&str],
) -> Result<(), String> {
    let sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取 v8 表 {table} 建表语句失败：{error}"))?
        .ok_or_else(|| format!("数据库已标记迁移 v8，但缺少表 {table}"))?
        .to_ascii_uppercase();
    for marker in markers {
        if !sql.contains(marker) {
            return Err(format!(
                "数据库已标记迁移 v8，但表 {table} 缺少约束：{marker}"
            ));
        }
    }
    Ok(())
}

fn validate_v8_foreign_keys(
    connection: &Connection,
    table: &str,
    expected: &[(&str, &str, &str, &str, &str)],
) -> Result<(), String> {
    let escaped_table = table.replace('"', "\"\"");
    let mut actual = connection
        .prepare(&format!("PRAGMA foreign_key_list(\"{escaped_table}\")"))
        .map_err(|error| format!("读取 v8 表 {table} 外键失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|error| format!("查询 v8 表 {table} 外键失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v8 表 {table} 外键失败：{error}"))?;
    let mut expected = expected
        .iter()
        .map(|(parent, from, to, update, delete)| {
            (
                (*parent).to_string(),
                (*from).to_string(),
                (*to).to_string(),
                (*update).to_string(),
                (*delete).to_string(),
            )
        })
        .collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(format!(
            "数据库已标记迁移 v8，但表 {table} 外键不匹配：实际 {actual:?}，期望 {expected:?}"
        ));
    }
    Ok(())
}

fn validate_v8_custom_index_set(
    connection: &Connection,
    table: &str,
    expected: &[&str],
) -> Result<(), String> {
    let escaped_table = table.replace('"', "\"\"");
    let mut actual = connection
        .prepare(&format!("PRAGMA index_list(\"{escaped_table}\")"))
        .map_err(|error| format!("读取 v8 表 {table} 索引集合失败：{error}"))?
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(3)?))
        })
        .map_err(|error| format!("查询 v8 表 {table} 索引集合失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v8 表 {table} 索引集合失败：{error}"))?
        .into_iter()
        .filter_map(|(name, origin)| (origin == "c").then_some(name))
        .collect::<Vec<_>>();
    let mut expected = expected
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(format!(
            "数据库已标记迁移 v8，但表 {table} 自定义索引集合不匹配：实际 {actual:?}，期望 {expected:?}"
        ));
    }
    Ok(())
}

fn validate_v8_triggers(connection: &Connection) -> Result<(), String> {
    let expected = [
        ("inventory_item_stack_insert", "inventory"),
        ("inventory_item_stack_update", "inventory"),
        ("shop_item_revival_insert", "shop_item"),
        ("shop_item_revival_update", "shop_item"),
        ("item_shop_contract_update", "item"),
        ("item_inventory_stack_update", "item"),
    ];
    for (name, table) in expected {
        let (actual_table, sql) = connection
            .query_row(
                "SELECT tbl_name, sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
                [name],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| format!("读取 v8 触发器 {name} 失败：{error}"))?
            .ok_or_else(|| format!("数据库已标记迁移 v8，但缺少触发器 {name}"))?;
        let normalized = sql.to_ascii_uppercase();
        let markers: &[&str] = match name {
            "inventory_item_stack_insert" => &[
                "BEFORE INSERT ON INVENTORY",
                "NEW.QUANTITY > COALESCE",
                "SELECT MAX_STACK FROM ITEM WHERE ITEM_KEY = NEW.ITEM_KEY",
                "RAISE(ABORT",
            ],
            "inventory_item_stack_update" => &[
                "BEFORE UPDATE OF ITEM_KEY, QUANTITY ON INVENTORY",
                "NEW.QUANTITY > COALESCE",
                "SELECT MAX_STACK FROM ITEM WHERE ITEM_KEY = NEW.ITEM_KEY",
                "RAISE(ABORT",
            ],
            "shop_item_revival_insert" => &[
                "BEFORE INSERT ON SHOP_ITEM",
                "CATEGORY = 'REVIVAL'",
                "PURCHASABLE = 0",
                "USABLE = 0",
                "NEW.BUY_PRICE < SELL_PRICE",
                "RAISE(ABORT",
            ],
            "shop_item_revival_update" => &[
                "BEFORE UPDATE OF ITEM_KEY, BUY_PRICE ON SHOP_ITEM",
                "CATEGORY = 'REVIVAL'",
                "PURCHASABLE = 0",
                "USABLE = 0",
                "NEW.BUY_PRICE < SELL_PRICE",
                "RAISE(ABORT",
            ],
            "item_shop_contract_update" => &[
                "BEFORE UPDATE OF CATEGORY, PURCHASABLE, USABLE, SELL_PRICE ON ITEM",
                "SHOP_ITEM.ITEM_KEY = OLD.ITEM_KEY",
                "SHOP_ITEM.BUY_PRICE < NEW.SELL_PRICE",
                "RAISE(ABORT",
            ],
            "item_inventory_stack_update" => &[
                "BEFORE UPDATE OF MAX_STACK ON ITEM",
                "INVENTORY.ITEM_KEY = OLD.ITEM_KEY",
                "INVENTORY.QUANTITY > NEW.MAX_STACK",
                "RAISE(ABORT",
            ],
            _ => return Err(format!("未知的 v8 触发器契约：{name}")),
        };
        if actual_table != table || markers.iter().any(|marker| !normalized.contains(marker)) {
            return Err(format!("数据库已标记迁移 v8，但触发器 {name} 契约不匹配"));
        }
    }
    let triggers = connection
        .prepare(
            "SELECT name, tbl_name, sql FROM sqlite_master WHERE type = 'trigger' ORDER BY name",
        )
        .map_err(|error| format!("读取 v8 全库触发器集合失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| format!("查询 v8 全库触发器集合失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v8 全库触发器集合失败：{error}"))?;
    for (name, table, sql) in triggers {
        // v9 在后续迁移中新增自己的触发器；其中 asset_kind 的合法值“item”
        // 不应被 v8 的简单标识扫描误判为 v8 的 item 表引用。旧表上的跨 v9
        // 引用仍由 validate_v9_triggers 全库扫描拒绝。
        if matches!(table.as_str(), "asset_transfer" | "item_transfer_policy") {
            continue;
        }
        let declared = expected.iter().any(|(expected_name, expected_table)| {
            name == *expected_name && table == *expected_table
        });
        let touches_v8 = matches!(
            table.as_str(),
            "item" | "inventory" | "npc" | "shop_item" | "player_npc"
        ) || ["item", "inventory", "npc", "shop_item", "player_npc"]
            .iter()
            .any(|identifier| sql_mentions_identifier(&sql, identifier));
        if touches_v8 && !declared {
            return Err(format!(
                "数据库已标记迁移 v8，但触发器 {name}（目标表 {table}）未声明却引用了经济表"
            ));
        }
    }
    Ok(())
}

fn probe_v8_economy_guards(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch("SAVEPOINT qimen_v8_economy_probe;")
        .map_err(|error| format!("开始 v8 经济约束探针失败：{error}"))?;
    let probe_result = (|| -> Result<(), String> {
        let token = connection
            .query_row("SELECT lower(hex(randomblob(16)))", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("生成 v8 经济探针标识失败：{error}"))?;
        let account_id = format!("v8-economy-probe-{token}");
        connection
            .execute(
                "INSERT INTO identity(protocol, account_id, namespace, subject_kind, subject_id, created_at) VALUES('onebot11', ?1, 'v8-probe', 'user', ?1, 0)",
                [&account_id],
            )
            .map_err(|error| format!("v8 经济探针无法创建身份：{error}"))?;
        let identity_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO player(identity_id, name, gender, created_at, updated_at) VALUES(?1, 'v8-probe', '男', 0, 0)",
                [identity_id],
            )
            .map_err(|error| format!("v8 经济探针无法创建角色：{error}"))?;
        let player_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO player_map(player_id, map_key, updated_at) VALUES(?1, 'holy-soul-village', 0)",
                [player_id],
            )
            .map_err(|error| format!("v8 经济探针无法绑定地图：{error}"))?;
        connection
            .execute(
                r#"INSERT INTO item(
                    item_key, name, category, quality, stackable, max_stack,
                    buy_price, sell_price, level_required, effect_kind, effect_amount,
                    revive_hp_percent, purchasable, sellable, usable, description,
                    created_at, updated_at
                ) VALUES('v8-probe-item', 'v8-probe-item', 'consumable', 1, 1, 2,
                         1, 0, 1, 'restore_hp', 1, 0, 1, 1, 1, '', 0, 0)"#,
                [],
            )
            .map_err(|error| format!("v8 经济探针无法插入合法物品：{error}"))?;
        connection
            .execute(
                "INSERT INTO inventory(player_id, item_key, quantity, updated_at) VALUES(?1, 'v8-probe-item', 2, 0)",
                [player_id],
            )
            .map_err(|error| format!("v8 经济探针无法插入合法背包：{error}"))?;
        if connection
            .execute(
                "UPDATE inventory SET quantity = 3 WHERE player_id = ?1 AND item_key = 'v8-probe-item'",
                [player_id],
            )
            .is_ok()
        {
            return Err("v8 max_stack 更新触发器探针被绕过".to_string());
        }
        connection
            .execute(
                "DELETE FROM inventory WHERE player_id = ?1 AND item_key = 'v8-probe-item'",
                [player_id],
            )
            .map_err(|error| format!("v8 经济探针无法重置背包：{error}"))?;
        if connection
            .execute(
                "INSERT INTO inventory(player_id, item_key, quantity, updated_at) VALUES(?1, 'v8-probe-item', 3, 0)",
                [player_id],
            )
            .is_ok()
        {
            return Err("v8 max_stack 插入触发器探针被绕过".to_string());
        }
        connection
            .execute(
                "INSERT INTO inventory(player_id, item_key, quantity, updated_at) VALUES(?1, 'v8-probe-item', 2, 0)",
                [player_id],
            )
            .map_err(|error| format!("v8 经济探针无法恢复合法背包：{error}"))?;
        if connection
            .execute(
                "UPDATE item SET max_stack = 1 WHERE item_key = 'v8-probe-item'",
                [],
            )
            .is_ok()
        {
            return Err("v8 物品 max_stack 更新保护探针被绕过".to_string());
        }
        if connection
            .execute(
                "INSERT INTO shop_item(npc_key, item_key, buy_price, stock, enabled, created_at, updated_at) VALUES('holy-soul-village-grocer', 'revival-grass', 1000, -1, 1, 0, 0)",
                [],
            )
            .is_ok()
        {
            return Err("v8 复活物品上架触发器探针被绕过".to_string());
        }
        if connection
            .execute(
                "UPDATE item SET purchasable = 0 WHERE item_key = 'small-healing-potion'",
                [],
            )
            .is_ok()
        {
            return Err("v8 商店物品可购买性保护探针被绕过".to_string());
        }
        if connection
            .execute(
                "UPDATE shop_item SET buy_price = 1 WHERE npc_key = 'holy-soul-village-grocer' AND item_key = 'small-healing-potion'",
                [],
            )
            .is_ok()
        {
            return Err("v8 商店买价低于回收价保护探针被绕过".to_string());
        }
        if connection
            .execute(
                "UPDATE item SET buy_price = 20, sell_price = 11 WHERE item_key = 'small-healing-potion'",
                [],
            )
            .is_ok()
        {
            return Err("v8 物品回收价破坏商店价格保护探针被绕过".to_string());
        }
        Ok(())
    })();
    let rollback_result = connection
        .execute_batch("ROLLBACK TO qimen_v8_economy_probe; RELEASE qimen_v8_economy_probe;")
        .map_err(|error| format!("回滚 v8 经济约束探针失败：{error}"));
    match (probe_result, rollback_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(probe_error), Err(rollback_error)) => Err(format!("{probe_error}；{rollback_error}")),
    }
}

fn validate_v9_schema(connection: &Connection) -> Result<(), String> {
    let policy_columns = table_columns_with_type(connection, "item_transfer_policy")?;
    let expected_policy_columns = vec![
        TableColumnInfo::new("item_key", "TEXT", true, true, None, 0),
        TableColumnInfo::new("transferable", "INTEGER", true, false, Some("0"), 0),
        TableColumnInfo::new("created_at", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("updated_at", "INTEGER", true, false, None, 0),
    ];
    if policy_columns != expected_policy_columns {
        return Err(format!(
            "数据库已标记迁移 v9，但物品转移策略字段不匹配：{policy_columns:?}"
        ));
    }

    let transfer_columns = table_columns_with_type(connection, "asset_transfer")?;
    let expected_transfer_columns = vec![
        TableColumnInfo::new("id", "INTEGER", false, true, None, 0),
        TableColumnInfo::new("protocol", "TEXT", true, false, None, 0),
        TableColumnInfo::new("account_id", "TEXT", true, false, None, 0),
        TableColumnInfo::new("namespace", "TEXT", true, false, None, 0),
        TableColumnInfo::new("sender_identity_id", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("recipient_identity_id", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("sender_subject_id", "TEXT", true, false, None, 0),
        TableColumnInfo::new("recipient_subject_id", "TEXT", true, false, None, 0),
        TableColumnInfo::new("asset_kind", "TEXT", true, false, None, 0),
        TableColumnInfo::new("currency_code", "TEXT", false, false, None, 0),
        TableColumnInfo::new("item_key", "TEXT", false, false, None, 0),
        TableColumnInfo::new("amount", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("sender_before", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("sender_after", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("recipient_before", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("recipient_after", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("source_message_id", "TEXT", true, false, None, 0),
        TableColumnInfo::new("operation_log_id", "INTEGER", true, false, None, 0),
        TableColumnInfo::new("created_at", "INTEGER", true, false, None, 0),
    ];
    if transfer_columns != expected_transfer_columns {
        return Err(format!(
            "数据库已标记迁移 v9，但资产转移账本字段不匹配：{transfer_columns:?}"
        ));
    }

    validate_v9_table_sql(
        connection,
        "item_transfer_policy",
        &[
            ") STRICT",
            "ITEM_KEY TEXT PRIMARY KEY REFERENCES ITEM(ITEM_KEY) ON DELETE CASCADE",
            "TRANSFERABLE INTEGER NOT NULL DEFAULT 0 CHECK(TRANSFERABLE IN (0, 1))",
            "CREATED_AT INTEGER NOT NULL CHECK(CREATED_AT >= 0)",
            "UPDATED_AT INTEGER NOT NULL CHECK(UPDATED_AT >= 0)",
        ],
    )?;
    validate_v9_table_sql(
        connection,
        "asset_transfer",
        &[
            "AUTOINCREMENT",
            ") STRICT",
            "PROTOCOL IN ('ONEBOT11', 'QQ-OFFICIAL')",
            "SENDER_IDENTITY_ID INTEGER NOT NULL REFERENCES IDENTITY(ID) ON DELETE RESTRICT",
            "RECIPIENT_IDENTITY_ID INTEGER NOT NULL REFERENCES IDENTITY(ID) ON DELETE RESTRICT",
            "ITEM_KEY TEXT REFERENCES ITEM(ITEM_KEY) ON DELETE RESTRICT",
            "OPERATION_LOG_ID INTEGER NOT NULL REFERENCES OPERATION_LOG(ID) ON DELETE RESTRICT",
            "ASSET_KIND TEXT NOT NULL CHECK(ASSET_KIND IN ('CURRENCY', 'ITEM'))",
            "SOURCE_MESSAGE_ID TEXT NOT NULL CHECK(",
            "LENGTH(SOURCE_MESSAGE_ID) BETWEEN 1 AND 256",
            "INSTR(SOURCE_MESSAGE_ID, CHAR(0)) = 0",
            "SENDER_IDENTITY_ID <> RECIPIENT_IDENTITY_ID",
            "SENDER_SUBJECT_ID <> RECIPIENT_SUBJECT_ID",
            "SENDER_BEFORE >= AMOUNT AND SENDER_AFTER = SENDER_BEFORE - AMOUNT",
            "RECIPIENT_AFTER > RECIPIENT_BEFORE AND RECIPIENT_AFTER - RECIPIENT_BEFORE = AMOUNT",
            "CURRENCY_CODE = 'GOLD_SOUL_COIN' AND ITEM_KEY IS NULL",
            "CURRENCY_CODE IS NULL AND ITEM_KEY IS NOT NULL",
        ],
    )?;

    validate_v9_foreign_keys(
        connection,
        "item_transfer_policy",
        &[("item", "item_key", "item_key", "NO ACTION", "CASCADE")],
    )?;
    validate_v9_foreign_keys(
        connection,
        "asset_transfer",
        &[
            (
                "identity",
                "recipient_identity_id",
                "id",
                "NO ACTION",
                "RESTRICT",
            ),
            (
                "identity",
                "sender_identity_id",
                "id",
                "NO ACTION",
                "RESTRICT",
            ),
            ("item", "item_key", "item_key", "NO ACTION", "RESTRICT"),
            (
                "operation_log",
                "operation_log_id",
                "id",
                "NO ACTION",
                "RESTRICT",
            ),
        ],
    )?;
    validate_v9_custom_index_set(connection, "item_transfer_policy", &[])?;
    validate_v9_custom_index_set(
        connection,
        "asset_transfer",
        &[
            "asset_transfer_operation_log",
            "asset_transfer_recipient_page",
            "asset_transfer_sender_message",
            "asset_transfer_sender_page",
        ],
    )?;
    validate_named_index(
        connection,
        "asset_transfer",
        "asset_transfer_sender_message",
        true,
        &["sender_identity_id", "source_message_id"],
    )?;
    validate_named_index(
        connection,
        "asset_transfer",
        "asset_transfer_operation_log",
        true,
        &["operation_log_id"],
    )?;
    validate_named_index(
        connection,
        "asset_transfer",
        "asset_transfer_sender_page",
        false,
        &["sender_identity_id", "id"],
    )?;
    validate_named_index(
        connection,
        "asset_transfer",
        "asset_transfer_recipient_page",
        false,
        &["recipient_identity_id", "id"],
    )?;
    validate_v9_triggers(connection)?;
    validate_v9_policy_seeds(connection)?;
    probe_v9_transfer_guards(connection)
}

fn validate_v9_table_sql(
    connection: &Connection,
    table: &str,
    markers: &[&str],
) -> Result<(), String> {
    let sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取 v9 表 {table} 建表语句失败：{error}"))?
        .ok_or_else(|| format!("数据库已标记迁移 v9，但缺少表 {table}"))?
        .to_ascii_uppercase();
    for marker in markers {
        if !sql.contains(marker) {
            return Err(format!(
                "数据库已标记迁移 v9，但表 {table} 缺少约束：{marker}"
            ));
        }
    }
    Ok(())
}

fn validate_v9_foreign_keys(
    connection: &Connection,
    table: &str,
    expected: &[(&str, &str, &str, &str, &str)],
) -> Result<(), String> {
    let escaped_table = table.replace('"', "\"\"");
    let mut actual = connection
        .prepare(&format!("PRAGMA foreign_key_list(\"{escaped_table}\")"))
        .map_err(|error| format!("读取 v9 表 {table} 外键失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|error| format!("查询 v9 表 {table} 外键失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v9 表 {table} 外键失败：{error}"))?;
    let mut expected = expected
        .iter()
        .map(|(parent, from, to, update, delete)| {
            (
                (*parent).to_string(),
                (*from).to_string(),
                (*to).to_string(),
                (*update).to_string(),
                (*delete).to_string(),
            )
        })
        .collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(format!(
            "数据库已标记迁移 v9，但表 {table} 外键不匹配：实际 {actual:?}，期望 {expected:?}"
        ));
    }
    Ok(())
}

fn validate_v9_custom_index_set(
    connection: &Connection,
    table: &str,
    expected: &[&str],
) -> Result<(), String> {
    let escaped_table = table.replace('"', "\"\"");
    let mut actual = connection
        .prepare(&format!("PRAGMA index_list(\"{escaped_table}\")"))
        .map_err(|error| format!("读取 v9 表 {table} 索引集合失败：{error}"))?
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(3)?))
        })
        .map_err(|error| format!("查询 v9 表 {table} 索引集合失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v9 表 {table} 索引集合失败：{error}"))?
        .into_iter()
        .filter_map(|(name, origin)| (origin == "c").then_some(name))
        .collect::<Vec<_>>();
    let mut expected = expected
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(format!(
            "数据库已标记迁移 v9，但表 {table} 自定义索引集合不匹配：实际 {actual:?}，期望 {expected:?}"
        ));
    }
    Ok(())
}

fn validate_v9_triggers(connection: &Connection) -> Result<(), String> {
    let expected = [
        (
            "asset_transfer_no_update",
            "BEFORE UPDATE ON ASSET_TRANSFER",
        ),
        (
            "asset_transfer_no_delete",
            "BEFORE DELETE ON ASSET_TRANSFER",
        ),
        (
            "asset_transfer_no_reinsert",
            "BEFORE INSERT ON ASSET_TRANSFER",
        ),
        (
            "asset_transfer_scope_guard",
            "BEFORE INSERT ON ASSET_TRANSFER",
        ),
    ];
    for (name, marker) in expected {
        let (table, sql) = connection
            .query_row(
                "SELECT tbl_name, sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
                [name],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| format!("读取 v9 触发器 {name} 失败：{error}"))?
            .ok_or_else(|| format!("数据库已标记迁移 v9，但缺少触发器 {name}"))?;
        let normalized = sql.to_ascii_uppercase();
        if table != "asset_transfer"
            || !normalized.contains(marker)
            || !normalized.contains("RAISE(ABORT")
        {
            return Err(format!("数据库已标记迁移 v9，但触发器 {name} 契约不匹配"));
        }
        if name == "asset_transfer_no_reinsert"
            && (!normalized.contains("SENDER_IDENTITY_ID = NEW.SENDER_IDENTITY_ID")
                || !normalized.contains("SOURCE_MESSAGE_ID = NEW.SOURCE_MESSAGE_ID")
                || !normalized.contains("OPERATION_LOG_ID = NEW.OPERATION_LOG_ID"))
        {
            return Err("数据库已标记迁移 v9，但资产转移禁止重插入触发器不完整".to_string());
        }
        if name == "asset_transfer_scope_guard"
            && (!normalized.contains("FROM IDENTITY SENDER")
                || !normalized.contains("JOIN IDENTITY RECIPIENT")
                || !normalized.contains("JOIN OPERATION_LOG AUDIT")
                || !normalized.contains("NEW.ASSET_KIND = 'CURRENCY' AND AUDIT.COMMAND = '转账'")
                || !normalized.contains("NEW.ASSET_KIND = 'ITEM' AND AUDIT.COMMAND = '发送物品'")
                || !normalized.contains("AUDIT.SOURCE_MESSAGE_ID = NEW.SOURCE_MESSAGE_ID"))
        {
            return Err("数据库已标记迁移 v9，但资产转移范围校验触发器不完整".to_string());
        }
    }

    let triggers = connection
        .prepare(
            "SELECT name, tbl_name, sql FROM sqlite_master WHERE type = 'trigger' ORDER BY name",
        )
        .map_err(|error| format!("读取 v9 全库触发器集合失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| format!("查询 v9 全库触发器集合失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v9 全库触发器集合失败：{error}"))?;
    for (name, table, sql) in triggers {
        let declared = expected
            .iter()
            .any(|(expected_name, _)| name == *expected_name && table == "asset_transfer");
        let touches_v9 = matches!(table.as_str(), "asset_transfer" | "item_transfer_policy")
            || ["asset_transfer", "item_transfer_policy"]
                .iter()
                .any(|identifier| sql_mentions_identifier(&sql, identifier));
        if touches_v9 && !declared {
            return Err(format!(
                "数据库已标记迁移 v9，但触发器 {name}（目标表 {table}）未声明却引用了资产转移表"
            ));
        }
    }
    Ok(())
}

fn validate_v9_policy_seeds(connection: &Connection) -> Result<(), String> {
    for (item_key, transferable) in [
        ("revival-grass", 0),
        ("nine-leaf-zhi-grass", 0),
        ("small-healing-potion", 1),
        ("medium-healing-potion", 1),
        ("soul-power-potion", 1),
    ] {
        let matches_contract = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM item_transfer_policy WHERE item_key = ?1 AND transferable = ?2)",
                params![item_key, transferable],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("读取 v9 物品转移策略种子失败：{error}"))?;
        if !matches_contract {
            return Err(format!(
                "数据库已标记迁移 v9，但物品转移策略种子 {item_key} 不匹配"
            ));
        }
    }
    Ok(())
}

fn probe_v9_transfer_guards(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch("SAVEPOINT qimen_v9_transfer_probe;")
        .map_err(|error| format!("开始 v9 资产转移约束探针失败：{error}"))?;
    let probe_result = (|| -> Result<(), String> {
        let token = connection
            .query_row("SELECT lower(hex(randomblob(16)))", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("生成 v9 资产转移探针标识失败：{error}"))?;
        let account_id = format!("v9-probe-{token}");
        let sender_subject = format!("v9-sender-{token}");
        let recipient_subject = format!("v9-recipient-{token}");
        for subject in [&sender_subject, &recipient_subject] {
            connection
                .execute(
                    "INSERT INTO identity(protocol, account_id, namespace, subject_kind, subject_id, created_at) VALUES('onebot11', ?1, 'v9-probe', 'user', ?2, 0)",
                    params![account_id, subject],
                )
                .map_err(|error| format!("v9 资产转移探针无法创建身份：{error}"))?;
        }
        let sender_identity_id = connection
            .query_row(
                "SELECT id FROM identity WHERE protocol = 'onebot11' AND account_id = ?1 AND namespace = 'v9-probe' AND subject_id = ?2",
                params![account_id, sender_subject],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("v9 资产转移探针无法读取发送身份：{error}"))?;
        let recipient_identity_id = connection
            .query_row(
                "SELECT id FROM identity WHERE protocol = 'onebot11' AND account_id = ?1 AND namespace = 'v9-probe' AND subject_id = ?2",
                params![account_id, recipient_subject],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("v9 资产转移探针无法读取接收身份：{error}"))?;
        connection
            .execute(
                "INSERT INTO operation_log(protocol, account_id, namespace, subject_kind, subject_id, command, outcome, source_message_id, details_json, created_at) VALUES('onebot11', ?1, 'v9-probe', 'user', ?2, '转账', 'ok', 'v9-message', '{}', 0)",
                params![account_id, sender_subject],
            )
            .map_err(|error| format!("v9 资产转移探针无法创建审计：{error}"))?;
        let operation_log_id = connection.last_insert_rowid();
        connection
            .execute(
                r#"
                INSERT INTO asset_transfer(
                    protocol, account_id, namespace,
                    sender_identity_id, recipient_identity_id,
                    sender_subject_id, recipient_subject_id,
                    asset_kind, currency_code, item_key, amount,
                    sender_before, sender_after, recipient_before, recipient_after,
                    source_message_id, operation_log_id, created_at
                ) VALUES(
                    'onebot11', ?1, 'v9-probe', ?2, ?3, ?4, ?5,
                    'currency', 'gold_soul_coin', NULL, 10,
                    20, 10, 0, 10, 'v9-message', ?6, 0
                )
                "#,
                params![
                    account_id,
                    sender_identity_id,
                    recipient_identity_id,
                    sender_subject,
                    recipient_subject,
                    operation_log_id
                ],
            )
            .map_err(|error| format!("v9 资产转移探针无法插入合法账本：{error}"))?;
        let transfer_id = connection.last_insert_rowid();
        for (label, sql) in [
            (
                "UPDATE",
                "UPDATE asset_transfer SET amount = 1 WHERE id = ?1",
            ),
            ("DELETE", "DELETE FROM asset_transfer WHERE id = ?1"),
            (
                "REPLACE",
                "INSERT OR REPLACE INTO asset_transfer SELECT * FROM asset_transfer WHERE id = ?1",
            ),
        ] {
            if connection.execute(sql, [transfer_id]).is_ok() {
                return Err(format!("v9 资产转移账本 {label} 保护探针被绕过"));
            }
        }
        if connection
            .execute(
                r#"
                INSERT INTO asset_transfer(
                    protocol, account_id, namespace,
                    sender_identity_id, recipient_identity_id,
                    sender_subject_id, recipient_subject_id,
                    asset_kind, currency_code, item_key, amount,
                    sender_before, sender_after, recipient_before, recipient_after,
                    source_message_id, operation_log_id, created_at
                ) VALUES(
                    'onebot11', ?1, 'v9-probe', ?2, ?3, 'wrong-sender', ?4,
                    'currency', 'gold_soul_coin', NULL, 10,
                    20, 10, 0, 10, 'v9-mismatch', ?5, 0
                )
                "#,
                params![
                    account_id,
                    sender_identity_id,
                    recipient_identity_id,
                    recipient_subject,
                    operation_log_id
                ],
            )
            .is_ok()
        {
            return Err("v9 资产转移身份/审计范围保护探针被绕过".to_string());
        }
        connection
            .execute(
                "INSERT INTO operation_log(protocol, account_id, namespace, subject_kind, subject_id, command, outcome, source_message_id, details_json, created_at) VALUES('onebot11', ?1, 'v9-probe', 'user', ?2, '发送物品', 'ok', 'v9-command-mismatch', '{}', 0)",
                params![account_id, sender_subject],
            )
            .map_err(|error| format!("v9 资产转移探针无法创建错配审计：{error}"))?;
        let mismatched_operation_log_id = connection.last_insert_rowid();
        if connection
            .execute(
                r#"
                INSERT INTO asset_transfer(
                    protocol, account_id, namespace,
                    sender_identity_id, recipient_identity_id,
                    sender_subject_id, recipient_subject_id,
                    asset_kind, currency_code, item_key, amount,
                    sender_before, sender_after, recipient_before, recipient_after,
                    source_message_id, operation_log_id, created_at
                ) VALUES(
                    'onebot11', ?1, 'v9-probe', ?2, ?3, ?4, ?5,
                    'currency', 'gold_soul_coin', NULL, 10,
                    20, 10, 0, 10, 'v9-command-mismatch', ?6, 0
                )
                "#,
                params![
                    account_id,
                    sender_identity_id,
                    recipient_identity_id,
                    sender_subject,
                    recipient_subject,
                    mismatched_operation_log_id
                ],
            )
            .is_ok()
        {
            return Err("v9 资产种类与审计命令绑定探针被绕过".to_string());
        }
        Ok(())
    })();
    let rollback_result = connection
        .execute_batch("ROLLBACK TO qimen_v9_transfer_probe; RELEASE qimen_v9_transfer_probe;")
        .map_err(|error| format!("回滚 v9 资产转移约束探针失败：{error}"));
    match (probe_result, rollback_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(probe_error), Err(rollback_error)) => Err(format!("{probe_error}；{rollback_error}")),
    }
}

fn validate_column_names_and_types(
    table: &str,
    actual: &[TableColumnInfo],
    expected: &[(&str, &str)],
) -> Result<(), String> {
    if actual.len() != expected.len()
        || actual
            .iter()
            .zip(expected)
            .any(|(actual, (name, sql_type))| actual.name != *name || actual.sql_type != *sql_type)
    {
        let expected = expected
            .iter()
            .map(|(name, sql_type)| format!("{name}:{sql_type}"))
            .collect::<Vec<_>>();
        return Err(format!(
            "数据库迁移记录存在，但表 {table} 字段不匹配：实际 {actual:?}，期望 {expected:?}"
        ));
    }
    Ok(())
}

fn validate_v7_table_sql(
    connection: &Connection,
    table: &str,
    markers: &[&str],
) -> Result<(), String> {
    let sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("读取 v7 表 {table} 建表语句失败：{error}"))?
        .ok_or_else(|| format!("数据库已标记迁移 v7，但缺少表 {table}"))?
        .to_ascii_uppercase();
    for marker in markers {
        if !sql.contains(marker) {
            return Err(format!(
                "数据库已标记迁移 v7，但表 {table} 缺少约束：{marker}"
            ));
        }
    }
    Ok(())
}

fn validate_v7_foreign_keys(
    connection: &Connection,
    table: &str,
    expected: &[(&str, &str, &str, &str, &str)],
) -> Result<(), String> {
    let escaped_table = table.replace('"', "\"\"");
    let mut actual = connection
        .prepare(&format!("PRAGMA foreign_key_list(\"{escaped_table}\")"))
        .map_err(|error| format!("读取 v7 表 {table} 外键失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|error| format!("查询 v7 表 {table} 外键失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v7 表 {table} 外键失败：{error}"))?;
    let mut expected = expected
        .iter()
        .map(|(parent, from, to, update, delete)| {
            (
                (*parent).to_string(),
                (*from).to_string(),
                (*to).to_string(),
                (*update).to_string(),
                (*delete).to_string(),
            )
        })
        .collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(format!(
            "数据库已标记迁移 v7，但表 {table} 外键不匹配：实际 {actual:?}，期望 {expected:?}"
        ));
    }
    Ok(())
}

fn validate_v7_index(
    connection: &Connection,
    index_name: &str,
    unique: bool,
    expected_columns: &[&str],
    partial: bool,
) -> Result<(), String> {
    let (table, _sql) = connection
        .query_row(
            "SELECT tbl_name, sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [index_name],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(|error| format!("读取 v7 索引 {index_name} 失败：{error}"))?
        .ok_or_else(|| format!("数据库已标记迁移 v7，但缺少索引 {index_name}"))?;
    let escaped_index = index_name.replace('"', "\"\"");
    let mut info = connection
        .prepare(&format!(
            "PRAGMA index_list(\"{}\")",
            table.replace('"', "\"\"")
        ))
        .map_err(|error| format!("读取 v7 索引 {index_name} 元数据失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .map_err(|error| format!("查询 v7 索引 {index_name} 元数据失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v7 索引 {index_name} 元数据失败：{error}"))?;
    let (_, actual_unique, origin, actual_partial) = info
        .drain(..)
        .find(|(name, _, _, _)| name == index_name)
        .ok_or_else(|| format!("数据库已标记迁移 v7，但索引 {index_name} 不在表索引集合中"))?;
    if actual_unique != unique || origin != "c" || actual_partial != partial {
        return Err(format!(
            "数据库已标记迁移 v7，但索引 {index_name} 的 unique/origin/partial 不匹配"
        ));
    }
    let columns = connection
        .prepare(&format!("PRAGMA index_info(\"{escaped_index}\")"))
        .map_err(|error| format!("读取 v7 索引 {index_name} 字段失败：{error}"))?
        .query_map([], |row| row.get::<_, String>(2))
        .map_err(|error| format!("查询 v7 索引 {index_name} 字段失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v7 索引 {index_name} 字段失败：{error}"))?;
    if columns != expected_columns {
        return Err(format!(
            "数据库已标记迁移 v7，但索引 {index_name} 字段不匹配：实际 {columns:?}，期望 {expected_columns:?}"
        ));
    }
    Ok(())
}

fn validate_v7_custom_index_set(
    connection: &Connection,
    table: &str,
    expected: &[&str],
) -> Result<(), String> {
    let escaped_table = table.replace('"', "\"\"");
    let mut actual = connection
        .prepare(&format!("PRAGMA index_list(\"{escaped_table}\")"))
        .map_err(|error| format!("读取 v7 表 {table} 索引集合失败：{error}"))?
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(3)?))
        })
        .map_err(|error| format!("查询 v7 表 {table} 索引集合失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v7 表 {table} 索引集合失败：{error}"))?
        .into_iter()
        .filter_map(|(name, origin)| (origin == "c").then_some(name))
        .collect::<Vec<_>>();
    let mut expected = expected
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(format!(
            "数据库已标记迁移 v7，但表 {table} 自定义索引集合不匹配：实际 {actual:?}，期望 {expected:?}"
        ));
    }
    Ok(())
}

fn validate_v7_triggers(connection: &Connection) -> Result<(), String> {
    let triggers = connection
        .prepare(
            "SELECT name, tbl_name, sql FROM sqlite_master WHERE type = 'trigger' ORDER BY name",
        )
        .map_err(|error| format!("读取 v7 触发器集合失败：{error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| format!("查询 v7 触发器集合失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 v7 触发器集合失败：{error}"))?;
    for (name, table, sql) in triggers {
        if matches!(table.as_str(), "map" | "map_edge" | "player_map")
            || sql_mentions_identifier(&sql, "map")
            || sql_mentions_identifier(&sql, "map_edge")
            || sql_mentions_identifier(&sql, "player_map")
        {
            return Err(format!(
                "数据库已标记迁移 v7，但触发器 {name}（目标表 {table}）引用了世界数据表"
            ));
        }
    }
    Ok(())
}

fn probe_v7_map_guards(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch("SAVEPOINT qimen_v7_map_probe;")
        .map_err(|error| format!("开始 v7 地图约束探针失败：{error}"))?;
    let probe_result = (|| -> Result<(), String> {
        let valid_map = |key: &str, name: &str, sort_order: i64| {
            connection.execute(
                r#"
                INSERT INTO map(
                    map_key, name, description, level_required, safe, pvp_enabled,
                    teleport_enabled, sort_order, created_at, updated_at
                ) VALUES(?1, ?2, '', 1, 0, 1, 0, ?3, 0, 0)
                "#,
                params![key, name, sort_order],
            )
        };
        if valid_map("v7-probe", "v7-probe", 100_000).is_err() {
            return Err("v7 地图探针无法插入合法地图".to_string());
        }
        if valid_map("Bad-Key", "v7-probe-bad-key", 100_001).is_ok() {
            return Err("v7 地图 key 约束探针被绕过".to_string());
        }
        if valid_map("v7-probe-2", "v7-probe", 100_002).is_ok() {
            return Err("v7 地图名称唯一约束探针被绕过".to_string());
        }
        if connection
            .execute(
                r#"
                INSERT INTO map_edge(
                    from_map_key, to_map_key, travel_kind, direction,
                    level_required, enabled, created_at
                ) VALUES('v7-probe', 'holy-soul-village', 'walk', 'teleport', 1, 1, 0)
                "#,
                [],
            )
            .is_ok()
        {
            return Err("v7 地图方向约束探针被绕过".to_string());
        }
        let foreign_keys_enabled = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, bool>(0))
            .map_err(|error| format!("读取 v7 地图探针外键状态失败：{error}"))?;
        if foreign_keys_enabled
            && connection
                .execute(
                    "INSERT INTO map_edge(from_map_key,to_map_key,travel_kind,direction,level_required,enabled,created_at) VALUES('v7-probe','missing-map','teleport',NULL,1,1,0)",
                    [],
                )
                .is_ok()
        {
            return Err("v7 地图外键约束探针被绕过".to_string());
        }
        Ok(())
    })();
    let rollback_result = connection
        .execute_batch("ROLLBACK TO qimen_v7_map_probe; RELEASE qimen_v7_map_probe;")
        .map_err(|error| format!("回滚 v7 地图约束探针失败：{error}"));
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

fn reject_replayed_operation(
    connection: &Connection,
    key: &IdentityKey<'_>,
    operation: &OperationLogInput<'_>,
) -> Result<(), String> {
    // Host 可能因重试重复投递同一消息；空 message_id 是协议允许的无 ID 场景，
    // 仍由调用方负责避免重放。非空 ID 在同一写事务锁内检查，防止重复扣款/消耗。
    if operation.source_message_id.is_empty() {
        return Ok(());
    }
    let replayed = connection
        .query_row(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM operation_log
                 WHERE protocol = ?1 AND account_id = ?2 AND namespace = ?3
                   AND subject_kind = ?4 AND subject_id = ?5
                   AND command = ?6 AND source_message_id = ?7 AND outcome = 'ok'
            )
            "#,
            params![
                key.protocol.as_str(),
                key.account_id,
                key.namespace,
                key.subject_kind,
                key.subject_id,
                operation.command,
                operation.source_message_id
            ],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| format!("检查经济操作幂等键失败：{error}"))?;
    if replayed {
        return Err("该消息对应的操作已经处理，拒绝重复执行".to_string());
    }
    Ok(())
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

    fn recipient_identity<'a>() -> IdentityKey<'a> {
        IdentityKey {
            protocol: Protocol::OneBot11,
            account_id: "10001",
            namespace: "test",
            subject_kind: "user",
            subject_id: "recipient-openid",
        }
    }

    fn register_awakened_pair(store: &Store) {
        store
            .register_player(&identity(), "转移发送方", "男")
            .expect("应创建资产转移发送方");
        store
            .awaken_wuhun(&identity())
            .expect("发送方应完成武魂觉醒");
        store
            .register_player(&recipient_identity(), "转移接收方", "女")
            .expect("应创建资产转移接收方");
        store
            .awaken_wuhun(&recipient_identity())
            .expect("接收方应完成武魂觉醒");
    }

    fn player_id_for(store: &Store, key: &IdentityKey<'_>) -> i64 {
        store
            .open()
            .expect("应打开测试数据库")
            .query_row(
                r#"
                SELECT p.id FROM identity i
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
                |row| row.get(0),
            )
            .expect("应读取测试角色 ID")
    }

    fn transfer_operation<'a>(command: &'a str, message_id: &'a str) -> OperationLogInput<'a> {
        OperationLogInput {
            command,
            outcome: "ok",
            source_message_id: message_id,
            details_json: r#"{"context":"private","has_args":true}"#,
        }
    }

    fn seed_wallet(store: &Store, key: &IdentityKey<'_>, balance: i64) {
        let player_id = player_id_for(store, key);
        store
            .open()
            .expect("应打开测试数据库")
            .execute(
                r#"
                INSERT INTO wallet(player_id, currency_code, balance, created_at, updated_at)
                VALUES(?1, 'gold_soul_coin', ?2, 0, 0)
                ON CONFLICT(player_id, currency_code) DO UPDATE SET
                    balance = excluded.balance,
                    updated_at = excluded.updated_at
                "#,
                params![player_id, balance],
            )
            .expect("应设置测试钱包余额");
    }

    fn seed_inventory(store: &Store, key: &IdentityKey<'_>, item_key: &str, quantity: i64) {
        let player_id = player_id_for(store, key);
        store
            .open()
            .expect("应打开测试数据库")
            .execute(
                r#"
                INSERT INTO inventory(player_id, item_key, quantity, updated_at)
                VALUES(?1, ?2, ?3, 0)
                ON CONFLICT(player_id, item_key) DO UPDATE SET
                    quantity = excluded.quantity,
                    updated_at = excluded.updated_at
                "#,
                params![player_id, item_key, quantity],
            )
            .expect("应设置测试背包数量");
    }

    fn inventory_for(store: &Store, key: &IdentityKey<'_>, item_key: &str) -> i64 {
        inventory_quantity(
            &store.open().expect("应打开测试数据库"),
            player_id_for(store, key),
            item_key,
        )
        .expect("应读取测试背包数量")
    }

    fn assert_v9_damage_fails_closed(mutation: &str) {
        let directory = tempdir().expect("应创建 v9 损坏测试目录");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("v9 迁移应成功");
        let connection = store.open().expect("应打开 v9 损坏测试数据库");
        connection
            .execute_batch(mutation)
            .expect("应能构造 v9 schema 损坏");
        drop(connection);
        drop(store);
        let error = Store::initialize(directory.path(), &DatabaseConfig::default())
            .expect_err("记录 v9 后损坏 schema 必须拒绝启动");
        assert!(
            error.contains("v9"),
            "v9 损坏未由对应校验拒绝：{mutation}；实际错误：{error}"
        );
    }

    fn checkin_operation(message_id: &str) -> OperationLogInput<'_> {
        OperationLogInput {
            command: "签到",
            outcome: "ok",
            source_message_id: message_id,
            details_json: r#"{"context":"private","has_args":false}"#,
        }
    }

    fn map_operation<'a>(command: &'a str, message_id: &'a str) -> OperationLogInput<'a> {
        OperationLogInput {
            command,
            outcome: "ok",
            source_message_id: message_id,
            details_json: r#"{"context":"private","has_args":true}"#,
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

    fn create_v5_database(directory: &Path, subject_id: &str) -> PathBuf {
        let path = create_v1_database(directory, subject_id);
        let connection = Connection::open(&path).expect("应打开 v1 数据库");
        connection
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .expect("应关闭迁移测试外键");
        connection
            .execute_batch(MIGRATION_V2)
            .expect("应创建 v2 测试结构");
        connection
            .execute_batch(MIGRATION_V3)
            .expect("应创建 v3 测试结构");
        connection
            .execute_batch(MIGRATION_V4)
            .expect("应创建 v4 测试结构");
        connection
            .execute_batch(MIGRATION_V5)
            .expect("应创建 v5 测试结构");
        connection
            .execute_batch(
                "INSERT INTO schema_migration(version, applied_at) VALUES(2, 1), (3, 1), (4, 1), (5, 1)",
            )
            .expect("应记录 v2-v5 测试版本");
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
        let sequence_connection = Connection::open(&path).expect("应打开 v1 数据库");
        sequence_connection
            .execute(
                "UPDATE sqlite_sequence SET seq = 77 WHERE name = 'player'",
                [],
            )
            .expect("应提升旧角色序列水位");
        drop(sequence_connection);
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
        let new_player_id = connection
            .query_row(
                "SELECT p.id FROM identity i JOIN player p ON p.identity_id = i.id WHERE i.account_id = '10001' AND i.subject_id = 'new-user'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("应找到新角色");
        assert!(new_player_id > 77);
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
                    "SELECT COUNT(*) FROM schema_migration WHERE version IN (2, 3, 4, 5, 6)",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("应读取迁移记录"),
            5
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
    fn v6_source_preflight_rejects_extra_player_structures_without_mutating_database() {
        for (label, mutation, marker_query) in [
            (
                "extra column",
                "ALTER TABLE player ADD COLUMN migration_shadow TEXT",
                "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo('player') WHERE name = 'migration_shadow')",
            ),
            (
                "extra index",
                "CREATE INDEX player_migration_extra ON player(name)",
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'player_migration_extra')",
            ),
            (
                "extra trigger",
                "CREATE TRIGGER player_migration_extra AFTER UPDATE OF exp ON player BEGIN SELECT 1; END",
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = 'player_migration_extra')",
            ),
        ] {
            let directory = tempdir().expect("应创建 v6 预检测试目录");
            let path = create_v5_database(directory.path(), &format!("v6-{label}"));
            let connection = Connection::open(&path).expect("应打开 v5 预检数据库");
            connection
                .execute_batch(mutation)
                .unwrap_or_else(|error| panic!("{label} 结构应可注入：{error}"));
            drop(connection);

            let error = Store::initialize(directory.path(), &DatabaseConfig::default())
                .expect_err("v6 源表异常必须拒绝迁移");
            assert!(error.contains("v6"), "{label} 错误应指出 v6：{error}");

            let connection = Connection::open(path).expect("应重新打开未迁移数据库");
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM schema_migration WHERE version = 6",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .expect("应读取 v6 迁移记录"),
                0
            );
            assert!(
                connection
                    .query_row(marker_query, [], |row| row.get::<_, bool>(0))
                    .expect("原异常结构应保留")
            );
            assert!(
                connection
                    .query_row(
                        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'player'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .expect("原 player 表应保留")
                    .contains("level BETWEEN 1 AND 100")
            );
        }
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
                    "SELECT COUNT(*) FROM schema_migration WHERE version IN (2, 3, 4, 5, 6)",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("应读取迁移记录"),
            5
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
    fn wallet_balance_is_zero_before_checkin_and_isolated_by_identity() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "钱包角色", "男")
            .expect("应创建钱包角色");
        let mut other = identity();
        other.subject_id = "wallet-other";
        store
            .register_player(&other, "另一个钱包角色", "女")
            .expect("应创建另一个钱包角色");

        assert_eq!(
            store
                .wallet_balance(&identity(), "gold_soul_coin")
                .expect("未签到余额查询应成功"),
            Some(0)
        );
        store
            .daily_checkin(
                &identity(),
                &DailyCheckinInput {
                    game_day: 400,
                    currency_code: "gold_soul_coin",
                    currency_reward_override: Some(123),
                },
                &checkin_operation("wallet-checkin"),
            )
            .expect("签到应成功");
        assert_eq!(
            store
                .wallet_balance(&identity(), "gold_soul_coin")
                .expect("签到后余额查询应成功"),
            Some(123)
        );
        assert_eq!(
            store
                .wallet_balance(&other, "gold_soul_coin")
                .expect("其他角色余额查询应成功"),
            Some(0)
        );
        assert!(store.wallet_balance(&identity(), "bad code").is_err());
    }

    #[test]
    fn daily_checkin_is_idempotent_and_tracks_real_streaks() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "签到角色", "女")
            .expect("应创建签到角色");

        let first_input = DailyCheckinInput {
            game_day: 100,
            currency_code: "gold_soul_coin",
            currency_reward_override: Some(100),
        };
        let first = store
            .daily_checkin(&identity(), &first_input, &checkin_operation("checkin-1"))
            .expect("首签应成功");
        let DailyCheckinResult::Claimed(first) = first else {
            panic!("首签不应返回重复领取");
        };
        assert_eq!(first.total_claims, 1);
        assert_eq!(first.streak_days, 1);
        assert_eq!(first.cycle_day, 1);
        assert_eq!(first.exp_reward, 60);
        assert_eq!(first.exp_after, 60);
        assert_eq!(first.currency_balance_after, 100);

        let duplicate = store
            .daily_checkin(
                &identity(),
                &DailyCheckinInput {
                    currency_reward_override: Some(199),
                    ..first_input
                },
                &checkin_operation("checkin-duplicate"),
            )
            .expect("同日重复签到应返回原记录");
        assert_eq!(duplicate, DailyCheckinResult::AlreadyClaimed(first.clone()));
        assert_eq!(
            store
                .list_operation_logs(&identity(), None, 100)
                .expect("应读取签到日志")
                .entries
                .iter()
                .filter(|entry| entry.command == "签到")
                .count(),
            1
        );

        let next = store
            .daily_checkin(
                &identity(),
                &DailyCheckinInput {
                    game_day: 101,
                    currency_code: "gold_soul_coin",
                    currency_reward_override: Some(150),
                },
                &checkin_operation("checkin-2"),
            )
            .expect("次日签到应成功");
        let DailyCheckinResult::Claimed(next) = next else {
            panic!("次日不应重复");
        };
        assert_eq!(
            (next.total_claims, next.streak_days, next.cycle_day),
            (2, 2, 2)
        );
        assert_eq!((next.exp_reward, next.exp_after), (70, 130));
        assert_eq!(next.currency_balance_after, 250);

        let after_gap = store
            .daily_checkin(
                &identity(),
                &DailyCheckinInput {
                    game_day: 103,
                    currency_code: "gold_soul_coin",
                    currency_reward_override: Some(199),
                },
                &checkin_operation("checkin-3"),
            )
            .expect("断签后应重新开始连签");
        let DailyCheckinResult::Claimed(after_gap) = after_gap else {
            panic!("断签后的新游戏日不应重复");
        };
        assert_eq!(
            (
                after_gap.total_claims,
                after_gap.streak_days,
                after_gap.cycle_day,
                after_gap.exp_reward
            ),
            (3, 1, 1, 60)
        );
        assert_eq!(after_gap.currency_balance_after, 449);
    }

    #[test]
    fn legacy_level_tiers_use_cumulative_exp_and_cap_at_120() {
        for (level, title) in [
            (1, "魂士"),
            (10, "魂士"),
            (11, "魂师"),
            (20, "魂师"),
            (21, "大魂师"),
            (31, "魂尊"),
            (41, "魂宗"),
            (51, "魂王"),
            (61, "魂帝"),
            (71, "魂圣"),
            (81, "魂斗罗"),
            (91, "封号斗罗"),
            (100, "半神"),
            (101, "一级神祇"),
            (106, "神王"),
            (111, "至高神"),
            (120, "至高神"),
        ] {
            assert_eq!(level_title(level), Ok(title));
            assert_eq!(full_level_title(level), Ok(format!("{level}级{title}")));
        }
        assert_eq!(level_exp_required(1), Ok(Some(100)));
        assert_eq!(level_exp_required(10), Ok(Some(190)));
        assert_eq!(level_exp_required(11), Ok(Some(500)));
        assert_eq!(level_exp_required(120), Ok(None));

        let level_two_total = total_exp_for_level(2).expect("2 级累计经验应可计算");
        assert_eq!(level_two_total, 100);
        assert_eq!(level_for_total_exp(99), Ok(1));
        assert_eq!(level_for_total_exp(level_two_total), Ok(2));
        assert_eq!(level_for_total_exp(i64::MAX), Ok(MAX_PLAYER_LEVEL));
        assert!(level_title(0).is_err());
        assert!(level_for_total_exp(-1).is_err());

        let progress = experience_progress(2, 130).expect("累计经验进度应可计算");
        assert_eq!(progress.level, 2);
        assert_eq!(progress.title, "魂士");
        assert_eq!(progress.exp_in_level, 30);
        assert_eq!(progress.exp_for_next, Some(110));
    }

    #[test]
    fn grant_experience_atomically_advances_level_and_records_operation() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "经验角色", "男")
            .expect("应创建经验测试角色");
        fn experience_operation<'a>(message_id: &'a str) -> OperationLogInput<'a> {
            OperationLogInput {
                command: "获得经验",
                outcome: "ok",
                source_message_id: message_id,
                details_json: r#"{"context":"private","has_args":true}"#,
            }
        }

        let first = store
            .grant_experience(&identity(), 99, &experience_operation("exp-1"))
            .expect("首次经验奖励应成功");
        assert_eq!(
            (first.level_before, first.level_after, first.exp_after),
            (1, 1, 99)
        );
        assert_eq!(first.levels_gained, 0);

        let second = store
            .grant_experience(&identity(), 1, &experience_operation("exp-2"))
            .expect("达到阈值应成功升级");
        assert_eq!(
            (second.level_before, second.level_after, second.exp_after),
            (1, 2, 100)
        );
        assert_eq!(second.levels_gained, 1);
        assert_eq!(second.title_after, "2级魂士");
        let player = store
            .player_status(&identity())
            .expect("应读取经验角色")
            .expect("经验角色应存在");
        assert_eq!((player.level, player.exp), (2, 100));
        assert_eq!(
            store
                .list_operation_logs(&identity(), None, 10)
                .expect("应读取经验操作日志")
                .entries
                .iter()
                .filter(|entry| entry.command == "获得经验")
                .count(),
            2
        );
    }

    #[test]
    fn concurrent_daily_checkin_grants_exactly_once() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "并发签到", "男")
            .expect("应创建并发签到角色");
        let store = Arc::new(store);
        let barrier = Arc::new(Barrier::new(12));
        let handles = (0..12)
            .map(|index| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store.daily_checkin(
                        &identity(),
                        &DailyCheckinInput {
                            game_day: 200,
                            currency_code: "gold_soul_coin",
                            currency_reward_override: Some(123),
                        },
                        &checkin_operation(&format!("concurrent-{index}")),
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .expect("签到线程不应 panic")
                    .expect("签到应成功")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, DailyCheckinResult::Claimed(_)))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, DailyCheckinResult::AlreadyClaimed(_)))
                .count(),
            11
        );
        assert!(results.iter().all(|result| match result {
            DailyCheckinResult::Claimed(receipt) | DailyCheckinResult::AlreadyClaimed(receipt) => {
                receipt.exp_after == 60 && receipt.currency_balance_after == 123
            }
        }));
        assert_eq!(
            store
                .list_operation_logs(&identity(), None, 100)
                .expect("应读取并发签到日志")
                .entries
                .iter()
                .filter(|entry| entry.command == "签到")
                .count(),
            1
        );
    }

    #[test]
    fn daily_checkin_rolls_back_rewards_when_audit_or_arithmetic_fails() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "回滚签到", "男")
            .expect("应创建回滚签到角色");
        let connection = store.open().expect("应打开数据库");
        connection
            .execute_batch(
                r#"
                CREATE TRIGGER operation_log_checkin_abort
                BEFORE INSERT ON operation_log
                WHEN NEW.command = '签到'
                BEGIN SELECT RAISE(ABORT, 'test checkin audit failure'); END;
                "#,
            )
            .expect("应安装签到审计失败触发器");
        drop(connection);
        let input = DailyCheckinInput {
            game_day: 300,
            currency_code: "gold_soul_coin",
            currency_reward_override: Some(188),
        };
        assert!(
            store
                .daily_checkin(&identity(), &input, &checkin_operation("rollback-audit"))
                .is_err()
        );
        assert_eq!(store.player_status(&identity()).unwrap().unwrap().exp, 0);
        let connection = store.open().expect("应重开数据库");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM wallet", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM daily_checkin_claim", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        connection
            .execute("DROP TRIGGER operation_log_checkin_abort", [])
            .expect("应移除审计失败触发器");
        connection
            .execute("UPDATE player SET exp = ?1", [i64::MAX])
            .expect("应设置经验边界");
        drop(connection);
        assert!(
            store
                .daily_checkin(&identity(), &input, &checkin_operation("rollback-overflow"))
                .expect_err("经验溢出必须拒绝")
                .contains("溢出")
        );
        let connection = store.open().expect("应再次打开数据库");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM wallet", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM daily_checkin_claim", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn recorded_v5_with_damaged_schema_fails_closed() {
        let directory = tempdir().expect("应创建测试目录");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("v5 迁移应成功");
        let connection = store.open().expect("应打开数据库");
        connection
            .execute("DROP INDEX wallet_player_currency", [])
            .expect("应破坏钱包唯一索引");
        drop(connection);
        assert!(
            Store::initialize(directory.path(), &DatabaseConfig::default())
                .expect_err("记录 v5 后损坏 schema 必须拒绝")
                .contains("v5")
        );

        let trigger_directory = tempdir().expect("应创建第二测试目录");
        let trigger_store = Store::initialize(trigger_directory.path(), &DatabaseConfig::default())
            .expect("v5 迁移应成功");
        let connection = trigger_store.open().expect("应打开第二数据库");
        connection
            .execute_batch(
                r#"
                CREATE TRIGGER wallet_test_tamper
                AFTER UPDATE ON wallet
                BEGIN UPDATE wallet SET balance = balance + 1 WHERE id = NEW.id; END;
                "#,
            )
            .expect("应安装钱包篡改触发器");
        drop(connection);
        assert!(
            Store::initialize(trigger_directory.path(), &DatabaseConfig::default())
                .expect_err("v5 表额外 trigger 必须拒绝")
                .contains("v5")
        );
        let extra_index_directory = tempdir().expect("应创建额外索引测试目录");
        let extra_index_store =
            Store::initialize(extra_index_directory.path(), &DatabaseConfig::default())
                .expect("v5 迁移应成功");
        let connection = extra_index_store.open().expect("应打开额外索引数据库");
        connection
            .execute(
                "CREATE UNIQUE INDEX wallet_currency_only ON wallet(currency_code)",
                [],
            )
            .expect("应安装额外唯一索引");
        drop(connection);
        assert!(
            Store::initialize(extra_index_directory.path(), &DatabaseConfig::default())
                .expect_err("v5 额外索引必须拒绝")
                .contains("v5")
        );

        let cross_trigger_directory = tempdir().expect("应创建跨表触发器测试目录");
        let cross_trigger_store =
            Store::initialize(cross_trigger_directory.path(), &DatabaseConfig::default())
                .expect("v5 迁移应成功");
        let connection = cross_trigger_store.open().expect("应打开跨表触发器数据库");
        connection
            .execute_batch(
                r#"
                CREATE TRIGGER player_wallet_tamper
                AFTER UPDATE OF exp ON player
                BEGIN
                    UPDATE wallet SET balance = balance WHERE player_id = NEW.id;
                END;
                "#,
            )
            .expect("应安装跨表经济触发器");
        drop(connection);
        assert!(
            Store::initialize(cross_trigger_directory.path(), &DatabaseConfig::default())
                .expect_err("引用 v5 经济表的跨表触发器必须拒绝")
                .contains("v5")
        );

        let hidden_column_directory = tempdir().expect("应创建隐藏列测试目录");
        let hidden_column_store =
            Store::initialize(hidden_column_directory.path(), &DatabaseConfig::default())
                .expect("v5 迁移应成功");
        let connection = hidden_column_store.open().expect("应打开隐藏列测试数据库");
        connection
            .execute(
                "ALTER TABLE wallet ADD COLUMN shadow INTEGER GENERATED ALWAYS AS(balance) VIRTUAL",
                [],
            )
            .expect("应安装 generated 列");
        drop(connection);
        assert!(
            Store::initialize(hidden_column_directory.path(), &DatabaseConfig::default())
                .expect_err("v5 隐藏 generated 列必须拒绝")
                .contains("v5")
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

    #[test]
    fn map_contract_binds_new_players_and_pages_seeded_world_data() {
        let (_directory, store) = test_store();
        let first = store.list_maps_page(1, 5).expect("地图第一页应可查询");
        assert_eq!((first.page, first.page_count, first.total), (1, 2, 7));
        assert_eq!(first.entries.len(), 5);
        assert_eq!(first.entries[0].map_key, "holy-soul-village");
        assert_eq!(first.next_after_key.as_deref(), Some("sunset-forest"));
        let second = store.list_maps_page(2, 5).expect("地图第二页应可查询");
        assert_eq!(second.entries.len(), 2);
        assert!(second.next_after_key.is_none());
        assert!(store.list_maps_page(0, 5).is_err());
        assert!(store.list_maps_page(3, 5).is_err());

        store
            .register_player(&identity(), "地图角色", "男")
            .expect("应创建地图测试角色");
        let current = store
            .current_map(&identity())
            .expect("当前地图查询应成功")
            .expect("新角色应有地图绑定");
        assert_eq!(
            (current.map_key.as_str(), current.name.as_str()),
            ("holy-soul-village", "圣魂村")
        );
        let exits = store.map_exits(&identity()).expect("出口应可查询");
        assert_eq!(
            exits
                .iter()
                .filter(|exit| exit.travel_kind == "walk")
                .count(),
            4
        );
        assert_eq!(
            exits
                .iter()
                .filter(|exit| exit.travel_kind == "teleport")
                .count(),
            4
        );
    }

    #[test]
    fn movement_and_teleport_are_atomic_and_respect_edge_requirements() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "旅行角色", "女")
            .expect("应创建旅行角色");

        let north = store
            .move_direction_with_operation(&identity(), "上", &map_operation("向", "move-1"))
            .expect("圣魂村向上应到达天斗帝国主城");
        assert_eq!(
            (north.from.name.as_str(), north.to.name.as_str()),
            ("圣魂村", "天斗帝国主城")
        );
        assert_eq!(north.direction.as_deref(), Some("north"));
        assert_eq!(
            store.player_status(&identity()).unwrap().unwrap().map_name,
            "天斗帝国主城"
        );

        let back = store
            .teleport_with_operation(
                &identity(),
                Some("圣魂村"),
                &map_operation("传送", "teleport-1"),
            )
            .expect("主城传送阵应可返回圣魂村");
        assert_eq!(
            (back.from.name.as_str(), back.to.name.as_str()),
            ("天斗帝国主城", "圣魂村")
        );
        assert_eq!(back.travel_kind, "teleport");
        assert!(back.direction.is_none());

        store
            .move_direction_with_operation(&identity(), "右", &map_operation("向", "move-2"))
            .expect("圣魂村向右应到达新手村");
        store
            .move_direction_with_operation(&identity(), "下", &map_operation("向", "move-3"))
            .expect("新手村向下应到达星斗外围");
        let locked = store
            .move_direction_with_operation(&identity(), "下", &map_operation("向", "move-4"))
            .expect_err("一级角色不能进入星斗中心");
        assert!(locked.contains("需要20级"));
        assert_eq!(
            store.player_status(&identity()).unwrap().unwrap().map_name,
            "星斗外围"
        );
        assert!(
            store
                .teleport_with_operation(
                    &identity(),
                    Some("圣魂村"),
                    &map_operation("传送", "teleport-no-array"),
                )
                .expect_err("无传送阵区域必须拒绝传送")
                .contains("没有传送阵")
        );
    }

    #[test]
    fn movement_rolls_back_when_atomic_audit_fails() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "回滚旅行", "男")
            .expect("应创建旅行角色");
        let connection = store.open().expect("应打开数据库");
        connection
            .execute_batch(
                r#"
                CREATE TRIGGER operation_log_move_abort
                BEFORE INSERT ON operation_log
                WHEN NEW.command = '向'
                BEGIN SELECT RAISE(ABORT, 'test move audit failure'); END;
                "#,
            )
            .expect("应安装移动审计失败触发器");
        drop(connection);
        assert!(
            store
                .move_direction_with_operation(
                    &identity(),
                    "上",
                    &map_operation("向", "move-rollback"),
                )
                .is_err()
        );
        assert_eq!(
            store.current_map(&identity()).unwrap().unwrap().name,
            "圣魂村"
        );
    }

    #[test]
    fn recorded_v7_with_damaged_world_schema_fails_closed() {
        let directory = tempdir().expect("应创建测试目录");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("v7 迁移应成功");
        let connection = store.open().expect("应打开数据库");
        connection
            .execute("DROP INDEX map_edge_walk_direction", [])
            .expect("应破坏地图方向唯一索引");
        drop(connection);
        assert!(
            Store::initialize(directory.path(), &DatabaseConfig::default())
                .expect_err("记录 v7 后损坏地图 schema 必须拒绝")
                .contains("v7")
        );
    }

    #[test]
    fn v8_seeds_npcs_and_requires_current_conversation_for_shop_access() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "商店角色", "男")
            .expect("应创建商店角色");
        let npcs = store
            .npcs_at_current_map(&identity())
            .expect("圣魂村 NPC 应可查询");
        assert_eq!(npcs.map_name, "圣魂村");
        assert_eq!(
            npcs.entries
                .iter()
                .map(|npc| npc.name.as_str())
                .collect::<Vec<_>>(),
            ["村长", "杂货商人"]
        );
        assert!(store.shop_items_page(&identity(), None, 1, 10).is_err());

        let talk = map_operation("对话", "talk-v8-1");
        let merchant = store
            .talk_to_npc_with_operation(&identity(), "杂货商人", &talk)
            .expect("当前地图应可与杂货商人对话");
        assert_eq!(merchant.npc_key, "holy-soul-village-grocer");
        assert!(merchant.has_shop);
        let shop = store
            .shop_items_page(&identity(), None, 1, 10)
            .expect("对话后裸商店应使用当前 NPC");
        assert_eq!(shop.npc.name, "杂货商人");
        assert_eq!(shop.total, 3);
        assert!(
            shop.entries
                .iter()
                .any(|entry| entry.item.name == "魂力恢复药")
        );
        assert!(
            shop.entries
                .iter()
                .all(|entry| entry.item.category != "revival")
        );

        assert!(
            store
                .talk_to_npc_with_operation(&identity(), "杂货商人", &talk)
                .expect_err("同一消息不得重复写对话绑定")
                .contains("已经处理")
        );
        store
            .move_direction_with_operation(&identity(), "上", &map_operation("向", "leave-shop"))
            .expect("应离开圣魂村");
        assert!(
            store
                .shop_items_page(&identity(), None, 1, 10)
                .expect_err("离开地图后旧 NPC 绑定不得继续交易")
                .contains("先与当前地图")
        );
        store
            .move_direction_with_operation(&identity(), "下", &map_operation("向", "return-shop"))
            .expect("应返回圣魂村");
        assert!(
            store
                .shop_items_page(&identity(), None, 1, 10)
                .expect_err("返回原地图后也必须重新对话")
                .contains("先与当前地图")
        );
    }

    #[test]
    fn v8_allows_additional_catalog_rows_without_relaxing_schema_contract() {
        let (directory, store) = test_store();
        let connection = store.open().expect("应打开经济数据库");
        connection
            .execute(
                r#"INSERT INTO item(
                    item_key, name, category, quality, stackable, max_stack,
                    buy_price, sell_price, level_required, effect_kind, effect_amount,
                    revive_hp_percent, purchasable, sellable, usable, description,
                    created_at, updated_at
                ) VALUES('custom-healing', '自定义恢复药', 'consumable', 3, 1, 20,
                         100, 20, 1, 'restore_hp', 300, 0, 1, 1, 1, '测试商品', 0, 0)"#,
                [],
            )
            .expect("应允许新增物品种子");
        connection
            .execute(
                r#"INSERT INTO npc(
                    npc_key, map_key, name, npc_kind, dialogue, description,
                    enabled, sort_order, created_at, updated_at
                ) VALUES('holy-soul-village-apothecary', 'holy-soul-village', '药师',
                         'merchant', '需要药剂吗？', '新增测试商人', 1, 30, 0, 0)"#,
                [],
            )
            .expect("应允许新增 NPC");
        connection
            .execute(
                "INSERT INTO shop_item(npc_key, item_key, buy_price, stock, enabled, created_at, updated_at) VALUES('holy-soul-village-apothecary', 'custom-healing', 100, -1, 1, 0, 0)",
                [],
            )
            .expect("应允许新增商店商品");
        drop(connection);

        Store::initialize(directory.path(), &DatabaseConfig::default())
            .expect("新增合法目录数据不应破坏 v8 启动校验");
    }

    #[test]
    fn v8_purchase_and_sale_are_atomic_checked_and_replay_safe() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "经济角色", "女")
            .expect("应创建经济角色");
        store
            .daily_checkin(
                &identity(),
                &DailyCheckinInput {
                    game_day: 1,
                    currency_code: "gold_soul_coin",
                    currency_reward_override: Some(199),
                },
                &checkin_operation("economy-fund"),
            )
            .expect("应发放交易资金");
        store
            .talk_to_npc_with_operation(
                &identity(),
                "杂货商人",
                &map_operation("对话", "economy-talk"),
            )
            .expect("应建立商人对话绑定");

        let buy_operation = map_operation("购买", "economy-buy-1");
        let bought = store
            .buy_item_with_operation(&identity(), "小回复药", 5, &buy_operation)
            .expect("应购买五瓶小回复药");
        assert_eq!(
            (
                bought.total_price,
                bought.balance_after,
                bought.inventory_after
            ),
            (50, 149, 5)
        );
        assert!(bought.stock_after.is_none());
        assert!(
            store
                .buy_item_with_operation(&identity(), "小回复药", 5, &buy_operation)
                .expect_err("同一消息不得重复扣款")
                .contains("已经处理")
        );
        assert_eq!(
            store.wallet_balance(&identity(), "gold_soul_coin").unwrap(),
            Some(149)
        );

        let sold = store
            .sell_item_with_operation(
                &identity(),
                "小回复药",
                2,
                &map_operation("出售", "economy-sell-1"),
            )
            .expect("应出售两瓶小回复药");
        assert_eq!(
            (sold.total_price, sold.balance_after, sold.inventory_after),
            (4, 153, 3)
        );
        assert!(
            store
                .buy_item_with_operation(
                    &identity(),
                    "中回复药",
                    1,
                    &map_operation("购买", "economy-level"),
                )
                .expect_err("一级玩家不得购买十级物品")
                .contains("需要10级")
        );
        assert!(
            store
                .buy_item_with_operation(
                    &identity(),
                    "小回复药",
                    97,
                    &map_operation("购买", "economy-stack"),
                )
                .expect_err("购买后不得超过 max_stack")
                .contains("最多堆叠99件")
        );
    }

    #[test]
    fn concurrent_v8_purchase_with_same_message_is_applied_once() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "并发经济", "男")
            .expect("应创建并发经济角色");
        store
            .daily_checkin(
                &identity(),
                &DailyCheckinInput {
                    game_day: 1,
                    currency_code: "gold_soul_coin",
                    currency_reward_override: Some(199),
                },
                &checkin_operation("concurrent-economy-fund"),
            )
            .expect("应发放并发交易资金");
        store
            .talk_to_npc_with_operation(
                &identity(),
                "杂货商人",
                &map_operation("对话", "concurrent-economy-talk"),
            )
            .expect("应建立并发商人绑定");

        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let store = store.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store.buy_item_with_operation(
                        &identity(),
                        "小回复药",
                        1,
                        &map_operation("购买", "concurrent-economy-buy"),
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("购买线程不应 panic"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| result
                    .as_ref()
                    .is_err_and(|error| error.contains("已经处理")))
                .count(),
            1
        );
        assert_eq!(
            store.wallet_balance(&identity(), "gold_soul_coin").unwrap(),
            Some(189)
        );
        let inventory = store.inventory_page(&identity(), 1, 10).unwrap();
        assert_eq!(inventory.entries.len(), 1);
        assert_eq!(inventory.entries[0].quantity, 1);
    }

    #[test]
    fn v8_economy_and_use_roll_back_when_atomic_audit_fails() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "回滚经济", "男")
            .expect("应创建回滚角色");
        store
            .daily_checkin(
                &identity(),
                &DailyCheckinInput {
                    game_day: 1,
                    currency_code: "gold_soul_coin",
                    currency_reward_override: Some(199),
                },
                &checkin_operation("rollback-fund"),
            )
            .expect("应发放回滚测试资金");
        store
            .talk_to_npc_with_operation(
                &identity(),
                "杂货商人",
                &map_operation("对话", "rollback-talk"),
            )
            .expect("应建立回滚商人绑定");
        let connection = store.open().expect("应打开数据库");
        connection
            .execute_batch(
                r#"
                CREATE TRIGGER operation_log_v8_buy_abort
                BEFORE INSERT ON operation_log
                WHEN NEW.command = '购买'
                BEGIN SELECT RAISE(ABORT, 'test v8 buy audit failure'); END;
                "#,
            )
            .expect("应安装购买审计失败触发器");
        drop(connection);
        assert!(
            store
                .buy_item_with_operation(
                    &identity(),
                    "小回复药",
                    1,
                    &map_operation("购买", "rollback-buy"),
                )
                .is_err()
        );
        assert_eq!(
            store.wallet_balance(&identity(), "gold_soul_coin").unwrap(),
            Some(199)
        );
        assert!(
            store
                .inventory_page(&identity(), 1, 10)
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn v8_restore_items_apply_atomically_and_full_state_does_not_consume() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "药剂角色", "女")
            .expect("应创建药剂角色");
        store
            .daily_checkin(
                &identity(),
                &DailyCheckinInput {
                    game_day: 1,
                    currency_code: "gold_soul_coin",
                    currency_reward_override: Some(199),
                },
                &checkin_operation("potion-fund"),
            )
            .expect("应发放药剂资金");
        store
            .talk_to_npc_with_operation(
                &identity(),
                "杂货商人",
                &map_operation("对话", "potion-talk"),
            )
            .expect("应建立药剂商人绑定");
        store
            .buy_item_with_operation(
                &identity(),
                "小回复药",
                2,
                &map_operation("购买", "potion-buy-hp"),
            )
            .expect("应购买生命药剂");
        store
            .buy_item_with_operation(
                &identity(),
                "魂力恢复药",
                1,
                &map_operation("购买", "potion-buy-soul"),
            )
            .expect("应购买魂力药剂");
        let connection = store.open().expect("应打开数据库");
        connection
            .execute(
                "UPDATE player SET hp = 25, soul_power = 10 WHERE identity_id = (SELECT id FROM identity WHERE protocol = 'onebot11' AND account_id = '10001' AND namespace = 'test' AND subject_kind = 'user' AND subject_id = '1875390189')",
                [],
            )
            .expect("应设置受伤状态");
        drop(connection);

        let hp = store
            .use_item_with_operation(
                &identity(),
                "小回复药",
                &map_operation("使用", "potion-use-hp"),
            )
            .expect("应使用生命药剂");
        assert!(hp.consumed);
        assert_eq!((hp.hp_before, hp.hp_after, hp.inventory_after), (25, 75, 1));
        let soul = store
            .use_item_with_operation(
                &identity(),
                "魂力恢复药",
                &map_operation("使用", "potion-use-soul"),
            )
            .expect("应使用魂力药剂");
        assert!(soul.consumed);
        assert_eq!((soul.soul_power_before, soul.soul_power_after), (10, 50));

        let connection = store.open().expect("应再次打开数据库");
        connection
            .execute(
                "UPDATE player SET hp = max_hp WHERE identity_id = (SELECT id FROM identity WHERE protocol = 'onebot11' AND account_id = '10001' AND namespace = 'test' AND subject_kind = 'user' AND subject_id = '1875390189')",
                [],
            )
            .expect("应设置满生命");
        drop(connection);
        let full = store
            .use_item_with_operation(
                &identity(),
                "小回复药",
                &map_operation("使用", "potion-use-full"),
            )
            .expect("满生命应返回未消耗结果");
        assert!(!full.consumed);
        assert_eq!(full.inventory_after, 1);

        let connection = store.open().expect("应打开复活目录测试数据库");
        let player_id = connection
            .query_row(
                "SELECT p.id FROM identity i JOIN player p ON p.identity_id = i.id WHERE i.protocol = 'onebot11' AND i.account_id = '10001' AND i.namespace = 'test' AND i.subject_kind = 'user' AND i.subject_id = '1875390189'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("应读取角色 ID");
        connection
            .execute(
                "INSERT INTO inventory(player_id, item_key, quantity, updated_at) VALUES(?1, 'revival-grass', 1, 0)",
                [player_id],
            )
            .expect("测试可直接放入复活 catalog 物品");
        drop(connection);
        assert!(
            store
                .use_item_with_operation(
                    &identity(),
                    "复活草",
                    &map_operation("使用", "revival-disabled"),
                )
                .expect_err("死亡系统完成前复活物品必须 fail closed")
                .contains("不能直接使用")
        );
    }

    #[test]
    fn recorded_v8_with_damaged_economy_schema_fails_closed() {
        let directory = tempdir().expect("应创建测试目录");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("v8 迁移应成功");
        let connection = store.open().expect("应打开数据库");
        connection
            .execute("DROP TRIGGER shop_item_revival_insert", [])
            .expect("应破坏复活物品上架保护");
        drop(connection);
        assert!(
            Store::initialize(directory.path(), &DatabaseConfig::default())
                .expect_err("记录 v8 后损坏经济 schema 必须拒绝")
                .contains("v8")
        );
    }

    #[test]
    fn recorded_v8_with_mutated_required_seeds_fails_closed() {
        let mutations = [
            "UPDATE item SET sellable = 0 WHERE item_key = 'small-healing-potion'",
            "UPDATE npc SET name = '伪装商人' WHERE npc_key = 'holy-soul-village-grocer'",
            "UPDATE shop_item SET buy_price = 999999 WHERE npc_key = 'holy-soul-village-grocer' AND item_key = 'small-healing-potion'",
            "DELETE FROM shop_item WHERE npc_key = 'holy-soul-village-grocer' AND item_key = 'small-healing-potion'",
        ];
        for mutation in mutations {
            let directory = tempdir().expect("应创建种子损坏测试目录");
            let store = Store::initialize(directory.path(), &DatabaseConfig::default())
                .expect("v8 迁移应成功");
            let connection = store.open().expect("应打开种子损坏测试数据库");
            connection.execute(mutation, []).expect("应能构造种子损坏");
            drop(connection);
            assert!(
                Store::initialize(directory.path(), &DatabaseConfig::default())
                    .expect_err("必需 v8 种子损坏必须拒绝启动")
                    .contains("v8"),
                "未拒绝种子损坏：{mutation}"
            );
        }
    }

    #[test]
    fn recorded_v8_with_cross_table_trigger_reference_fails_closed() {
        let directory = tempdir().expect("应创建跨表触发器测试目录");
        let store =
            Store::initialize(directory.path(), &DatabaseConfig::default()).expect("v8 迁移应成功");
        let connection = store.open().expect("应打开跨表触发器测试数据库");
        connection
            .execute_batch(
                r#"
                CREATE TRIGGER player_wuhun_erases_inventory
                AFTER INSERT ON player_wuhun
                BEGIN
                    DELETE FROM inventory;
                END;
                "#,
            )
            .expect("应能在旧表挂载引用经济表的恶意触发器");
        drop(connection);
        let error = Store::initialize(directory.path(), &DatabaseConfig::default())
            .expect_err("跨表触发器引用 v8 经济表必须拒绝启动");
        assert!(error.contains("v8") && error.contains("触发器"));
    }

    #[test]
    fn v9_wallet_transfer_is_atomic_replay_safe_and_checked() {
        let (_directory, store) = test_store();
        register_awakened_pair(&store);
        seed_wallet(&store, &identity(), 500);

        let first = store
            .transfer_gold_with_operation(
                &identity(),
                recipient_identity().subject_id,
                125,
                &transfer_operation("转账", "transfer-success"),
            )
            .expect("钱包转账应成功");
        assert!(!first.replayed);
        assert_eq!(first.amount, 125);
        assert_eq!(first.sender_balance_after, 375);
        assert_eq!(first.recipient_balance_after, 125);
        assert_eq!(
            store.wallet_balance(&identity(), GOLD_SOUL_COIN).unwrap(),
            Some(375)
        );
        assert_eq!(
            store
                .wallet_balance(&recipient_identity(), GOLD_SOUL_COIN)
                .unwrap(),
            Some(125)
        );

        let replay = store
            .transfer_gold_with_operation(
                &identity(),
                recipient_identity().subject_id,
                125,
                &transfer_operation("转账", "transfer-success"),
            )
            .expect("同一消息应返回原转账回执");
        assert!(replay.replayed);
        assert_eq!(replay.transfer_id, first.transfer_id);
        assert_eq!(replay.sender_balance_after, 375);
        assert_eq!(replay.recipient_balance_after, 125);
        assert!(
            store
                .transfer_gold_with_operation(
                    &identity(),
                    recipient_identity().subject_id,
                    126,
                    &transfer_operation("转账", "transfer-success"),
                )
                .expect_err("同消息不同金额必须拒绝")
                .contains("不同的资产转移请求")
        );

        for (recipient, amount, message_id) in [
            (identity().subject_id, 1, "transfer-self"),
            (recipient_identity().subject_id, 0, "transfer-zero"),
            (
                recipient_identity().subject_id,
                1_000,
                "transfer-insufficient",
            ),
        ] {
            assert!(
                store
                    .transfer_gold_with_operation(
                        &identity(),
                        recipient,
                        amount,
                        &transfer_operation("转账", message_id),
                    )
                    .is_err()
            );
        }
        assert!(
            store
                .transfer_gold_with_operation(
                    &identity(),
                    recipient_identity().subject_id,
                    1,
                    &transfer_operation("转账", ""),
                )
                .expect_err("资产转移不接受空消息 ID")
                .contains("消息 ID")
        );

        seed_wallet(&store, &recipient_identity(), i64::MAX);
        assert!(
            store
                .transfer_gold_with_operation(
                    &identity(),
                    recipient_identity().subject_id,
                    1,
                    &transfer_operation("转账", "transfer-overflow"),
                )
                .expect_err("接收方钱包溢出必须回滚")
                .contains("溢出")
        );
        assert_eq!(
            store.wallet_balance(&identity(), GOLD_SOUL_COIN).unwrap(),
            Some(375)
        );
        assert_eq!(
            store
                .wallet_balance(&recipient_identity(), GOLD_SOUL_COIN)
                .unwrap(),
            Some(i64::MAX)
        );

        let connection = store.open().expect("应检查资产转移账本");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM asset_transfer", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM operation_log WHERE command = '转账'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        let snapshots = connection
            .query_row(
                "SELECT sender_before, sender_after, recipient_before, recipient_after FROM asset_transfer WHERE id = ?1",
                [first.transfer_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?)),
            )
            .unwrap();
        assert_eq!(snapshots, (500, 375, 0, 125));
    }

    #[test]
    fn v9_transfer_targets_are_isolated_by_protocol_account_and_namespace() {
        let (_directory, store) = test_store();
        store
            .register_player(&identity(), "范围发送方", "男")
            .expect("应创建范围发送方");
        store.awaken_wuhun(&identity()).expect("范围发送方应觉醒");
        seed_wallet(&store, &identity(), 100);

        let cross_bot = IdentityKey {
            account_id: "10002",
            subject_id: "cross-bot-user",
            ..identity()
        };
        let cross_namespace = IdentityKey {
            namespace: "other-test",
            subject_id: "cross-namespace-user",
            ..identity()
        };
        let cross_protocol = IdentityKey {
            protocol: Protocol::QqOfficial,
            subject_id: "cross-protocol-user",
            ..identity()
        };
        for (key, name) in [
            (&cross_bot, "跨机器人玩家"),
            (&cross_namespace, "跨命名空间玩家"),
            (&cross_protocol, "跨协议玩家"),
        ] {
            store.register_player(key, name, "女").unwrap();
            store.awaken_wuhun(key).unwrap();
        }

        for (target, message_id) in [
            (cross_bot.subject_id, "transfer-cross-bot"),
            (cross_namespace.subject_id, "transfer-cross-namespace"),
            (cross_protocol.subject_id, "transfer-cross-protocol"),
        ] {
            assert!(
                store
                    .transfer_gold_with_operation(
                        &identity(),
                        target,
                        1,
                        &transfer_operation("转账", message_id),
                    )
                    .expect_err("跨身份范围目标必须不可见")
                    .contains("当前身份范围内不存在")
            );
        }
        assert_eq!(
            store.wallet_balance(&identity(), GOLD_SOUL_COIN).unwrap(),
            Some(100)
        );
        assert_eq!(
            store
                .open()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM asset_transfer", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn concurrent_v9_wallet_transfers_with_distinct_messages_cannot_overdraw() {
        use std::sync::{Arc, Barrier};

        let (_directory, store) = test_store();
        register_awakened_pair(&store);
        seed_wallet(&store, &identity(), 100);
        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|index| {
                let store = store.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let message_id = format!("concurrent-transfer-{index}");
                    let operation = transfer_operation("转账", &message_id);
                    barrier.wait();
                    store.transfer_gold_with_operation(
                        &identity(),
                        recipient_identity().subject_id,
                        30,
                        &operation,
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("并发转账线程不应 panic"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 3);
        assert_eq!(
            results
                .iter()
                .filter(|result| result
                    .as_ref()
                    .is_err_and(|error| error.contains("余额不足")))
                .count(),
            5
        );
        assert_eq!(
            store.wallet_balance(&identity(), GOLD_SOUL_COIN).unwrap(),
            Some(10)
        );
        assert_eq!(
            store
                .wallet_balance(&recipient_identity(), GOLD_SOUL_COIN)
                .unwrap(),
            Some(90)
        );
        let connection = store.open().expect("应检查并发转移结果");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM asset_transfer", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            3
        );
    }

    #[test]
    fn v9_item_gift_is_policy_bound_replay_safe_and_stack_checked() {
        let (_directory, store) = test_store();
        register_awakened_pair(&store);
        seed_inventory(&store, &identity(), "small-healing-potion", 10);
        seed_inventory(&store, &identity(), "revival-grass", 1);

        let first = store
            .gift_item_with_operation(
                &identity(),
                recipient_identity().subject_id,
                "小回复药",
                4,
                &transfer_operation("发送物品", "gift-success"),
            )
            .expect("可转移消耗品应赠送成功");
        assert!(!first.replayed);
        assert_eq!(first.sender_inventory_after, 6);
        assert_eq!(first.recipient_inventory_after, 4);
        assert_eq!(
            inventory_for(&store, &identity(), "small-healing-potion"),
            6
        );
        assert_eq!(
            inventory_for(&store, &recipient_identity(), "small-healing-potion"),
            4
        );

        let replay = store
            .gift_item_with_operation(
                &identity(),
                recipient_identity().subject_id,
                "small-healing-potion",
                4,
                &transfer_operation("发送物品", "gift-success"),
            )
            .expect("同一消息应返回原赠送回执");
        assert!(replay.replayed);
        assert_eq!(replay.transfer_id, first.transfer_id);
        assert!(
            store
                .gift_item_with_operation(
                    &identity(),
                    recipient_identity().subject_id,
                    "小回复药",
                    3,
                    &transfer_operation("发送物品", "gift-success"),
                )
                .expect_err("同消息不同数量必须拒绝")
                .contains("不同的资产转移请求")
        );
        assert!(
            store
                .gift_item_with_operation(
                    &identity(),
                    recipient_identity().subject_id,
                    "复活草",
                    1,
                    &transfer_operation("发送物品", "gift-revival"),
                )
                .expect_err("复活物品不得赠送")
                .contains("不可赠送")
        );

        seed_inventory(&store, &recipient_identity(), "small-healing-potion", 98);
        assert!(
            store
                .gift_item_with_operation(
                    &identity(),
                    recipient_identity().subject_id,
                    "小回复药",
                    2,
                    &transfer_operation("发送物品", "gift-stack-overflow"),
                )
                .expect_err("接收方堆叠上限必须完整回滚")
                .contains("最多堆叠")
        );
        assert_eq!(
            inventory_for(&store, &identity(), "small-healing-potion"),
            6
        );
        assert_eq!(
            inventory_for(&store, &recipient_identity(), "small-healing-potion"),
            98
        );
        let connection = store.open().expect("应检查物品赠送账本");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM asset_transfer", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM operation_log WHERE command = '发送物品'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn v9_transfer_rolls_back_when_audit_or_ledger_insert_fails() {
        let (_audit_directory, audit_store) = test_store();
        register_awakened_pair(&audit_store);
        seed_wallet(&audit_store, &identity(), 100);
        let connection = audit_store.open().expect("应打开审计失败测试数据库");
        connection
            .execute_batch(
                r#"
                CREATE TRIGGER operation_log_transfer_abort
                BEFORE INSERT ON operation_log
                WHEN NEW.command = '转账'
                BEGIN SELECT RAISE(ABORT, 'test transfer audit failure'); END;
                "#,
            )
            .expect("应安装资产转移审计失败触发器");
        drop(connection);
        assert!(
            audit_store
                .transfer_gold_with_operation(
                    &identity(),
                    recipient_identity().subject_id,
                    25,
                    &transfer_operation("转账", "transfer-audit-failure"),
                )
                .is_err()
        );
        assert_eq!(
            audit_store
                .wallet_balance(&identity(), GOLD_SOUL_COIN)
                .unwrap(),
            Some(100)
        );
        assert_eq!(
            audit_store
                .wallet_balance(&recipient_identity(), GOLD_SOUL_COIN)
                .unwrap(),
            Some(0)
        );
        let connection = audit_store.open().expect("应检查审计失败回滚");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM operation_log", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM asset_transfer", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );

        let (_ledger_directory, ledger_store) = test_store();
        register_awakened_pair(&ledger_store);
        seed_inventory(&ledger_store, &identity(), "small-healing-potion", 5);
        let connection = ledger_store.open().expect("应打开账本失败测试数据库");
        connection
            .execute_batch(
                r#"
                CREATE TRIGGER asset_transfer_test_abort
                BEFORE INSERT ON asset_transfer
                BEGIN SELECT RAISE(ABORT, 'test asset ledger failure'); END;
                "#,
            )
            .expect("应安装资产账本失败触发器");
        drop(connection);
        assert!(
            ledger_store
                .gift_item_with_operation(
                    &identity(),
                    recipient_identity().subject_id,
                    "小回复药",
                    2,
                    &transfer_operation("发送物品", "gift-ledger-failure"),
                )
                .is_err()
        );
        assert_eq!(
            inventory_for(&ledger_store, &identity(), "small-healing-potion"),
            5
        );
        assert_eq!(
            inventory_for(&ledger_store, &recipient_identity(), "small-healing-potion"),
            0
        );
        let connection = ledger_store.open().expect("应检查账本失败回滚");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM operation_log", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM asset_transfer", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn recorded_v9_with_damaged_schema_or_seed_fails_closed() {
        for mutation in [
            "DROP TABLE item_transfer_policy;",
            "DROP INDEX asset_transfer_sender_page;",
            "UPDATE item_transfer_policy SET transferable = 0 WHERE item_key = 'small-healing-potion';",
            "DROP TRIGGER asset_transfer_scope_guard;",
        ] {
            assert_v9_damage_fails_closed(mutation);
        }
    }

    #[test]
    fn recorded_v9_with_cross_table_trigger_reference_fails_closed() {
        assert_v9_damage_fails_closed(
            r#"
            CREATE TRIGGER player_wuhun_reads_asset_transfer
            AFTER INSERT ON player_wuhun
            BEGIN
                SELECT EXISTS(SELECT 1 FROM asset_transfer);
            END;
            "#,
        );
    }
}
