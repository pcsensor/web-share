use std::env;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub password_hash: String,
    pub data_dir: PathBuf,
    pub max_file_size: usize,
    pub max_message_len: usize,
    /// Hard cap when loading history within the retention window.
    pub max_history: usize,
    /// How long messages/files are kept (default 3 hours).
    pub retention_secs: u64,
    /// Background purge interval.
    pub purge_interval_secs: u64,
    pub session_ttl_secs: u64,
    pub login_max_attempts: u32,
    pub login_window_secs: u64,
    pub secure_cookie: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let password = env::var("CHAT_PASSWORD").unwrap_or_else(|_| "change-me-now".into());
        let password_hash = if password.starts_with("$2") {
            password
        } else {
            bcrypt::hash(password.as_bytes(), bcrypt::DEFAULT_COST)?
        };

        let data_dir =
            PathBuf::from(env::var("CHAT_DATA_DIR").unwrap_or_else(|_| "./data".into()));

        let max_file_size_mb: usize = env::var("CHAT_MAX_FILE_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        Ok(Self {
            bind: env::var("CHAT_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            password_hash,
            data_dir,
            max_file_size: max_file_size_mb * 1024 * 1024,
            max_message_len: env::var("CHAT_MAX_MSG_LEN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(4000),
            max_history: env::var("CHAT_MAX_HISTORY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2000),
            retention_secs: env::var("CHAT_RETENTION_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3 * 3600),
            purge_interval_secs: env::var("CHAT_PURGE_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60),
            session_ttl_secs: env::var("CHAT_SESSION_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3 * 3600),
            login_max_attempts: env::var("CHAT_LOGIN_MAX_ATTEMPTS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8),
            login_window_secs: env::var("CHAT_LOGIN_WINDOW_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            secure_cookie: env::var("CHAT_SECURE_COOKIE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("chat.db")
    }

    pub fn uploads_dir(&self) -> PathBuf {
        self.data_dir.join("uploads")
    }
}
