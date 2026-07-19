use crate::models::{
    AdminUserView, AuditView, DeviceView, FileMeta, InviteCodeView, Message, MessageKind, UserRole,
    UserRow, UserStatus,
};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
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

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS users (
                id              TEXT PRIMARY KEY NOT NULL,
                username        TEXT NOT NULL UNIQUE,
                username_norm   TEXT NOT NULL UNIQUE,
                password_hash   TEXT NOT NULL,
                display_name    TEXT NOT NULL,
                role            TEXT NOT NULL,
                status          TEXT NOT NULL,
                totp_secret_enc TEXT,
                totp_enabled    INTEGER NOT NULL DEFAULT 0,
                last_totp_step  INTEGER,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                approved_at     TEXT,
                approved_by     TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS idx_users_status ON users(status)"#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS recovery_codes (
                id         TEXT PRIMARY KEY NOT NULL,
                user_id    TEXT NOT NULL,
                code_hash  TEXT NOT NULL,
                used_at    TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS idx_recovery_user ON recovery_codes(user_id)"#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS trusted_devices (
                id          TEXT PRIMARY KEY NOT NULL,
                user_id     TEXT NOT NULL,
                token_hash  TEXT NOT NULL UNIQUE,
                label       TEXT,
                created_at  TEXT NOT NULL,
                last_seen   TEXT NOT NULL,
                expires_at  TEXT NOT NULL,
                revoked_at  TEXT,
                FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS idx_devices_user ON trusted_devices(user_id)"#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS admin_audit (
                id          TEXT PRIMARY KEY NOT NULL,
                admin_id    TEXT NOT NULL,
                action      TEXT NOT NULL,
                target_id   TEXT,
                meta_json   TEXT,
                created_at  TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS invite_codes (
                id          TEXT PRIMARY KEY NOT NULL,
                code        TEXT NOT NULL UNIQUE,
                created_by  TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                expires_at  TEXT NOT NULL,
                used_at     TEXT,
                used_by     TEXT,
                revoked_at  TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Migrate older DBs that predate expires_at (default: created_at + 24h, or now if missing).
        let cols = sqlx::query(r#"PRAGMA table_info(invite_codes)"#)
            .fetch_all(&self.pool)
            .await?;
        let has_expires = cols.iter().any(|row| {
            row.try_get::<String, _>("name")
                .map(|n| n == "expires_at")
                .unwrap_or(false)
        });
        if !has_expires {
            sqlx::query(r#"ALTER TABLE invite_codes ADD COLUMN expires_at TEXT"#)
                .execute(&self.pool)
                .await?;
            // Backfill: treat legacy codes as already expired so they cannot be reused indefinitely.
            let now = Utc::now().to_rfc3339();
            sqlx::query(
                r#"UPDATE invite_codes SET expires_at = ? WHERE expires_at IS NULL OR expires_at = ''"#,
            )
            .bind(&now)
            .execute(&self.pool)
            .await?;
        }

        sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS idx_invite_codes_code ON invite_codes(code)"#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // ---------- Users ----------

    pub async fn count_admins(&self) -> anyhow::Result<i64> {
        let row = sqlx::query(
            r#"SELECT COUNT(*) AS c FROM users WHERE role = 'admin'"#,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get::<i64, _>("c")?)
    }

    pub async fn insert_user(&self, user: &UserRow) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        insert_user_tx(&mut tx, user).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Insert user and optionally consume a one-time invite in one transaction.
    /// Returns `Ok(true)` if invite was used, `Ok(false)` if open registration.
    /// Returns `Err` with message `"invalid_invite"` if code missing/used/revoked.
    pub async fn insert_user_with_invite(
        &self,
        user: &UserRow,
        invite_code: Option<&str>,
    ) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;

        let used_invite = if let Some(code) = invite_code {
            let now = Utc::now().to_rfc3339();
            let row = sqlx::query(
                r#"
                SELECT id FROM invite_codes
                WHERE code = ?
                  AND used_at IS NULL
                  AND revoked_at IS NULL
                  AND expires_at > ?
                LIMIT 1
                "#,
            )
            .bind(code)
            .bind(&now)
            .fetch_optional(&mut *tx)
            .await?;

            let Some(row) = row else {
                anyhow::bail!("invalid_invite");
            };
            let invite_id: String = row.try_get("id")?;
            let result = sqlx::query(
                r#"
                UPDATE invite_codes
                SET used_at = ?, used_by = ?
                WHERE id = ?
                  AND used_at IS NULL
                  AND revoked_at IS NULL
                  AND expires_at > ?
                "#,
            )
            .bind(&now)
            .bind(user.id.to_string())
            .bind(&invite_id)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() == 0 {
                anyhow::bail!("invalid_invite");
            }
            true
        } else {
            false
        };

        insert_user_tx(&mut tx, user).await?;
        tx.commit().await?;
        Ok(used_invite)
    }

    pub async fn get_user_by_id(&self, id: &Uuid) -> anyhow::Result<Option<UserRow>> {
        let row = sqlx::query(r#"SELECT * FROM users WHERE id = ?"#)
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(r) => Some(row_to_user(&r)?),
            None => None,
        })
    }

    pub async fn get_user_by_username_norm(
        &self,
        username_norm: &str,
    ) -> anyhow::Result<Option<UserRow>> {
        let row = sqlx::query(r#"SELECT * FROM users WHERE username_norm = ?"#)
            .bind(username_norm)
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(r) => Some(row_to_user(&r)?),
            None => None,
        })
    }

    pub async fn username_exists(&self, username_norm: &str) -> anyhow::Result<bool> {
        let row = sqlx::query(r#"SELECT 1 AS ok FROM users WHERE username_norm = ? LIMIT 1"#)
            .bind(username_norm)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn update_user_status(
        &self,
        id: &Uuid,
        status: UserStatus,
        approved_by: Option<Uuid>,
    ) -> anyhow::Result<()> {
        let now = Utc::now();
        let approved_at = if matches!(
            status,
            UserStatus::ApprovedUnbound | UserStatus::Active
        ) {
            Some(now.to_rfc3339())
        } else {
            None
        };
        sqlx::query(
            r#"
            UPDATE users SET status = ?, updated_at = ?,
                approved_at = COALESCE(?, approved_at),
                approved_by = COALESCE(?, approved_by)
            WHERE id = ?
            "#,
        )
        .bind(status.as_str())
        .bind(now.to_rfc3339())
        .bind(approved_at)
        .bind(approved_by.map(|u| u.to_string()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_user_status_simple(
        &self,
        id: &Uuid,
        status: UserStatus,
    ) -> anyhow::Result<()> {
        let now = Utc::now();
        sqlx::query(r#"UPDATE users SET status = ?, updated_at = ? WHERE id = ?"#)
            .bind(status.as_str())
            .bind(now.to_rfc3339())
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn enable_totp(
        &self,
        id: &Uuid,
        totp_secret_enc: &str,
    ) -> anyhow::Result<()> {
        let now = Utc::now();
        sqlx::query(
            r#"
            UPDATE users SET
                totp_secret_enc = ?,
                totp_enabled = 1,
                status = 'active',
                last_totp_step = NULL,
                updated_at = ?
            WHERE id = ?
            "#,
        )
        .bind(totp_secret_enc)
        .bind(now.to_rfc3339())
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn reset_totp(&self, id: &Uuid) -> anyhow::Result<()> {
        let now = Utc::now();
        sqlx::query(
            r#"
            UPDATE users SET
                totp_secret_enc = NULL,
                totp_enabled = 0,
                last_totp_step = NULL,
                status = 'approved_unbound',
                updated_at = ?
            WHERE id = ?
            "#,
        )
        .bind(now.to_rfc3339())
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        // Invalidate recovery codes
        sqlx::query(r#"DELETE FROM recovery_codes WHERE user_id = ?"#)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_last_totp_step(&self, id: &Uuid, step: i64) -> anyhow::Result<()> {
        sqlx::query(r#"UPDATE users SET last_totp_step = ?, updated_at = ? WHERE id = ?"#)
            .bind(step)
            .bind(Utc::now().to_rfc3339())
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_password(&self, id: &Uuid, password_hash: &str) -> anyhow::Result<()> {
        sqlx::query(r#"UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?"#)
            .bind(password_hash)
            .bind(Utc::now().to_rfc3339())
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_users(
        &self,
        status_filter: Option<&str>,
    ) -> anyhow::Result<Vec<AdminUserView>> {
        let rows = if let Some(status) = status_filter {
            sqlx::query(r#"SELECT * FROM users WHERE status = ? ORDER BY created_at DESC"#)
                .bind(status)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query(r#"SELECT * FROM users ORDER BY created_at DESC"#)
                .fetch_all(&self.pool)
                .await?
        };

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let u = row_to_user(&row)?;
            out.push(AdminUserView {
                id: u.id,
                username: u.username,
                display_name: u.display_name,
                role: u.role,
                status: u.status,
                totp_enabled: u.totp_enabled,
                created_at: u.created_at,
                approved_at: u.approved_at,
            });
        }
        Ok(out)
    }

    // ---------- Recovery codes ----------

    pub async fn replace_recovery_codes(
        &self,
        user_id: &Uuid,
        code_hashes: &[String],
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(r#"DELETE FROM recovery_codes WHERE user_id = ?"#)
            .bind(user_id.to_string())
            .execute(&mut *tx)
            .await?;
        let now = Utc::now().to_rfc3339();
        for hash in code_hashes {
            sqlx::query(
                r#"
                INSERT INTO recovery_codes (id, user_id, code_hash, used_at, created_at)
                VALUES (?, ?, ?, NULL, ?)
                "#,
            )
            .bind(Uuid::new_v4().to_string())
            .bind(user_id.to_string())
            .bind(hash)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Consume a recovery code if it matches an unused hash. Returns true if consumed.
    pub async fn consume_recovery_code(
        &self,
        user_id: &Uuid,
        code_hash: &str,
    ) -> anyhow::Result<bool> {
        let row = sqlx::query(
            r#"
            SELECT id FROM recovery_codes
            WHERE user_id = ? AND code_hash = ? AND used_at IS NULL
            LIMIT 1
            "#,
        )
        .bind(user_id.to_string())
        .bind(code_hash)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(false);
        };
        let id: String = row.try_get("id")?;
        let result = sqlx::query(
            r#"UPDATE recovery_codes SET used_at = ? WHERE id = ? AND used_at IS NULL"#,
        )
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    // ---------- Trusted devices ----------

    pub async fn find_trusted_device(
        &self,
        user_id: &Uuid,
        token_hash: &str,
    ) -> anyhow::Result<Option<(Uuid, DateTime<Utc>)>> {
        let now = Utc::now().to_rfc3339();
        let row = sqlx::query(
            r#"
            SELECT id, expires_at FROM trusted_devices
            WHERE user_id = ? AND token_hash = ? AND revoked_at IS NULL AND expires_at > ?
            LIMIT 1
            "#,
        )
        .bind(user_id.to_string())
        .bind(token_hash)
        .bind(&now)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => {
                let id = Uuid::parse_str(&r.try_get::<String, _>("id")?)?;
                let exp = parse_ts(&r.try_get::<String, _>("expires_at")?)?;
                Ok(Some((id, exp)))
            }
            None => Ok(None),
        }
    }

    pub async fn touch_device(&self, device_id: &Uuid) -> anyhow::Result<()> {
        sqlx::query(r#"UPDATE trusted_devices SET last_seen = ? WHERE id = ?"#)
            .bind(Utc::now().to_rfc3339())
            .bind(device_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn insert_trusted_device(
        &self,
        id: Uuid,
        user_id: &Uuid,
        token_hash: &str,
        label: Option<&str>,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let now = Utc::now();
        sqlx::query(
            r#"
            INSERT INTO trusted_devices
                (id, user_id, token_hash, label, created_at, last_seen, expires_at, revoked_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, NULL)
            "#,
        )
        .bind(id.to_string())
        .bind(user_id.to_string())
        .bind(token_hash)
        .bind(label)
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_devices(
        &self,
        user_id: &Uuid,
        current_token_hash: Option<&str>,
    ) -> anyhow::Result<Vec<DeviceView>> {
        let now = Utc::now().to_rfc3339();
        let rows = sqlx::query(
            r#"
            SELECT id, token_hash, label, created_at, last_seen, expires_at
            FROM trusted_devices
            WHERE user_id = ? AND revoked_at IS NULL AND expires_at > ?
            ORDER BY last_seen DESC
            "#,
        )
        .bind(user_id.to_string())
        .bind(&now)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let token_hash: String = row.try_get("token_hash")?;
            out.push(DeviceView {
                id: Uuid::parse_str(&row.try_get::<String, _>("id")?)?,
                label: row.try_get("label")?,
                created_at: parse_ts(&row.try_get::<String, _>("created_at")?)?,
                last_seen: parse_ts(&row.try_get::<String, _>("last_seen")?)?,
                expires_at: parse_ts(&row.try_get::<String, _>("expires_at")?)?,
                current: current_token_hash
                    .map(|h| h == token_hash)
                    .unwrap_or(false),
            });
        }
        Ok(out)
    }

    pub async fn revoke_device(&self, user_id: &Uuid, device_id: &Uuid) -> anyhow::Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE trusted_devices SET revoked_at = ?
            WHERE id = ? AND user_id = ? AND revoked_at IS NULL
            "#,
        )
        .bind(Utc::now().to_rfc3339())
        .bind(device_id.to_string())
        .bind(user_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn revoke_all_devices(&self, user_id: &Uuid) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            UPDATE trusted_devices SET revoked_at = ?
            WHERE user_id = ? AND revoked_at IS NULL
            "#,
        )
        .bind(Utc::now().to_rfc3339())
        .bind(user_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---------- Invite codes ----------

    pub async fn insert_invite_code(
        &self,
        id: Uuid,
        code: &str,
        created_by: &Uuid,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let now = Utc::now();
        sqlx::query(
            r#"
            INSERT INTO invite_codes
                (id, code, created_by, created_at, expires_at, used_at, used_by, revoked_at)
            VALUES (?, ?, ?, ?, ?, NULL, NULL, NULL)
            "#,
        )
        .bind(id.to_string())
        .bind(code)
        .bind(created_by.to_string())
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_invite_codes(&self, limit: i64) -> anyhow::Result<Vec<InviteCodeView>> {
        let rows = sqlx::query(
            r#"SELECT * FROM invite_codes ORDER BY created_at DESC LIMIT ?"#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row_to_invite(&row)?);
        }
        Ok(out)
    }

    pub async fn revoke_invite_code(
        &self,
        id: &Uuid,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE invite_codes SET revoked_at = ?
            WHERE id = ? AND used_at IS NULL AND revoked_at IS NULL
            "#,
        )
        .bind(Utc::now().to_rfc3339())
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    // ---------- Audit ----------

    pub async fn insert_audit(
        &self,
        admin_id: &Uuid,
        action: &str,
        target_id: Option<Uuid>,
        meta_json: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO admin_audit (id, admin_id, action, target_id, meta_json, created_at)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(admin_id.to_string())
        .bind(action)
        .bind(target_id.map(|id| id.to_string()))
        .bind(meta_json)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_audit(&self, limit: i64) -> anyhow::Result<Vec<AuditView>> {
        let rows = sqlx::query(
            r#"SELECT * FROM admin_audit ORDER BY created_at DESC LIMIT ?"#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(AuditView {
                id: Uuid::parse_str(&row.try_get::<String, _>("id")?)?,
                admin_id: Uuid::parse_str(&row.try_get::<String, _>("admin_id")?)?,
                action: row.try_get("action")?,
                target_id: row
                    .try_get::<Option<String>, _>("target_id")?
                    .map(|s| Uuid::parse_str(&s))
                    .transpose()?,
                meta_json: row.try_get("meta_json")?,
                created_at: parse_ts(&row.try_get::<String, _>("created_at")?)?,
            });
        }
        Ok(out)
    }

    // ---------- Messages / files (unchanged logic) ----------

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

    pub async fn purge_expired(&self) -> anyhow::Result<PurgeStats> {
        let cutoff = self.cutoff_rfc3339();

        let expired_files = sqlx::query(r#"SELECT id, path FROM files WHERE created_at < ?"#)
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

    pub fn absolute_file_path(&self, stored: &StoredFile) -> PathBuf {
        resolve_path(&self.uploads_dir, &stored.path.to_string_lossy())
    }
}

pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex::encode(digest)
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

fn row_to_invite(row: &SqliteRow) -> anyhow::Result<InviteCodeView> {
    let used_at = row
        .try_get::<Option<String>, _>("used_at")?
        .map(|s| parse_ts(&s))
        .transpose()?;
    let revoked_at = row
        .try_get::<Option<String>, _>("revoked_at")?
        .map(|s| parse_ts(&s))
        .transpose()?;
    let created_at = parse_ts(&row.try_get::<String, _>("created_at")?)?;
    // Older rows may have NULL expires_at before migration backfill — treat as expired.
    let expires_at = match row.try_get::<Option<String>, _>("expires_at")? {
        Some(s) if !s.is_empty() => parse_ts(&s)?,
        _ => created_at,
    };
    let now = Utc::now();
    let status = if revoked_at.is_some() {
        "revoked"
    } else if used_at.is_some() {
        "used"
    } else if expires_at <= now {
        "expired"
    } else {
        "unused"
    }
    .to_string();

    Ok(InviteCodeView {
        id: Uuid::parse_str(&row.try_get::<String, _>("id")?)?,
        code: row.try_get("code")?,
        created_by: Uuid::parse_str(&row.try_get::<String, _>("created_by")?)?,
        created_at,
        expires_at,
        used_at,
        used_by: row
            .try_get::<Option<String>, _>("used_by")?
            .map(|s| Uuid::parse_str(&s))
            .transpose()?,
        revoked_at,
        status,
    })
}

fn row_to_user(row: &SqliteRow) -> anyhow::Result<UserRow> {
    Ok(UserRow {
        id: Uuid::parse_str(&row.try_get::<String, _>("id")?)?,
        username: row.try_get("username")?,
        username_norm: row.try_get("username_norm")?,
        password_hash: row.try_get("password_hash")?,
        display_name: row.try_get("display_name")?,
        role: UserRole::parse(&row.try_get::<String, _>("role")?)
            .ok_or_else(|| anyhow::anyhow!("invalid role"))?,
        status: UserStatus::parse(&row.try_get::<String, _>("status")?)
            .ok_or_else(|| anyhow::anyhow!("invalid status"))?,
        totp_secret_enc: row.try_get("totp_secret_enc")?,
        totp_enabled: row.try_get::<i64, _>("totp_enabled")? != 0,
        last_totp_step: row.try_get("last_totp_step")?,
        created_at: parse_ts(&row.try_get::<String, _>("created_at")?)?,
        updated_at: parse_ts(&row.try_get::<String, _>("updated_at")?)?,
        approved_at: row
            .try_get::<Option<String>, _>("approved_at")?
            .map(|s| parse_ts(&s))
            .transpose()?,
        approved_by: row
            .try_get::<Option<String>, _>("approved_by")?
            .map(|s| Uuid::parse_str(&s))
            .transpose()?,
    })
}

async fn insert_user_tx(
    tx: &mut Transaction<'_, Sqlite>,
    user: &UserRow,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO users (
            id, username, username_norm, password_hash, display_name,
            role, status, totp_secret_enc, totp_enabled, last_totp_step,
            created_at, updated_at, approved_at, approved_by
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(user.id.to_string())
    .bind(&user.username)
    .bind(&user.username_norm)
    .bind(&user.password_hash)
    .bind(&user.display_name)
    .bind(user.role.as_str())
    .bind(user.status.as_str())
    .bind(&user.totp_secret_enc)
    .bind(if user.totp_enabled { 1i64 } else { 0 })
    .bind(user.last_totp_step)
    .bind(user.created_at.to_rfc3339())
    .bind(user.updated_at.to_rfc3339())
    .bind(user.approved_at.map(|t| t.to_rfc3339()))
    .bind(user.approved_by.map(|id| id.to_string()))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_file_tx(
    tx: &mut Transaction<'_, Sqlite>,
    stored: &StoredFile,
) -> anyhow::Result<()> {
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
