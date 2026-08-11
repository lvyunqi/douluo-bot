//! 只读媒体服务的 SQLite 发布 catalog。
//!
//! catalog 只在发布前由命令行写入；运行期只读校验当前 published root，避免媒体服务在
//! 公开读取路径中修改元数据或文件。

use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};

use crate::{
    AssetMeta, MediaIndex, MediaVariant, VariantMeta, is_safe_asset_key, is_safe_storage_key,
    mime_for_extension,
};

const CATALOG_SCHEMA_VERSION: i64 = 2;
const INITIAL_SCHEMA_VERSION: i64 = 1;
const STATUS_PUBLISHED: &str = "published";
const STATUS_DISABLED: &str = "disabled";
const STORAGE_DRIVER_FILESYSTEM: &str = "filesystem";

/// catalog 建立或校验失败时的受限错误类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogError {
    DatabaseUnavailable,
    SchemaInvalid,
    PublishedIndexMismatch,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::DatabaseUnavailable => "媒体 catalog 数据库不可用",
            Self::SchemaInvalid => "媒体 catalog 结构无效",
            Self::PublishedIndexMismatch => "媒体 catalog 与 published root 不一致",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CatalogError {}

#[derive(Clone, Debug)]
struct CatalogObject {
    asset_key: String,
    variant: Option<MediaVariant>,
    sha256: String,
    mime_type: String,
    byte_size: u64,
    storage_key: String,
}

#[derive(Clone, Debug)]
struct CurrentVersion {
    id: i64,
    object: CatalogObject,
    storage_driver: String,
}

#[derive(Clone, Debug)]
struct CurrentVariant {
    version_id: i64,
    object: CatalogObject,
    storage_driver: String,
    width: Option<i64>,
    height: Option<i64>,
}

/// 根据发布根建立或更新 catalog。此函数只能在服务停止时的发布阶段调用。
pub(crate) fn publish_catalog(
    index: &MediaIndex,
    catalog_path: impl AsRef<Path>,
) -> Result<(), CatalogError> {
    for variant in index.variants() {
        if index.get(variant.asset_key()).is_none() {
            return Err(CatalogError::PublishedIndexMismatch);
        }
    }

    let mut connection =
        Connection::open(catalog_path.as_ref()).map_err(|_| CatalogError::DatabaseUnavailable)?;
    enable_foreign_keys(&connection)?;
    migrate(&mut connection)?;

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    let now = unix_timestamp();
    let mut seen_keys = HashSet::new();

    for asset in index.assets() {
        seen_keys.insert(asset.asset_key().to_owned());
        let variants = index.variants_for(asset.asset_key());
        publish_asset(&transaction, asset, &variants, now)?;
    }
    disable_missing_assets(&transaction, &seen_keys, now)?;
    transaction
        .commit()
        .map_err(|_| CatalogError::DatabaseUnavailable)?;

    validate_published_catalog(index, catalog_path).map(|_| ())
}

/// 运行期以只读方式验证本地 catalog 与启动索引完全一致。
pub(crate) fn validate_published_catalog(
    index: &MediaIndex,
    catalog_path: impl AsRef<Path>,
) -> Result<(), CatalogError> {
    let connection = Connection::open_with_flags(catalog_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    enable_foreign_keys(&connection)?;
    validate_schema(&connection)?;

    let versions = read_current_published_versions(&connection)?;
    let variants = read_current_published_variants(&connection)?;
    let published_count = query_i64(
        &connection,
        "SELECT COUNT(*) FROM media_version WHERE status = ?1",
        [STATUS_PUBLISHED],
    )?;
    if usize::try_from(published_count).ok() != Some(versions.len())
        || current_versions_are_invalid(&connection)?
    {
        return Err(CatalogError::PublishedIndexMismatch);
    }

    let mut versions_by_key = HashMap::new();
    for version in versions {
        if versions_by_key
            .insert(version.object.asset_key.clone(), version.clone())
            .is_some()
        {
            return Err(CatalogError::SchemaInvalid);
        }
        if version.storage_driver != STORAGE_DRIVER_FILESYSTEM {
            return Err(CatalogError::SchemaInvalid);
        }
        let Some(asset) = index.get(&version.object.asset_key) else {
            return Err(CatalogError::PublishedIndexMismatch);
        };
        if !matches_asset(&version.object, asset) {
            return Err(CatalogError::PublishedIndexMismatch);
        }
    }
    if versions_by_key.len() != index.len() {
        return Err(CatalogError::PublishedIndexMismatch);
    }

    let mut seen_variants = HashSet::new();
    for variant in variants {
        let key = (variant.object.asset_key.clone(), variant.object.variant);
        if !seen_variants.insert(key.clone()) {
            return Err(CatalogError::SchemaInvalid);
        }
        let Some(parent) = versions_by_key.get(&variant.object.asset_key) else {
            return Err(CatalogError::SchemaInvalid);
        };
        if parent.id != variant.version_id || variant.object.variant.is_none() {
            return Err(CatalogError::SchemaInvalid);
        }
        if !valid_dimensions(variant.width, variant.height) {
            return Err(CatalogError::SchemaInvalid);
        }
        if variant.storage_driver != STORAGE_DRIVER_FILESYSTEM {
            return Err(CatalogError::SchemaInvalid);
        }
        let Some(variant_key) = variant.object.variant else {
            return Err(CatalogError::SchemaInvalid);
        };
        let Some(indexed) = index.get_variant(&variant.object.asset_key, variant_key) else {
            return Err(CatalogError::PublishedIndexMismatch);
        };
        if !matches_variant(&variant.object, variant.width, variant.height, indexed) {
            return Err(CatalogError::PublishedIndexMismatch);
        }
    }
    if seen_variants.len() != index.variants().len() {
        return Err(CatalogError::PublishedIndexMismatch);
    }
    for indexed in index.variants() {
        if index.get(indexed.asset_key()).is_none()
            || !seen_variants.contains(&(indexed.asset_key().to_owned(), Some(indexed.variant())))
        {
            return Err(CatalogError::PublishedIndexMismatch);
        }
    }

    Ok(())
}

fn enable_foreign_keys(connection: &Connection) -> Result<(), CatalogError> {
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|_| CatalogError::DatabaseUnavailable)
}

fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    transaction
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS media_catalog_schema_migration(
                version INTEGER PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );
            ",
        )
        .map_err(|_| CatalogError::SchemaInvalid)?;

    let version = transaction
        .query_row(
            "SELECT MAX(version) FROM media_catalog_schema_migration",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(|_| CatalogError::SchemaInvalid)?;
    match version {
        None => {
            create_v1_schema(&transaction)?;
            transaction
                .execute(
                    "INSERT INTO media_catalog_schema_migration(version, applied_at) VALUES(?1, ?2)",
                    params![INITIAL_SCHEMA_VERSION, unix_timestamp()],
                )
                .map_err(|_| CatalogError::SchemaInvalid)?;
            upgrade_to_v2(&transaction)?;
            transaction
                .execute(
                    "INSERT INTO media_catalog_schema_migration(version, applied_at) VALUES(?1, ?2)",
                    params![CATALOG_SCHEMA_VERSION, unix_timestamp()],
                )
                .map_err(|_| CatalogError::SchemaInvalid)?;
        }
        Some(INITIAL_SCHEMA_VERSION) => {
            upgrade_to_v2(&transaction)?;
            transaction
                .execute(
                    "INSERT INTO media_catalog_schema_migration(version, applied_at) VALUES(?1, ?2)",
                    params![CATALOG_SCHEMA_VERSION, unix_timestamp()],
                )
                .map_err(|_| CatalogError::SchemaInvalid)?;
        }
        Some(CATALOG_SCHEMA_VERSION) => {}
        Some(_) => return Err(CatalogError::SchemaInvalid),
    }
    validate_schema(&transaction)?;
    transaction
        .commit()
        .map_err(|_| CatalogError::DatabaseUnavailable)
}

fn create_v1_schema(transaction: &Transaction<'_>) -> Result<(), CatalogError> {
    transaction
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS media_asset(
                id INTEGER PRIMARY KEY,
                asset_key TEXT NOT NULL UNIQUE,
                current_version_id INTEGER,
                source_url TEXT,
                license TEXT,
                attribution TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                FOREIGN KEY(current_version_id)
                    REFERENCES media_version(id)
                    DEFERRABLE INITIALLY DEFERRED
            );

            CREATE TABLE IF NOT EXISTS media_version(
                id INTEGER PRIMARY KEY,
                asset_id INTEGER NOT NULL,
                version_number INTEGER NOT NULL CHECK(version_number > 0),
                sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
                mime_type TEXT NOT NULL CHECK(mime_type IN (
                    'image/png', 'image/jpeg', 'image/webp', 'image/gif', 'image/bmp'
                )),
                byte_size INTEGER NOT NULL CHECK(byte_size >= 0),
                storage_driver TEXT NOT NULL CHECK(storage_driver = 'filesystem'),
                storage_key TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN ('draft', 'approved', 'published', 'disabled')),
                created_at INTEGER NOT NULL,
                published_at INTEGER,
                FOREIGN KEY(asset_id) REFERENCES media_asset(id) ON DELETE RESTRICT,
                UNIQUE(asset_id, version_number)
            );

            CREATE INDEX IF NOT EXISTS media_version_asset_status_idx
                ON media_version(asset_id, status);

            CREATE TRIGGER IF NOT EXISTS media_version_metadata_immutable
            BEFORE UPDATE OF asset_id, version_number, sha256, mime_type, byte_size,
                storage_driver, storage_key, created_at ON media_version
            BEGIN
                SELECT RAISE(ABORT, 'media_version metadata is immutable');
            END;

            CREATE TRIGGER IF NOT EXISTS media_version_no_delete
            BEFORE DELETE ON media_version
            BEGIN
                SELECT RAISE(ABORT, 'media_version cannot be deleted');
            END;
            ",
        )
        .map_err(|_| CatalogError::SchemaInvalid)
}

/// 将旧 catalog 离线升级到 v2；运行服务只读打开已完成升级的文件。
fn upgrade_to_v2(transaction: &Transaction<'_>) -> Result<(), CatalogError> {
    transaction
        .execute_batch(
            "
            DROP TRIGGER IF EXISTS media_version_metadata_immutable;
            DROP TRIGGER IF EXISTS media_version_no_delete;
            DROP INDEX IF EXISTS media_version_asset_status_idx;

            ALTER TABLE media_asset RENAME TO media_asset_v1;
            ALTER TABLE media_version RENAME TO media_version_v1;

            CREATE TABLE media_asset(
                id INTEGER PRIMARY KEY,
                asset_key TEXT NOT NULL UNIQUE,
                current_version_id INTEGER,
                source_url TEXT,
                license TEXT,
                attribution TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                FOREIGN KEY(current_version_id)
                    REFERENCES media_version(id)
                    DEFERRABLE INITIALLY DEFERRED
            );

            CREATE TABLE media_version(
                id INTEGER PRIMARY KEY,
                asset_id INTEGER NOT NULL,
                version_number INTEGER NOT NULL CHECK(version_number > 0),
                sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
                mime_type TEXT NOT NULL CHECK(mime_type IN (
                    'image/png', 'image/jpeg', 'image/webp', 'image/gif', 'image/bmp'
                )),
                byte_size INTEGER NOT NULL CHECK(byte_size >= 0),
                storage_driver TEXT NOT NULL CHECK(storage_driver = 'filesystem'),
                storage_key TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN ('draft', 'approved', 'published', 'disabled')),
                created_at INTEGER NOT NULL,
                published_at INTEGER,
                FOREIGN KEY(asset_id)
                    REFERENCES media_asset(id) ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED,
                UNIQUE(asset_id, version_number)
            );

            INSERT INTO media_version(
                id, asset_id, version_number, sha256, mime_type, byte_size, storage_driver,
                storage_key, status, created_at, published_at
            )
            SELECT id, asset_id, version_number, sha256, mime_type, byte_size, storage_driver,
                storage_key, status, created_at, published_at
            FROM media_version_v1;

            INSERT INTO media_asset(
                id, asset_key, current_version_id, source_url, license, attribution, created_at, updated_at
            )
            SELECT id, asset_key, current_version_id, source_url, license, attribution, created_at, updated_at
            FROM media_asset_v1;

            DROP TABLE media_version_v1;
            DROP TABLE media_asset_v1;

            CREATE INDEX media_version_asset_status_idx
                ON media_version(asset_id, status);

            CREATE TRIGGER media_version_metadata_immutable
            BEFORE UPDATE OF asset_id, version_number, sha256, mime_type, byte_size,
                storage_driver, storage_key, created_at ON media_version
            BEGIN
                SELECT RAISE(ABORT, 'media_version metadata is immutable');
            END;

            CREATE TRIGGER media_version_no_delete
            BEFORE DELETE ON media_version
            BEGIN
                SELECT RAISE(ABORT, 'media_version cannot be deleted');
            END;

            CREATE TABLE media_variant(
                id INTEGER PRIMARY KEY,
                media_version_id INTEGER NOT NULL,
                variant_key TEXT NOT NULL CHECK(variant_key IN ('chat', 'thumb', 'large')),
                sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
                mime_type TEXT NOT NULL CHECK(mime_type IN (
                    'image/png', 'image/jpeg', 'image/webp', 'image/gif', 'image/bmp'
                )),
                width INTEGER CHECK(width IS NULL OR width > 0),
                height INTEGER CHECK(height IS NULL OR height > 0),
                byte_size INTEGER NOT NULL CHECK(byte_size >= 0),
                storage_driver TEXT NOT NULL CHECK(storage_driver = 'filesystem'),
                storage_key TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(media_version_id) REFERENCES media_version(id) ON DELETE RESTRICT,
                UNIQUE(media_version_id, variant_key),
                CHECK((width IS NULL AND height IS NULL) OR (width IS NOT NULL AND height IS NOT NULL))
            );

            CREATE INDEX media_variant_version_idx ON media_variant(media_version_id);

            CREATE TRIGGER media_variant_metadata_immutable
            BEFORE UPDATE OF media_version_id, variant_key, sha256, mime_type, width, height,
                byte_size, storage_driver, storage_key, created_at ON media_variant
            BEGIN
                SELECT RAISE(ABORT, 'media_variant metadata is immutable');
            END;

            CREATE TRIGGER media_variant_no_delete
            BEFORE DELETE ON media_variant
            BEGIN
                SELECT RAISE(ABORT, 'media_variant cannot be deleted');
            END;
            ",
        )
        .map_err(|_| CatalogError::SchemaInvalid)
}

fn validate_schema(connection: &Connection) -> Result<(), CatalogError> {
    let versions = {
        let mut statement = connection
            .prepare("SELECT version FROM media_catalog_schema_migration ORDER BY version")
            .map_err(|_| CatalogError::SchemaInvalid)?;
        statement
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|_| CatalogError::SchemaInvalid)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| CatalogError::SchemaInvalid)?
    };
    if versions != [INITIAL_SCHEMA_VERSION, CATALOG_SCHEMA_VERSION] {
        return Err(CatalogError::SchemaInvalid);
    }

    for query in [
        "SELECT id, asset_key, current_version_id, source_url, license, attribution, created_at, updated_at FROM media_asset LIMIT 0",
        "SELECT id, asset_id, version_number, sha256, mime_type, byte_size, storage_driver, storage_key, status, created_at, published_at FROM media_version LIMIT 0",
        "SELECT id, media_version_id, variant_key, sha256, mime_type, width, height, byte_size, storage_driver, storage_key, created_at FROM media_variant LIMIT 0",
    ] {
        connection
            .prepare(query)
            .map_err(|_| CatalogError::SchemaInvalid)?;
    }

    let expected_triggers = [
        "media_version_metadata_immutable",
        "media_version_no_delete",
        "media_variant_metadata_immutable",
        "media_variant_no_delete",
    ];
    for trigger in expected_triggers {
        if query_i64(
            connection,
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            [trigger],
        )? != 1
        {
            return Err(CatalogError::SchemaInvalid);
        }
    }
    let unknown_trigger_count = query_i64(
        connection,
        "
        SELECT COUNT(*)
        FROM sqlite_master
        WHERE type = 'trigger'
          AND tbl_name IN ('media_version', 'media_variant')
          AND name NOT IN (
              'media_version_metadata_immutable',
              'media_version_no_delete',
              'media_variant_metadata_immutable',
              'media_variant_no_delete'
          )
        ",
        [],
    )?;
    if unknown_trigger_count != 0 {
        return Err(CatalogError::SchemaInvalid);
    }

    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(|_| CatalogError::SchemaInvalid)?;
    let mut rows = statement
        .query([])
        .map_err(|_| CatalogError::SchemaInvalid)?;
    if rows
        .next()
        .map_err(|_| CatalogError::SchemaInvalid)?
        .is_some()
    {
        return Err(CatalogError::SchemaInvalid);
    }
    Ok(())
}

fn publish_asset(
    transaction: &Transaction<'_>,
    asset: &AssetMeta,
    variants: &[&VariantMeta],
    now: i64,
) -> Result<(), CatalogError> {
    transaction
        .execute(
            "
            INSERT INTO media_asset(asset_key, created_at, updated_at)
            VALUES(?1, ?2, ?2)
            ON CONFLICT(asset_key) DO NOTHING
            ",
            params![asset.asset_key(), now],
        )
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    let (asset_id, current_version_id) = transaction
        .query_row(
            "SELECT id, current_version_id FROM media_asset WHERE asset_key = ?1",
            [asset.asset_key()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .map_err(|_| CatalogError::DatabaseUnavailable)?;

    let version_id = match current_version_id {
        Some(current_version_id) => {
            let (current_sha256, storage_driver) = transaction
                .query_row(
                    "SELECT sha256, storage_driver FROM media_version WHERE id = ?1 AND asset_id = ?2",
                    params![current_version_id, asset_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(|_| CatalogError::SchemaInvalid)?;
            if current_sha256 == asset.sha256()
                && storage_driver == STORAGE_DRIVER_FILESYSTEM
                && current_variant_set_matches(transaction, current_version_id, variants)?
            {
                current_version_id
            } else {
                // 原图或变体集合的任意变化都新建版本，旧记录保持审计语义。
                insert_version(transaction, asset_id, asset, variants, now)?
            }
        }
        None => insert_version(transaction, asset_id, asset, variants, now)?,
    };

    if let Some(previous_id) = current_version_id.filter(|previous_id| *previous_id != version_id) {
        transaction
            .execute(
                "UPDATE media_version SET status = ?1 WHERE id = ?2 AND status = ?3",
                params![STATUS_DISABLED, previous_id, STATUS_PUBLISHED],
            )
            .map_err(|_| CatalogError::DatabaseUnavailable)?;
    }
    transaction
        .execute(
            "
            UPDATE media_version
            SET status = ?1, published_at = COALESCE(published_at, ?2)
            WHERE id = ?3
            ",
            params![STATUS_PUBLISHED, now, version_id],
        )
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    transaction
        .execute(
            "
            UPDATE media_asset
            SET current_version_id = ?1, updated_at = ?2
            WHERE id = ?3
            ",
            params![version_id, now, asset_id],
        )
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    Ok(())
}

fn current_variant_set_matches(
    transaction: &Transaction<'_>,
    version_id: i64,
    variants: &[&VariantMeta],
) -> Result<bool, CatalogError> {
    let stored = {
        let mut statement = transaction
            .prepare(
                "
                SELECT variant_key, sha256, mime_type, width, height, byte_size, storage_driver, storage_key
                FROM media_variant
                WHERE media_version_id = ?1
                ORDER BY variant_key
                ",
            )
            .map_err(|_| CatalogError::DatabaseUnavailable)?;
        statement
            .query_map([version_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(|_| CatalogError::DatabaseUnavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| CatalogError::DatabaseUnavailable)?
    };
    if stored.len() != variants.len() {
        return Ok(false);
    }
    let mut expected = variants.to_vec();
    expected.sort_by_key(|variant| variant.variant());
    for (stored, variant) in stored.iter().zip(expected) {
        let width = variant.width().map(i64::from);
        let height = variant.height().map(i64::from);
        if stored.0 != variant.variant().as_str()
            || stored.1 != variant.sha256()
            || stored.2 != variant.mime()
            || stored.3 != width
            || stored.4 != height
            || i64::try_from(variant.size()).ok() != Some(stored.5)
            || stored.6 != STORAGE_DRIVER_FILESYSTEM
            || stored.7 != variant.storage_key()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn insert_version(
    transaction: &Transaction<'_>,
    asset_id: i64,
    asset: &AssetMeta,
    variants: &[&VariantMeta],
    now: i64,
) -> Result<i64, CatalogError> {
    let version_number = transaction
        .query_row(
            "SELECT COALESCE(MAX(version_number), 0) + 1 FROM media_version WHERE asset_id = ?1",
            [asset_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    let byte_size =
        i64::try_from(asset.size()).map_err(|_| CatalogError::PublishedIndexMismatch)?;
    transaction
        .execute(
            "
            INSERT INTO media_version(
                asset_id, version_number, sha256, mime_type, byte_size, storage_driver,
                storage_key, status, created_at, published_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
            ",
            params![
                asset_id,
                version_number,
                asset.sha256(),
                asset.mime(),
                byte_size,
                STORAGE_DRIVER_FILESYSTEM,
                asset.asset_key(),
                STATUS_PUBLISHED,
                now,
            ],
        )
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    let version_id = transaction.last_insert_rowid();
    for variant in variants {
        insert_variant(transaction, version_id, variant, now)?;
    }
    Ok(version_id)
}

fn insert_variant(
    transaction: &Transaction<'_>,
    version_id: i64,
    variant: &VariantMeta,
    now: i64,
) -> Result<(), CatalogError> {
    let byte_size =
        i64::try_from(variant.size()).map_err(|_| CatalogError::PublishedIndexMismatch)?;
    transaction
        .execute(
            "
            INSERT INTO media_variant(
                media_version_id, variant_key, sha256, mime_type, width, height, byte_size,
                storage_driver, storage_key, created_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ",
            params![
                version_id,
                variant.variant().as_str(),
                variant.sha256(),
                variant.mime(),
                variant.width().map(i64::from),
                variant.height().map(i64::from),
                byte_size,
                STORAGE_DRIVER_FILESYSTEM,
                variant.storage_key(),
                now,
            ],
        )
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    Ok(())
}

fn disable_missing_assets(
    transaction: &Transaction<'_>,
    seen_keys: &HashSet<String>,
    now: i64,
) -> Result<(), CatalogError> {
    let current_assets = {
        let mut statement = transaction
            .prepare(
                "
                SELECT id, asset_key, current_version_id
                FROM media_asset
                WHERE current_version_id IS NOT NULL
                ",
            )
            .map_err(|_| CatalogError::DatabaseUnavailable)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|_| CatalogError::DatabaseUnavailable)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| CatalogError::DatabaseUnavailable)?
    };

    for (asset_id, asset_key, version_id) in current_assets {
        if seen_keys.contains(&asset_key) {
            continue;
        }
        transaction
            .execute(
                "UPDATE media_version SET status = ?1 WHERE id = ?2 AND status = ?3",
                params![STATUS_DISABLED, version_id, STATUS_PUBLISHED],
            )
            .map_err(|_| CatalogError::DatabaseUnavailable)?;
        transaction
            .execute(
                "
                UPDATE media_asset
                SET current_version_id = NULL, updated_at = ?1
                WHERE id = ?2
                ",
                params![now, asset_id],
            )
            .map_err(|_| CatalogError::DatabaseUnavailable)?;
    }
    Ok(())
}

fn read_current_published_versions(
    connection: &Connection,
) -> Result<Vec<CurrentVersion>, CatalogError> {
    let mut statement = connection
        .prepare(
            "
            SELECT a.asset_key, v.id, v.sha256, v.mime_type, v.byte_size, v.storage_driver, v.storage_key
            FROM media_asset AS a
            JOIN media_version AS v
                ON v.id = a.current_version_id AND v.asset_id = a.id
            WHERE v.status = ?1
            ORDER BY a.asset_key
            ",
        )
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    let rows = statement
        .query_map([STATUS_PUBLISHED], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    let mut versions = Vec::new();
    for row in rows {
        let (asset_key, id, sha256, mime_type, byte_size, storage_driver, storage_key) =
            row.map_err(|_| CatalogError::DatabaseUnavailable)?;
        let object = catalog_object(asset_key, None, sha256, mime_type, byte_size, storage_key)?;
        versions.push(CurrentVersion {
            id,
            object,
            storage_driver,
        });
    }
    Ok(versions)
}

fn read_current_published_variants(
    connection: &Connection,
) -> Result<Vec<CurrentVariant>, CatalogError> {
    let mut statement = connection
        .prepare(
            "
            SELECT a.asset_key, v.id, mv.variant_key, mv.sha256, mv.mime_type, mv.width, mv.height,
                   mv.byte_size, mv.storage_driver, mv.storage_key
            FROM media_asset AS a
            JOIN media_version AS v
                ON v.id = a.current_version_id AND v.asset_id = a.id
            JOIN media_variant AS mv ON mv.media_version_id = v.id
            WHERE v.status = ?1
            ORDER BY a.asset_key, mv.variant_key
            ",
        )
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    let rows = statement
        .query_map([STATUS_PUBLISHED], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
            ))
        })
        .map_err(|_| CatalogError::DatabaseUnavailable)?;
    let mut variants = Vec::new();
    for row in rows {
        let (
            asset_key,
            version_id,
            variant_key,
            sha256,
            mime_type,
            width,
            height,
            byte_size,
            storage_driver,
            storage_key,
        ) = row.map_err(|_| CatalogError::DatabaseUnavailable)?;
        let variant = MediaVariant::parse(&variant_key).ok_or(CatalogError::SchemaInvalid)?;
        let object = catalog_object(
            asset_key,
            Some(variant),
            sha256,
            mime_type,
            byte_size,
            storage_key,
        )?;
        variants.push(CurrentVariant {
            version_id,
            object,
            storage_driver,
            width,
            height,
        });
    }
    Ok(variants)
}

fn catalog_object(
    asset_key: String,
    variant: Option<MediaVariant>,
    sha256: String,
    mime_type: String,
    byte_size: i64,
    storage_key: String,
) -> Result<CatalogObject, CatalogError> {
    if !is_safe_asset_key(&asset_key)
        || !is_sha256(&sha256)
        || byte_size < 0
        || !is_safe_storage_key(&storage_key)
    {
        return Err(CatalogError::SchemaInvalid);
    }
    let has_matching_extension = storage_key
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .filter(|extension| mime_for_extension(extension) == Some(mime_type.as_str()))
        .is_some();
    if !has_matching_extension {
        return Err(CatalogError::SchemaInvalid);
    }
    let byte_size = u64::try_from(byte_size).map_err(|_| CatalogError::SchemaInvalid)?;
    Ok(CatalogObject {
        asset_key,
        variant,
        sha256,
        mime_type,
        byte_size,
        storage_key,
    })
}

fn matches_asset(object: &CatalogObject, asset: &AssetMeta) -> bool {
    object.sha256 == asset.sha256()
        && object.mime_type == asset.mime()
        && object.byte_size == asset.size()
        && object.storage_key == asset.asset_key()
}

fn matches_variant(
    object: &CatalogObject,
    width: Option<i64>,
    height: Option<i64>,
    variant: &VariantMeta,
) -> bool {
    object.sha256 == variant.sha256()
        && object.mime_type == variant.mime()
        && object.byte_size == variant.size()
        && object.storage_key == variant.storage_key()
        && width == variant.width().map(i64::from)
        && height == variant.height().map(i64::from)
}

fn valid_dimensions(width: Option<i64>, height: Option<i64>) -> bool {
    match (width, height) {
        (None, None) => true,
        (Some(width), Some(height)) => width > 0 && height > 0,
        _ => false,
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn current_versions_are_invalid(connection: &Connection) -> Result<bool, CatalogError> {
    let invalid_count = query_i64(
        connection,
        "
        SELECT COUNT(*)
        FROM media_asset AS a
        WHERE a.current_version_id IS NOT NULL
          AND NOT EXISTS(
              SELECT 1
              FROM media_version AS v
              WHERE v.id = a.current_version_id
                AND v.asset_id = a.id
                AND v.status = ?1
          )
        ",
        [STATUS_PUBLISHED],
    )?;
    Ok(invalid_count != 0)
}

fn query_i64<P>(connection: &Connection, query: &str, params: P) -> Result<i64, CatalogError>
where
    P: rusqlite::Params,
{
    connection
        .query_row(query, params, |row| row.get::<_, i64>(0))
        .map_err(|_| CatalogError::SchemaInvalid)
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::Connection;

    use super::*;

    fn write_png(path: &Path, content: &[u8]) {
        fs::create_dir_all(path.parent().expect("图片目录")).expect("创建图片目录");
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(content);
        fs::write(path, bytes).expect("写入图片");
    }

    #[test]
    fn publishes_index_and_validates_read_only_catalog() {
        let directory = tempfile::tempdir().expect("临时目录");
        write_png(&directory.path().join("maps/village.png"), b"v1");
        let index = MediaIndex::build(directory.path()).expect("构建索引");
        let catalog_path = directory.path().join("catalog.sqlite");

        publish_catalog(&index, &catalog_path).expect("发布 catalog");
        validate_published_catalog(&index, &catalog_path).expect("校验 catalog");

        let connection = Connection::open(&catalog_path).expect("打开 catalog");
        let (asset_key, status, version_number): (String, String, i64) = connection
            .query_row(
                "
                SELECT a.asset_key, v.status, v.version_number
                FROM media_asset AS a
                JOIN media_version AS v ON v.id = a.current_version_id
                ",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("读取发布版本");
        assert_eq!(asset_key, "maps/village.png");
        assert_eq!(status, STATUS_PUBLISHED);
        assert_eq!(version_number, 1);
    }

    #[test]
    fn catalog_schema_rejects_non_filesystem_storage_driver() {
        let directory = tempfile::tempdir().expect("临时目录");
        let catalog_path = directory.path().join("catalog.sqlite");
        let index = MediaIndex::build(directory.path()).expect("构建空索引");
        publish_catalog(&index, &catalog_path).expect("发布 catalog");

        let connection = Connection::open(&catalog_path).expect("打开 catalog");
        connection
            .execute(
                "INSERT INTO media_asset(asset_key, created_at, updated_at) VALUES(?1, 1, 1)",
                ["maps/invalid.png"],
            )
            .expect("写入资产");
        let result = connection.execute(
            "
            INSERT INTO media_version(
                asset_id, version_number, sha256, mime_type, byte_size, storage_driver,
                storage_key, status, created_at, published_at
            ) VALUES(1, 1, ?1, 'image/png', 1, 'network', 'maps/invalid.png', 'published', 1, 1)
            ",
            ["0".repeat(64)],
        );
        assert!(result.is_err());
    }

    #[test]
    fn changed_file_creates_new_version_and_disables_previous_version() {
        let directory = tempfile::tempdir().expect("临时目录");
        let image = directory.path().join("maps/village.png");
        let catalog_path = directory.path().join("catalog.sqlite");
        write_png(&image, b"v1");
        publish_catalog(
            &MediaIndex::build(directory.path()).expect("构建首个索引"),
            &catalog_path,
        )
        .expect("发布首个版本");

        write_png(&image, b"v2");
        let index = MediaIndex::build(directory.path()).expect("构建新索引");
        publish_catalog(&index, &catalog_path).expect("发布新版本");
        validate_published_catalog(&index, &catalog_path).expect("校验新版本");

        let connection = Connection::open(&catalog_path).expect("打开 catalog");
        let versions: Vec<(i64, String)> = connection
            .prepare("SELECT version_number, status FROM media_version ORDER BY version_number")
            .expect("准备版本查询")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("查询版本")
            .collect::<Result<_, _>>()
            .expect("读取版本");
        assert_eq!(
            versions,
            vec![
                (1, STATUS_DISABLED.to_owned()),
                (2, STATUS_PUBLISHED.to_owned())
            ]
        );
    }

    #[test]
    fn returning_to_a_historical_hash_creates_a_new_version_instead_of_rollback() {
        let directory = tempfile::tempdir().expect("临时目录");
        let image = directory.path().join("maps/village.png");
        let catalog_path = directory.path().join("catalog.sqlite");
        write_png(&image, b"v1");
        publish_catalog(
            &MediaIndex::build(directory.path()).expect("构建首个索引"),
            &catalog_path,
        )
        .expect("发布首个版本");

        write_png(&image, b"v2");
        publish_catalog(
            &MediaIndex::build(directory.path()).expect("构建第二个索引"),
            &catalog_path,
        )
        .expect("发布第二个版本");

        write_png(&image, b"v1");
        let index = MediaIndex::build(directory.path()).expect("构建重复哈希索引");
        publish_catalog(&index, &catalog_path).expect("追加重复哈希版本");
        validate_published_catalog(&index, &catalog_path).expect("校验当前发布集");

        let connection = Connection::open(&catalog_path).expect("打开 catalog");
        let versions: Vec<(i64, String)> = connection
            .prepare("SELECT version_number, status FROM media_version ORDER BY version_number")
            .expect("准备版本查询")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("查询版本")
            .collect::<Result<_, _>>()
            .expect("读取版本");
        assert_eq!(
            versions,
            vec![
                (1, STATUS_DISABLED.to_owned()),
                (2, STATUS_DISABLED.to_owned()),
                (3, STATUS_PUBLISHED.to_owned()),
            ]
        );
    }

    #[test]
    fn changed_variant_creates_new_parent_version_without_reactivating_history() {
        let directory = tempfile::tempdir().expect("临时目录");
        let image = directory.path().join("maps/village.png");
        let variant = directory.path().join("__variants/chat/maps/village.png");
        let catalog_path = directory.path().join("catalog.sqlite");
        write_png(&image, b"original");
        write_png(&variant, b"chat-v1");
        publish_catalog(
            &MediaIndex::build(directory.path()).expect("构建首个索引"),
            &catalog_path,
        )
        .expect("发布首个版本");

        write_png(&variant, b"chat-v2");
        let index = MediaIndex::build(directory.path()).expect("构建变体索引");
        publish_catalog(&index, &catalog_path).expect("发布变体新版本");
        validate_published_catalog(&index, &catalog_path).expect("校验变体发布集");

        let connection = Connection::open(&catalog_path).expect("打开 catalog");
        let versions: Vec<(i64, String)> = connection
            .prepare("SELECT version_number, status FROM media_version ORDER BY version_number")
            .expect("准备版本查询")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("查询版本")
            .collect::<Result<_, _>>()
            .expect("读取版本");
        let variant_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM media_variant", [], |row| row.get(0))
            .expect("读取变体数");
        assert_eq!(
            versions,
            vec![
                (1, STATUS_DISABLED.to_owned()),
                (2, STATUS_PUBLISHED.to_owned())
            ]
        );
        assert_eq!(variant_count, 2);
    }

    #[test]
    fn rejects_catalog_when_published_file_changes_without_release() {
        let directory = tempfile::tempdir().expect("临时目录");
        let image = directory.path().join("maps/village.png");
        let catalog_path = directory.path().join("catalog.sqlite");
        write_png(&image, b"v1");
        publish_catalog(
            &MediaIndex::build(directory.path()).expect("构建首个索引"),
            &catalog_path,
        )
        .expect("发布 catalog");

        write_png(&image, b"changed-without-catalog");
        let changed_index = MediaIndex::build(directory.path()).expect("构建变更索引");
        assert_eq!(
            validate_published_catalog(&changed_index, &catalog_path),
            Err(CatalogError::PublishedIndexMismatch)
        );
    }

    #[test]
    fn missing_file_disables_current_publication_without_deleting_history() {
        let directory = tempfile::tempdir().expect("临时目录");
        let image = directory.path().join("maps/village.png");
        let catalog_path = directory.path().join("catalog.sqlite");
        write_png(&image, b"v1");
        publish_catalog(
            &MediaIndex::build(directory.path()).expect("构建首个索引"),
            &catalog_path,
        )
        .expect("发布 catalog");

        fs::remove_file(&image).expect("移除发布前文件");
        let empty_index = MediaIndex::build(directory.path()).expect("构建空索引");
        publish_catalog(&empty_index, &catalog_path).expect("更新空发布集");
        validate_published_catalog(&empty_index, &catalog_path).expect("校验空发布集");

        let connection = Connection::open(&catalog_path).expect("打开 catalog");
        let status: String = connection
            .query_row("SELECT status FROM media_version", [], |row| row.get(0))
            .expect("读取历史版本");
        let current_version_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM media_asset WHERE current_version_id IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .expect("读取当前指针");
        assert_eq!(status, STATUS_DISABLED);
        assert_eq!(current_version_count, 0);
    }
}
