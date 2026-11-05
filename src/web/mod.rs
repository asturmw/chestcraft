//! HTTP server: shared state, routes and middleware.

mod assets;
mod client;
mod files;
mod headers;
mod limits;
mod query;
mod stream;
#[cfg(test)]
mod tests;
mod views;

use crate::auth::perm::Principal;
use crate::config::Config;
use crate::fs::volume::Volumes;
use crate::i18n::{I18n, Lang};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

pub use client::ClientInfo;

pub struct AppState {
    pub cfg: Arc<Config>,
    pub volumes: Volumes,
    pub i18n: I18n,
    pub limiter: limits::DownloadLimiter,
    /// Lets tests act as a given principal until sessions exist.
    #[cfg(test)]
    pub test_principal: std::sync::Mutex<Option<Principal>>,
}

pub type Shared = Arc<AppState>;

impl AppState {
    pub fn new(cfg: Config, i18n: I18n) -> AppState {
        let volumes = Volumes::open(&cfg);
        AppState {
            cfg: Arc::new(cfg),
            volumes,
            i18n,
            limiter: limits::DownloadLimiter::default(),
            #[cfg(test)]
            test_principal: std::sync::Mutex::new(None),
        }
    }

    /// UI language for a request: the `cc_lang` cookie, else the browser's preference.
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
        .fallback(files::handle)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            principal_layer,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), client::layer))
        .layer(middleware::from_fn(headers::security))
        .with_state(state)
}

/// Attaches the request's principal. Sessions arrive in the auth slice; until then every
/// request is anonymous.
async fn principal_layer(State(st): State<Shared>, mut req: Request, next: Next) -> Response {
    #[cfg(test)]
    let principal = st
        .test_principal
        .lock()
        .unwrap()
        .clone()
        .unwrap_or(Principal::Anonymous);
    #[cfg(not(test))]
    let principal = {
        let _ = &st;
        Principal::Anonymous
    };
    req.extensions_mut().insert(principal);
    next.run(req).await
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

/// Run the server until SIGTERM or Ctrl-C.
pub async fn serve(cfg: Config, config_dir: Option<&Path>) -> anyhow::Result<()> {
    let i18n = I18n::load(config_dir.map(|d| d.join("i18n")).as_deref());
    let state = Arc::new(AppState::new(cfg, i18n));
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
