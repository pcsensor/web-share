mod admin;
mod auth;
mod chat;
mod config;
mod crypto_seal;
mod db;
mod files;
mod models;
mod totp;

use auth::AuthState;
use axum::{
    extract::DefaultBodyLimit,
    http::{header, HeaderValue, Method},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, post},
    Router,
};
use chat::ChatHub;
use config::Config;
use db::Db;
use files::FileStore;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Clone)]
pub struct AppState {
    pub auth: AuthState,
    pub chat: ChatHub,
    pub files: Arc<FileStore>,
    pub db: Db,
    pub config: Config,
}

fn load_dotenv() {
    // Prefer first existing .env among: cwd, exe dir, crate dir.
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(".env"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(".env"));
            // target/debug/.env → also try project root two levels up
            if let Some(parent) = dir.parent().and_then(|p| p.parent()) {
                candidates.push(parent.join(".env"));
            }
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env"));

    let mut loaded = false;
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        match dotenvy::from_path(&path) {
            Ok(()) => {
                eprintln!("loaded env file: {}", path.display());
                loaded = true;
                break;
            }
            Err(e) => eprintln!("warning: could not load {}: {e}", path.display()),
        }
    }
    if !loaded {
        // Fall back to dotenvy default search (cwd + parents)
        match dotenvy::dotenv() {
            Ok(path) => eprintln!("loaded env file: {}", path.display()),
            Err(dotenvy::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("warning: could not load .env: {e}"),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "chat_transfer=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env()?;
    tracing::info!(
        device_trust_days = config.device_trust_days,
        invite_ttl_hours = config.invite_ttl_hours,
        session_ttl_secs = config.session_ttl_secs,
        registration_open = config.registration_open,
        "config loaded"
    );

    std::fs::create_dir_all(&config.data_dir)?;
    let uploads = config.uploads_dir();
    std::fs::create_dir_all(&uploads)?;

    let db = Db::open(
        &config.db_path(),
        uploads.clone(),
        config.retention_secs,
        config.max_history,
    )
    .await?;

    auth::bootstrap_admin(&db, &config).await?;

    match db.purge_expired().await {
        Ok(stats) if !stats.is_empty() => {
            tracing::info!(
                messages = stats.messages_deleted,
                files = stats.files_deleted,
                disk = stats.files_removed_from_disk,
                "startup purge completed"
            );
        }
        Ok(_) => tracing::info!("startup purge: nothing expired"),
        Err(e) => tracing::error!(%e, "startup purge failed"),
    }

    let max_file = config.max_file_size;
    let state = AppState {
        auth: AuthState::new(config.clone()),
        chat: ChatHub::new(db.clone(), config.max_message_len),
        files: Arc::new(FileStore::new(uploads, max_file, db.clone())?),
        db: db.clone(),
        config: config.clone(),
    };

    {
        let auth = state.auth.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(600));
            loop {
                interval.tick().await;
                auth.purge_expired();
            }
        });
    }

    {
        let db = state.db.clone();
        let interval_secs = config.purge_interval_secs.max(10);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            loop {
                interval.tick().await;
                match db.purge_expired().await {
                    Ok(stats) if !stats.is_empty() => {
                        tracing::info!(
                            messages = stats.messages_deleted,
                            files = stats.files_deleted,
                            disk = stats.files_removed_from_disk,
                            "retention purge"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::error!(%e, "retention purge failed"),
                }
            }
        });
    }

    let static_dir = resolve_static_dir();
    tracing::info!(?static_dir, "serving static assets");
    tracing::info!(
        retention_secs = config.retention_secs,
        purge_interval_secs = config.purge_interval_secs,
        db = %config.db_path().display(),
        "persistence enabled"
    );

    let api = Router::new()
        // Auth
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/2fa/verify", post(auth::verify_2fa))
        .route("/api/auth/2fa/recover", post(auth::recover_2fa))
        .route("/api/auth/totp/setup/start", post(auth::totp_setup_start))
        .route(
            "/api/auth/totp/setup/confirm",
            post(auth::totp_setup_confirm),
        )
        .route("/api/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        .route("/api/config", get(auth::public_config))
        // Security
        .route("/api/security/devices", get(auth::list_devices))
        .route(
            "/api/security/devices/{id}",
            delete(auth::revoke_device),
        )
        .route("/api/security/password", post(auth::change_password))
        // Admin
        .route("/api/admin/users", get(admin::list_users))
        .route(
            "/api/admin/users/{id}/approve",
            post(admin::approve_user),
        )
        .route(
            "/api/admin/users/{id}/reject",
            post(admin::reject_user),
        )
        .route(
            "/api/admin/users/{id}/disable",
            post(admin::disable_user),
        )
        .route(
            "/api/admin/users/{id}/enable",
            post(admin::enable_user),
        )
        .route(
            "/api/admin/users/{id}/reset-totp",
            post(admin::reset_totp),
        )
        .route("/api/admin/audit", get(admin::list_audit))
        .route("/api/admin/invites", get(admin::list_invites).post(admin::create_invite))
        .route(
            "/api/admin/invites/{id}",
            delete(admin::revoke_invite),
        )
        // Chat
        .route("/api/messages", get(chat::get_messages))
        .route("/api/messages/text", post(chat::post_text))
        .route("/api/upload", post(files::upload))
        .route("/api/files/{id}", get(files::preview))
        .route("/api/files/{id}/download", get(files::download))
        .route("/ws", get(chat::ws_handler));

    let app = Router::new()
        .merge(api)
        .route_service("/", ServeFile::new(static_dir.join("index.html")))
        .route_service(
            "/admin",
            ServeFile::new(static_dir.join("admin.html")),
        )
        .nest_service("/assets", ServeDir::new(static_dir.join("assets")))
        .layer(DefaultBodyLimit::max(
            max_file.saturating_add(2 * 1024 * 1024),
        ))
        .layer(middleware::from_fn(security_headers))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = config
        .bind
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid CHAT_BIND `{}`: {e}", config.bind))?;

    tracing::info!("Chat Transfer listening on http://{addr}");
    if !config.secure_cookie {
        tracing::warn!(
            "CHAT_SECURE_COOKIE is false — enable it (and HTTPS) for production deployments"
        );
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

async fn security_headers(req: axum::extract::Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    headers.insert(
        header::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    headers.insert(
        header::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' blob: data:; media-src 'self' blob:; connect-src 'self' ws: wss:; \
             object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'",
        ),
    );
    let _ = Method::GET;
    res
}

fn resolve_static_dir() -> PathBuf {
    let candidates = [
        PathBuf::from("static"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static"),
    ];
    for c in candidates {
        if c.join("index.html").is_file() {
            return c;
        }
    }
    PathBuf::from("static")
}
