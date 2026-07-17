use crate::models::{FileMeta, Message, MessageKind};
use chrono::{DateTime, Duration, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::{Row, Sqlite, Transaction};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
    retention_secs: i64,
    max_history: i64,
    uploads_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct StoredFile {
    pub meta: FileMeta,
    pub path: PathBuf,
    pub uploader: Uuid,
    pub created_at: DateTime<Utc>,
}

impl Db {
    pub async fn open(
        db_path: &Path,
        uploads_dir: PathBuf,
        retention_secs: u64,
        max_history: usize,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(&uploads_dir)?;

        let options = SqliteConnectOptions::from_str(&format!(
            "sqlite://{}?mode=rwc",
            db_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("db path is not valid UTF-8"))?
                .replace('\\', "/")
        ))?
        .create_if_missing(true)
        .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        let db = Self {
            pool,
            retention_secs: retention_secs as i64,
            max_history: max_history as i64,
            uploads_dir,
        };
        db.migrate().await?;
        Ok(db)
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        // SQLite: run statements separately (multi-statement execute is not always supported).
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS files (
                id          TEXT PRIMARY KEY NOT NULL,
                name        TEXT NOT NULL,
                size        INTEGER NOT NULL,
                mime        TEXT NOT NULL,
                path        TEXT NOT NULL,
                uploader_id TEXT NOT NULL,
                created_at  TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(r#"CREATE INDEX IF NOT EXISTS idx_files_created ON files(created_at)"#)
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
                id          TEXT PRIMARY KEY NOT NULL,
                user_id     TEXT NOT NULL,
                nickname    TEXT NOT NULL,
                kind        TEXT NOT NULL,
                content     TEXT NOT NULL DEFAULT '',
                file_id     TEXT,
                created_at  TEXT NOT NULL,
                FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE SET NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(r#"CREATE INDEX IF NOT EXISTS idx_messages_created ON messages(created_at)"#)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    pub fn cutoff(&self) -> DateTime<Utc> {
        Utc::now() - Duration::seconds(self.retention_secs)
    }

    pub fn cutoff_rfc3339(&self) -> String {
        self.cutoff().to_rfc3339()
    }

    pub async fn list_messages(&self) -> anyhow::Result<Vec<Message>> {
        let cutoff = self.cutoff_rfc3339();
        let rows = sqlx::query(
            r#"
            SELECT
                m.id, m.user_id, m.nickname, m.kind, m.content, m.created_at,
                f.id AS f_id, f.name AS f_name, f.size AS f_size, f.mime AS f_mime
            FROM messages m
            LEFT JOIN files f ON m.file_id = f.id
            WHERE m.created_at >= ?
            ORDER BY m.created_at ASC
            LIMIT ?
            "#,
        )
        .bind(&cutoff)
        .bind(self.max_history)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row_to_message(&row)?);
        }
        Ok(out)
    }

    pub async fn insert_message(&self, message: &Message) -> anyhow::Result<()> {
        let file_id = message.file.as_ref().map(|f| f.id.to_string());
        sqlx::query(
            r#"
            INSERT INTO messages (id, user_id, nickname, kind, content, file_id, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(message.id.to_string())
        .bind(message.user_id.to_string())
        .bind(&message.nickname)
        .bind(kind_to_str(&message.kind))
        .bind(&message.content)
        .bind(file_id)
        .bind(message.ts.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert file row + message row in one transaction (file bytes already on disk).
    pub async fn insert_file_and_message(
        &self,
        stored: &StoredFile,
        message: &Message,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        insert_file_tx(&mut tx, stored).await?;
        insert_message_tx(&mut tx, message).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_file(&self, id: &Uuid) -> anyhow::Result<Option<StoredFile>> {
        let cutoff = self.cutoff_rfc3339();
        let row = sqlx::query(
            r#"
            SELECT id, name, size, mime, path, uploader_id, created_at
            FROM files
            WHERE id = ? AND created_at >= ?
            "#,
        )
        .bind(id.to_string())
        .bind(&cutoff)
        .fetch_optional(&self.pool)
        .await?;

        Ok(match row {
            Some(r) => Some(row_to_stored_file(&r)?),
            None => None,
        })
    }

    /// Delete expired rows and return absolute paths of files that should be unlinked.
    pub async fn purge_expired(&self) -> anyhow::Result<PurgeStats> {
        let cutoff = self.cutoff_rfc3339();

        let expired_files = sqlx::query(
            r#"SELECT id, path FROM files WHERE created_at < ?"#,
        )
        .bind(&cutoff)
        .fetch_all(&self.pool)
        .await?;

        let mut paths: Vec<PathBuf> = Vec::new();
        for row in &expired_files {
            let path: String = row.try_get("path")?;
            paths.push(resolve_path(&self.uploads_dir, &path));
        }

        let mut tx = self.pool.begin().await?;

        let msg_result = sqlx::query(r#"DELETE FROM messages WHERE created_at < ?"#)
            .bind(&cutoff)
            .execute(&mut *tx)
            .await?;

        // Messages referencing expired files: also drop messages whose file is expired
        // (file_id set but file already to be deleted — CASCADE not automatic on file delete)
        for row in &expired_files {
            let fid: String = row.try_get("id")?;
            sqlx::query(r#"DELETE FROM messages WHERE file_id = ?"#)
                .bind(&fid)
                .execute(&mut *tx)
                .await?;
        }

        let file_result = sqlx::query(r#"DELETE FROM files WHERE created_at < ?"#)
            .bind(&cutoff)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        let mut files_removed = 0u64;
        for path in &paths {
            if path.exists() {
                match std::fs::remove_file(path) {
                    Ok(()) => files_removed += 1,
                    Err(e) => tracing::warn!(?path, %e, "failed to remove expired file"),
                }
            }
        }

        // Orphan .bin files on disk (no DB row or already deleted)
        let orphans = self.cleanup_orphan_files().await?;

        Ok(PurgeStats {
            messages_deleted: msg_result.rows_affected(),
            files_deleted: file_result.rows_affected(),
            files_removed_from_disk: files_removed + orphans,
        })
    }

    async fn cleanup_orphan_files(&self) -> anyhow::Result<u64> {
        let mut removed = 0u64;
        let read_dir = match std::fs::read_dir(&self.uploads_dir) {
            Ok(d) => d,
            Err(_) => return Ok(0),
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("bin") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            let Ok(id) = Uuid::parse_str(stem) else {
                // Unknown naming — remove if older than retention by mtime
                if file_mtime_expired(&path, self.retention_secs) {
                    if std::fs::remove_file(&path).is_ok() {
                        removed += 1;
                    }
                }
                continue;
            };

            let exists = sqlx::query(r#"SELECT 1 AS ok FROM files WHERE id = ? LIMIT 1"#)
                .bind(id.to_string())
                .fetch_optional(&self.pool)
                .await?
                .is_some();

            if !exists {
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                    tracing::debug!(%id, "removed orphan upload");
                }
            }
        }
        Ok(removed)
    }
}

#[derive(Debug, Default)]
pub struct PurgeStats {
    pub messages_deleted: u64,
    pub files_deleted: u64,
    pub files_removed_from_disk: u64,
}

impl PurgeStats {
    pub fn is_empty(&self) -> bool {
        self.messages_deleted == 0
            && self.files_deleted == 0
            && self.files_removed_from_disk == 0
    }
}

async fn insert_file_tx(
    tx: &mut Transaction<'_, Sqlite>,
    stored: &StoredFile,
) -> anyhow::Result<()> {
    // Store path relative to uploads dir when possible
    let path_str = stored
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_string())
        .unwrap_or_else(|| stored.path.to_string_lossy().into_owned());

    sqlx::query(
        r#"
        INSERT INTO files (id, name, size, mime, path, uploader_id, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(stored.meta.id.to_string())
    .bind(&stored.meta.name)
    .bind(stored.meta.size as i64)
    .bind(&stored.meta.mime)
    .bind(&path_str)
    .bind(stored.uploader.to_string())
    .bind(stored.created_at.to_rfc3339())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_message_tx(
    tx: &mut Transaction<'_, Sqlite>,
    message: &Message,
) -> anyhow::Result<()> {
    let file_id = message.file.as_ref().map(|f| f.id.to_string());
    sqlx::query(
        r#"
        INSERT INTO messages (id, user_id, nickname, kind, content, file_id, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(message.id.to_string())
    .bind(message.user_id.to_string())
    .bind(&message.nickname)
    .bind(kind_to_str(&message.kind))
    .bind(&message.content)
    .bind(file_id)
    .bind(message.ts.to_rfc3339())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn resolve_path(uploads_dir: &Path, stored: &str) -> PathBuf {
    let p = Path::new(stored);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        uploads_dir.join(p)
    }
}

fn file_mtime_expired(path: &Path, retention_secs: i64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(elapsed) = modified.elapsed() else {
        return false;
    };
    elapsed.as_secs() as i64 > retention_secs
}

fn kind_to_str(k: &MessageKind) -> &'static str {
    match k {
        MessageKind::Text => "text",
        MessageKind::Image => "image",
        MessageKind::Video => "video",
        MessageKind::File => "file",
        MessageKind::System => "system",
    }
}

fn kind_from_str(s: &str) -> anyhow::Result<MessageKind> {
    Ok(match s {
        "text" => MessageKind::Text,
        "image" => MessageKind::Image,
        "video" => MessageKind::Video,
        "file" => MessageKind::File,
        "system" => MessageKind::System,
        other => anyhow::bail!("unknown message kind: {other}"),
    })
}

fn parse_ts(s: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc))
}

fn row_to_message(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Message> {
    let id: String = row.try_get("id")?;
    let user_id: String = row.try_get("user_id")?;
    let nickname: String = row.try_get("nickname")?;
    let kind: String = row.try_get("kind")?;
    let content: String = row.try_get("content")?;
    let created_at: String = row.try_get("created_at")?;

    let file = match row.try_get::<Option<String>, _>("f_id")? {
        Some(fid) => {
            let name: String = row.try_get("f_name")?;
            let size: i64 = row.try_get("f_size")?;
            let mime: String = row.try_get("f_mime")?;
            Some(FileMeta {
                id: Uuid::parse_str(&fid)?,
                name,
                size: size as u64,
                mime,
            })
        }
        None => None,
    };

    Ok(Message {
        id: Uuid::parse_str(&id)?,
        user_id: Uuid::parse_str(&user_id)?,
        nickname,
        kind: kind_from_str(&kind)?,
        content,
        file,
        ts: parse_ts(&created_at)?,
    })
}

fn row_to_stored_file(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<StoredFile> {
    let id: String = row.try_get("id")?;
    let name: String = row.try_get("name")?;
    let size: i64 = row.try_get("size")?;
    let mime: String = row.try_get("mime")?;
    let path: String = row.try_get("path")?;
    let uploader_id: String = row.try_get("uploader_id")?;
    let created_at: String = row.try_get("created_at")?;

    // path resolution needs uploads_dir — caller uses Db::get_file which has it
    // We store only filename; full path reconstructed in get_file
    Ok(StoredFile {
        meta: FileMeta {
            id: Uuid::parse_str(&id)?,
            name,
            size: size as u64,
            mime,
        },
        path: PathBuf::from(path),
        uploader: Uuid::parse_str(&uploader_id)?,
        created_at: parse_ts(&created_at)?,
    })
}

impl Db {
    pub fn absolute_file_path(&self, stored: &StoredFile) -> PathBuf {
        resolve_path(&self.uploads_dir, &stored.path.to_string_lossy())
    }
}
