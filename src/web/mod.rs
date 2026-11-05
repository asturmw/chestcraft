//! HTTP server: shared state, routes and middleware.

mod assets;
mod auth;
mod client;
mod files;
mod headers;
mod limits;
mod query;
mod stream;
#[cfg(test)]
mod tests;
mod views;

use crate::auth::backoff::Backoff;
use crate::auth::perm::Principal;
use crate::config::Config;
use crate::db::Db;
use crate::fs::volume::Volumes;
use crate::i18n::{I18n, Lang};
use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

pub use auth::{Current, CurrentUser};
pub use client::ClientInfo;

pub struct AppState {
    pub cfg: Arc<Config>,
    pub volumes: Volumes,
    pub i18n: I18n,
    pub db: Arc<Db>,
    pub backoff: Backoff,
    pub limiter: limits::DownloadLimiter,
    /// Lets tests act as a principal without signing in.
    #[cfg(test)]
    pub test_principal: std::sync::Mutex<Option<Principal>>,
}

pub type Shared = Arc<AppState>;

impl AppState {
    pub fn new(cfg: Config, i18n: I18n, db: Db) -> AppState {
        let volumes = Volumes::open(&cfg);
        AppState {
            cfg: Arc::new(cfg),
            volumes,
            i18n,
            db: Arc::new(db),
            backoff: Backoff::default(),
            limiter: limits::DownloadLimiter::default(),
            #[cfg(test)]
            test_principal: std::sync::Mutex::new(None),
        }
    }

    /// UI language from the `cc_lang` cookie, else the browser's preference.
    pub fn request_lang(&self, headers: &HeaderMap) -> Lang {
        cookie(headers, "cc_lang")
            .and_then(|c| self.i18n.by_code(c))
            .unwrap_or_else(|| {
                self.i18n.negotiate(
                    headers
                        .get(header::ACCEPT_LANGUAGE)
                        .and_then(|v| v.to_str().ok()),
                )
            })
    }
}

/// Rendering context: the account's settings first, then cookies and the browser.
pub fn page_ctx<'a>(
    st: &'a AppState,
    headers: &HeaderMap,
    principal: &Principal,
    current: Option<&'a CurrentUser>,
) -> views::Ctx<'a> {
    let settings = current.map(|c| &c.settings);
    let lang = settings
        .and_then(|s| s.lang.as_deref())
        .and_then(|code| st.i18n.by_code(code))
        .unwrap_or_else(|| st.request_lang(headers));
    let theme = match settings
        .and_then(|s| s.theme.as_deref())
        .or_else(|| cookie(headers, "cc_theme"))
    {
        Some("light") => Some("light"),
        Some("dark") => Some("dark"),
        _ => None,
    };
    let skin = settings
        .and_then(|s| s.skin.as_deref())
        .filter(|s| assets::skin_css(s).is_some())
        .unwrap_or(assets::default_skin());
    let keys = match settings.and_then(|s| s.keys.as_deref()) {
        Some("vim") => "vim",
        Some("off") => "off",
        _ => "standard",
    };
    views::Ctx {
        i18n: &st.i18n,
        lang,
        theme,
        skin,
        keys,
        signed_in: !principal.is_anonymous(),
        user_name: current.map(|c| c.name.as_str()),
        csrf: current.map(|c| c.csrf.as_str()),
        tz: &st.cfg.server.timezone,
    }
}

/// Value of one cookie from the `Cookie` header(s).
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(files::landing))
        .route("/healthz", get(healthz))
        .route("/robots.txt", get(robots))
        .route("/favicon.ico", get(favicon))
        .route("/.cc/assets/{*path}", get(assets::serve))
        .route("/.cc/login", get(auth::login_page).post(auth::login_submit))
        .route("/.cc/logout", post(auth::logout))
        .route(
            "/.cc/password",
            get(auth::password_page).post(auth::password_submit),
        )
        .fallback(files::handle)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::principal_layer,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), client::layer))
        .layer(middleware::from_fn(headers::security))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok\n"
}

async fn robots() -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "User-agent: *\nDisallow: /\n",
    )
        .into_response()
}

async fn favicon() -> StatusCode {
    StatusCode::NO_CONTENT
}

/// Open (creating if needed) the state database in `state_dir`.
pub fn open_db(cfg: &Config) -> anyhow::Result<Db> {
    let dir = &cfg.paths.state_dir;
    if !dir.is_dir() {
        std::fs::create_dir_all(dir)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(Db::open(&dir.join(crate::db::FILE_NAME))?)
}

/// Run the server until SIGTERM or Ctrl-C.
pub async fn serve(cfg: Config, config_dir: Option<&Path>) -> anyhow::Result<()> {
    let i18n = I18n::load(config_dir.map(|d| d.join("i18n")).as_deref());
    let db = open_db(&cfg)?;
    let state = Arc::new(AppState::new(cfg, i18n, db));
    let addr = state.cfg.server.listen;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, volumes = state.volumes.iter().count(), "chestcraft listening");
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    tracing::info!("shutting down");
}
