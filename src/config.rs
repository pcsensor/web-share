use crate::crypto_seal;
use std::env;
use std::path::PathBuf;

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(default)
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
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
    pub pending_2fa_ttl_secs: u64,
    pub login_max_attempts: u32,
    pub login_window_secs: u64,
    pub secure_cookie: bool,
    /// AES-256 key for sealing TOTP secrets at rest.
    pub secret_key: [u8; 32],
    pub bootstrap_admin_user: Option<String>,
    pub bootstrap_admin_password: Option<String>,
    /// If true, on startup set bootstrap admin password from env (emergency recovery).
    pub reset_admin_password: bool,
    /// If true, on startup clear bootstrap admin TOTP so they re-bind (emergency recovery).
    pub reset_admin_totp: bool,
    pub device_trust_days: u64,
    pub registration_open: bool,
    pub totp_issuer: String,
    /// Invite code validity in hours (default 24).
    pub invite_ttl_hours: u64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let data_dir =
            PathBuf::from(env::var("CHAT_DATA_DIR").unwrap_or_else(|_| "./data".into()));

        let max_file_size_mb: usize = env::var("CHAT_MAX_FILE_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        let secret_key_raw = env::var("CHAT_SECRET_KEY").unwrap_or_else(|_| {
            tracing::warn!(
                "CHAT_SECRET_KEY not set — generating ephemeral key (TOTP secrets will be invalid after restart)"
            );
            crypto_seal::generate_secret_key_hex()
        });
        let secret_key = crypto_seal::parse_secret_key(&secret_key_raw)?;

        let bootstrap_admin_user = env::var("CHAT_BOOTSTRAP_ADMIN_USER")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let bootstrap_admin_password = env::var("CHAT_BOOTSTRAP_ADMIN_PASSWORD")
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        Ok(Self {
            bind: env::var("CHAT_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
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
                .unwrap_or(24 * 3600),
            pending_2fa_ttl_secs: env::var("CHAT_PENDING_2FA_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            login_max_attempts: env::var("CHAT_LOGIN_MAX_ATTEMPTS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8),
            login_window_secs: env::var("CHAT_LOGIN_WINDOW_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            secure_cookie: env_bool("CHAT_SECURE_COOKIE", false),
            secret_key,
            bootstrap_admin_user,
            bootstrap_admin_password,
            reset_admin_password: env_bool("CHAT_RESET_ADMIN_PASSWORD", false),
            reset_admin_totp: env_bool("CHAT_RESET_ADMIN_TOTP", false),
            device_trust_days: env_u64("CHAT_DEVICE_TRUST_DAYS", 60).max(1),
            registration_open: env_bool("CHAT_REGISTRATION_OPEN", true),
            totp_issuer: {
                let raw = env::var("CHAT_TOTP_ISSUER").unwrap_or_default();
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    "Chat Transfer".into()
                } else {
                    trimmed.to_string()
                }
            },
            invite_ttl_hours: env_u64("CHAT_INVITE_TTL_HOURS", 24).max(1),
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("chat.db")
    }

    pub fn uploads_dir(&self) -> PathBuf {
        self.data_dir.join("uploads")
    }
}
