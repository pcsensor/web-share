mod auth;
mod chat;
mod config;
mod db;
mod files;
mod models;

use auth::AuthState;
use axum::{
    extract::DefaultBodyLimit,
    http::{header, HeaderValue, Method},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load `.env` from the current working directory (and parents).
    // Existing process env vars take precedence and are not overwritten.
    match dotenvy::dotenv() {
        Ok(path) => eprintln!("loaded env file: {}", path.display()),
        Err(dotenvy::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("warning: could not load .env: {e}"),
    }

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "chat_transfer=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env()?;
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

    // Startup purge
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

    // Background session cleanup
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

    // Background data retention purge
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
        .route("/api/login", post(auth::login))
        .route("/api/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        .route("/api/messages", get(chat::get_messages))
        .route("/api/messages/text", post(chat::post_text))
        .route("/api/upload", post(files::upload))
        .route("/api/files/{id}", get(files::preview))
        .route("/api/files/{id}/download", get(files::download))
        .route("/ws", get(chat::ws_handler));

    let app = Router::new()
        .merge(api)
        .route_service("/", ServeFile::new(static_dir.join("index.html")))
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
