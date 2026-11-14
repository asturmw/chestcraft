//! HTTP server: shared state, routes and middleware.

mod account;
mod account_views;
mod activity;
mod adminsock;
mod api;
mod assets;
mod auth;
mod charts;
mod client;
mod dashboard;
mod du;
mod files;
mod guest;
mod headers;
pub mod jobs;
mod layout;
mod limits;
pub mod monitor;
mod nav;
mod ops;
mod preview;
mod public;
mod query;
mod share;
mod share_views;
mod shares;
pub mod status;
mod stream;
#[cfg(test)]
mod tests;
mod trash;
mod users;
mod views;

use crate::auth::backoff::Backoff;
use crate::auth::perm::Principal;
use crate::config::Config;
use crate::db::Db;
use crate::fs::upload::Uploads;
use crate::fs::volume::Volumes;
use crate::i18n::{I18n, Lang};
use axum::Router;
use axum::extract::{Extension, Request, State};
use axum::handler::Handler;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get, post, put};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

pub use auth::{Current, CurrentUser, audit, now};
pub use client::ClientInfo;
pub use headers::security as security_headers;
pub use monitor::raise;

pub struct AppState {
    pub cfg: Arc<Config>,
    pub volumes: Volumes,
    pub i18n: I18n,
    pub db: Arc<Db>,
    pub backoff: Backoff,
    pub limiter: limits::DownloadLimiter,
    /// Downloads of signed-in users per IP, counted but not limited.
    pub account_downloads: limits::DownloadLimiter,
    pub uploads: Uploads,
    /// Passphrase word lists for share and public upload passwords.
    pub words: crate::shares::words::WordLists,
    pub thumbs: crate::fs::thumbs::Thumbs,
    /// New thumbnails per IP for anonymous visitors.
    pub thumb_limiter: governor::DefaultKeyedRateLimiter<std::net::IpAddr>,
    /// Folder sizes for listings (`?du`).
    pub folder_sizes: du::FolderSizes,
    /// Health snapshots, active alerts, chart history.
    pub health: monitor::Monitor,
    /// Telegram / ntfy.
    pub notifier: Arc<crate::notify::Notifier>,
    /// Appearance settings and background sets.
    pub looks: Arc<crate::appearance::Store>,
    /// Skins this build embeds (`skins/<name>.css`), sorted.
    pub skins: Vec<String>,
    /// Picture sets this build embeds (`backgrounds/<name>-<slot>.jpg`).
    pub builtin_sets: Vec<crate::appearance::Builtin>,
    /// Skins with an own background picture (`backgrounds/default-<skin>-<theme>.svg`).
    pub skin_pictures: std::collections::BTreeMap<String, crate::appearance::SkinPicture>,
    /// The config file the server was started with (copied by the nightly backup).
    pub config_file: Option<std::path::PathBuf>,
    /// Lets tests act as a principal without signing in.
    #[cfg(test)]
    pub test_principal: std::sync::Mutex<Option<Principal>>,
}

pub type Shared = Arc<AppState>;

impl AppState {
    pub fn new(
        cfg: Config,
        i18n: I18n,
        db: Db,
        secrets: &crate::config::secrets::Secrets,
    ) -> AppState {
        let volumes = Volumes::open(&cfg);
        let db = Arc::new(db);
        let notifier = Arc::new(crate::notify::Notifier::new(&cfg, secrets, db.clone()));
        let (words, warnings) = crate::shares::words::WordLists::load(&cfg.shares.wordlists);
        for w in warnings {
            tracing::warn!("{w}");
        }
        let looks = Arc::new(crate::appearance::Store::open(&db, &cfg.paths.state_dir));
        let skins: Vec<String> = assets::skins().into_iter().map(String::from).collect();
        AppState {
            looks,
            builtin_sets: crate::appearance::builtins(assets::has),
            skin_pictures: crate::appearance::skin_pictures(&skins, assets::has),
            skins,
            config_file: None,
            thumbs: crate::fs::thumbs::Thumbs::new(&cfg),
            thumb_limiter: preview::anon_limiter(),
            folder_sizes: du::FolderSizes::default(),
            health: monitor::Monitor::default(),
            notifier,
            cfg: Arc::new(cfg),
            volumes,
            i18n,
            uploads: Uploads::new(db.clone()),
            words,
            db,
            backoff: Backoff::default(),
            limiter: limits::DownloadLimiter::default(),
            account_downloads: limits::DownloadLimiter::default(),
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

/// Rendering context: the account's settings first, then cookies and the browser. Pages
/// for visitors without an account use the admin's skin and background for those pages.
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
    let keys = match settings.and_then(|s| s.keys.as_deref()) {
        Some("vim") => "vim",
        Some("off") => "off",
        _ => "standard",
    };
    let look = account::look(st, current);
    let mut ctx = views::Ctx {
        i18n: &st.i18n,
        lang,
        theme,
        skin: look.skin,
        glass: look.glass.code(),
        opacity: look.opacity,
        keys,
        backdrop: look.backdrop,
        signed_in: !principal.is_anonymous(),
        admin: principal.is_admin(),
        user_name: current.map(|c| c.name.as_str()),
        csrf: current.map(|c| c.csrf.as_str()),
        tz: &st.cfg.server.timezone,
        scripts: if current.is_some() {
            vec!["upload.js"]
        } else {
            Vec::new()
        },
        styles: Vec::new(),
        lang_switch: None,
        brand: true,
        sign_in: true,
        nav: None,
        skins: Vec::new(),
    };
    if current.is_some() {
        ctx.skins = st
            .skins
            .iter()
            .map(|k| (k.clone(), account::skin_label(&ctx, k)))
            .collect();
    }
    ctx
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

/// WebDAV methods go to the WebDAV handler, everything else to the folder handler.
async fn dav_or_files(State(st): State<Shared>, req: Request) -> Response {
    if crate::dav::is_dav_method(req.method()) {
        return crate::dav::handle(st, req).await;
    }
    files::handle.call(req, st).await
}

/// `/`: the landing page for GET, the volume list for WebDAV.
async fn dav_or_405(State(st): State<Shared>, req: Request) -> Response {
    if crate::dav::is_dav_method(req.method()) {
        return crate::dav::handle(st, req).await;
    }
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(files::landing).fallback(dav_or_405))
        .route("/robots.txt", get(robots))
        .route("/favicon.ico", get(favicon))
        .route("/.cc/assets/{*path}", get(assets::serve))
        .route("/.cc/login", get(auth::login_page).post(auth::login_submit))
        .route("/.cc/logout", post(auth::logout))
        .route(
            "/.cc/password",
            get(auth::password_page).post(auth::password_submit),
        )
        .route("/.cc/upload", post(api::create))
        .route("/.cc/upload/{id}", get(api::status).delete(api::cancel))
        .route("/.cc/upload/{id}/chunk/{n}", put(api::chunk))
        .route("/.cc/upload/{id}/done", post(api::done))
        .route("/.cc/batch", post(api::batch_new))
        .route("/.cc/batch/{id}/done", post(api::batch_done))
        .route("/.cc/rename", post(ops::rename))
        .route("/.cc/move", post(ops::move_to))
        .route("/.cc/copy", post(ops::copy_to))
        .route("/.cc/trash", get(trash::page))
        .route("/.cc/trash/restore", post(trash::restore))
        .route("/.cc/trash/empty", post(trash::empty))
        .route("/.cc/lang", get(lang))
        .route("/.cc/account", get(account::page).post(account::save_prefs))
        .route(
            "/.cc/account/backgrounds",
            get(account::backgrounds_page).post(account::backgrounds_submit),
        )
        .route(
            "/.cc/account/backgrounds/upload",
            post(account::upload).layer(axum::extract::DefaultBodyLimit::max(
                3 * crate::appearance::MAX_UPLOAD_BYTES + (1 << 20),
            )),
        )
        .route(
            "/.cc/admin/appearance",
            get(account::appearance_page).post(account::appearance_submit),
        )
        .route(
            "/.cc/account/app-passwords",
            post(users::app_passwords_submit),
        )
        .route("/.cc/account/sessions", post(users::sessions_submit))
        .route(
            "/.cc/admin/users",
            get(users::users_page).post(users::users_submit),
        )
        .route("/.cc/admin/audit", get(users::audit_page))
        .route("/.cc/bg/{id}/{file}", get(account::picture))
        .route("/.cc/backdrop.css", get(account::backdrop_css))
        .route("/.cc/presets.css", get(account::presets_css))
        .route("/.cc/dl-slots", get(files::dl_slots))
        .route("/.cc/shares", get(shares::list))
        .route(
            "/.cc/shares/new",
            get(shares::new_page).post(shares::new_submit),
        )
        .route(
            "/.cc/shares/{id}",
            get(shares::detail).post(shares::detail_submit),
        )
        .route(
            "/.cc/public-write",
            get(public::unlock_page).post(public::unlock_submit),
        )
        .route(
            "/.cc/admin/volumes",
            get(public::volumes_page).post(public::volumes_submit),
        )
        .route("/.cc/admin/dashboard", get(dashboard::page))
        .route(
            "/.cc/admin/alerts",
            get(dashboard::alerts_page).post(dashboard::alerts_submit),
        )
        .route("/s/{id}", any(share::handle))
        .route("/s/{id}/", any(share::handle))
        .route("/s/{id}/{*rest}", any(share::handle))
        .route("/.cc/admin/api-tokens", post(public::tokens_submit))
        .fallback(dav_or_files)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::principal_layer,
        ))
        // No sessions or Basic auth here: these answer even when the database hangs, and
        // the API takes only its own tokens.
        .route("/healthz", get(status::healthz))
        .route("/api/v1/status", get(status::status))
        // `client::layer` must stay outermost: `headers::security` reads `ClientInfo` from
        // the request to decide whether to set HSTS (HTTPS requests only).
        .layer(middleware::from_fn(headers::security))
        .layer(middleware::from_fn_with_state(state.clone(), client::layer))
        .with_state(state)
}

#[derive(serde::Deserialize)]
struct LangQuery {
    set: String,
    #[serde(default)]
    next: String,
}

/// `/.cc/lang?set=de&next=/.cc/login…`: the language switch of pages without an account
/// (sign-in, public volumes, share contents).
async fn lang(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    axum::extract::Query(q): axum::extract::Query<LangQuery>,
) -> Response {
    let mut res = Redirect::to(&auth::safe_next(&q.next)).into_response();
    if st.i18n.by_code(&q.set).is_some() {
        let secure = if client.https { "; Secure" } else { "" };
        let cookie = format!(
            "cc_lang={}; Path=/; SameSite=Lax; Max-Age=31536000{secure}",
            q.set
        );
        if let Ok(v) = axum::http::HeaderValue::from_str(&cookie) {
            res.headers_mut().insert(header::SET_COOKIE, v);
        }
    }
    res
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

/// How the server ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// SIGTERM or Ctrl-C.
    Stop,
    /// SIGHUP with a valid configuration: start again with it.
    Restart,
}

/// Open connections get this long to finish when the server stops or restarts.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Run the server until SIGTERM or Ctrl-C, or until SIGHUP brings a valid new
/// configuration (an invalid one is logged and the running one stays).
pub async fn serve(cfg: Config, config_file: &Path) -> anyhow::Result<Exit> {
    use std::future::IntoFuture;
    let config_dir = config_file.parent();
    let i18n = I18n::load(config_dir.map(|d| d.join("i18n")).as_deref());
    let secrets = match crate::config::secrets::Secrets::location(config_dir) {
        Some(path) => crate::config::secrets::Secrets::load(&path).unwrap_or_else(|e| {
            tracing::error!("secrets: {e}; alert channels that need a token stay off");
            Default::default()
        }),
        None => Default::default(),
    };
    let db = open_db(&cfg)?;
    let mut state = AppState::new(cfg, i18n, db, &secrets);
    state.config_file = Some(config_file.to_path_buf());
    let state = Arc::new(state);
    drop(secrets);
    for c in state.notifier.channels() {
        match &c.problem {
            Some(p) => tracing::warn!(channel = c.kind.name(), "alert channel off: {p}"),
            None => {
                tracing::info!(channel = c.kind.name(), to = %c.destination(), "alert channel on")
            }
        }
    }
    jobs::sweep_leftovers(state.clone()).await;
    jobs::spawn(state.clone());
    monitor::spawn(state.clone());
    state.thumbs.start();
    let addr = state.cfg.server.listen;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, volumes = state.volumes.iter().count(), version = crate::VERSION, "chestcraft listening");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let exit = Arc::new(std::sync::Mutex::new(Exit::Stop));
    tokio::spawn({
        let (shutdown, exit, file) = (shutdown.clone(), exit.clone(), config_file.to_path_buf());
        async move {
            *exit.lock().unwrap() = signals(file).await;
            shutdown.cancel();
        }
    });
    let sock = state.cfg.paths.admin_socket.clone();
    match adminsock::bind(&sock) {
        Ok(Some(listener)) => {
            tracing::info!(path = %sock.display(), "CLI commands accepted on the admin socket");
            tokio::spawn(adminsock::serve(
                state.clone(),
                listener,
                sock,
                shutdown.clone(),
            ));
        }
        Ok(None) => tracing::info!(
            "admin socket off ({} has no folder): CLI state commands need the server stopped",
            sock.display()
        ),
        Err(e) => tracing::warn!("admin socket off: {e}"),
    }
    let mcp = match state.cfg.mcp.listen {
        Some(mcp_addr) => {
            let (st, token) = (state.clone(), shutdown.clone());
            Some(tokio::spawn(async move {
                let result = crate::mcp::serve(st, mcp_addr, token.clone()).await;
                if result.is_err() {
                    // Without its MCP server chestcraft stops, so the problem gets noticed.
                    token.cancel();
                }
                result
            }))
        }
        None => None,
    };
    let done = shutdown.clone();
    let server = axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { done.cancelled().await })
    .into_future();
    let grace = shutdown.clone();
    tokio::select! {
        r = server => r?,
        _ = async { grace.cancelled().await; tokio::time::sleep(SHUTDOWN_GRACE).await } => {
            tracing::warn!("connections still open after {} s are cut", SHUTDOWN_GRACE.as_secs());
        }
    }
    shutdown.cancel();
    if let Some(task) = mcp {
        match tokio::time::timeout(SHUTDOWN_GRACE, task).await {
            Ok(r) => r??,
            Err(_) => tracing::warn!("MCP connections still open are cut"),
        }
    }
    let exit = *exit.lock().unwrap();
    Ok(exit)
}

/// Wait for SIGTERM / Ctrl-C (stop) or a SIGHUP whose configuration passes the same checks
/// as `chestcraft check` (restart).
async fn signals(config_file: std::path::PathBuf) -> Exit {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut hup = signal(SignalKind::hangup()).expect("install SIGHUP handler");
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = term.recv() => break,
            _ = hup.recv() => {
                let file = config_file.clone();
                let verdict = tokio::task::spawn_blocking(move || validate(&file)).await;
                match verdict {
                    Ok(Ok(())) => {
                        tracing::info!("configuration accepted; restarting with it");
                        return Exit::Restart;
                    }
                    Ok(Err(e)) => tracing::error!(
                        "{}: configuration rejected, the running one stays:\n{e}",
                        config_file.display()
                    ),
                    Err(_) => tracing::error!("checking the configuration crashed"),
                }
            }
        }
    }
    tracing::info!("shutting down");
    Exit::Stop
}

/// The checks of `chestcraft check`: the file, then the machine it points to.
pub fn validate(config_file: &Path) -> Result<(), String> {
    let cfg = Config::load(config_file).map_err(|e| e.to_string())?;
    let report = crate::config::check_environment(&cfg);
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(report.errors.join("\n"))
    }
}
