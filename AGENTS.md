# chestcraft: notes for coding agents

Read this first. It tells you what the project is, how to build and run it, where things
live, and the rules a change must keep. Read the source files only for the part you work on.

## What it is

A self-hosted file manager and sharing app for a home NAS, in Rust. One static binary
serves a web UI (browse, preview, upload, trash), password-protected share links, WebDAV,
an admin dashboard with ZFS/md/SMART/host health and alerts (Telegram, ntfy), `/healthz`
and a status API, and an MCP server for AI agents. Server-rendered HTML (axum + maud), a
little plain JS, no JS build step, state in one redb file. Early development: the backend
is mostly done, the final UI is still being designed.

## Docs

| File | Read it when |
|---|---|
| `README.md` | Feature overview |
| `config.example.toml` | Every config setting, with comments: the schema reference |
| `docs/install.md` | Release build, install, updates, reload, staging, backup and restore |
| `docs/api.md` | `/healthz` and `/api/v1/status` |
| `docs/mcp.md` | The MCP server: setup, agents, tokens, tools |
| `docs/ui-handoff.md` | The UI brief; section 5 is the markup/CSS contract the views follow |

## Build, test, run

Rust stable (edition 2024), Linux only (uses `openat2`, `/proc`, `/sys`).

```sh
cargo test                    # all tests (in-crate; dependencies are built with opt-level 2)
cargo clippy --all-targets -- -D warnings
deploy/release.sh             # fmt, clippy, tests, static musl build with no C compiler
                              # (needs the x86_64-unknown-linux-musl target)
```

Local run with a throwaway config (absolute paths; the admin socket path must stay under
108 bytes). Missing `zpool`/`zfs`/`smartctl` only switch the dashboard sections off:

```toml
# dev/config.toml
[server]
listen = "127.0.0.1:3999"
public_url = "http://127.0.0.1:3999"

[paths]
state_dir = "/tmp/cc-dev/state"
cache_dir = "/tmp/cc-dev/cache"
backup_dir = "/tmp/cc-dev/backup"     # must exist
smart_file = "/tmp/cc-dev/smart.json"
admin_socket = "/tmp/cc-dev/admin.sock"

[volumes.Test]
path = "/tmp/cc-dev/files/Test"      # must exist
```

```sh
mkdir -p /tmp/cc-dev/backup /tmp/cc-dev/files/Test
cargo run -- -c dev/config.toml check                       # validate config + machine
cargo run -- -c dev/config.toml user add dev --role admin   # prints a one-time password
cargo run -- -c dev/config.toml serve                       # http://127.0.0.1:3999/
```

`/healthz` answers `collector` instead of `ok` until a SMART reading exists; that is
expected locally. With no server running, `user`/`agent`/`api-token`/`backup` and
`import-copyparty` refuse to run as root (they would leave root-owned state files); run them
as the user that owns `state_dir`.

## How it fits together

- **Binary** (`src/main.rs`, clap): `serve`, `check`, `collect` (root, SMART readings to
  `paths.smart_file`), `telegram-setup`, `import-copyparty`, and the state commands `user`,
  `agent`, `api-token`, `backup` (`src/cli.rs`). Those go through the running server's
  admin socket (`web/adminsock.rs`, SO_PEERCRED) or, with no server, open the DB directly.
- **Requests** (`web/mod.rs` `router`): client origin from trusted proxies (`web/client.rs`:
  local vs. remote, Cloudflare) → session (`web/auth.rs`) → permissions
  (`auth/perm.rs` `volume_perms(cfg, principal, origin, volume)`) → file access through
  `fs/volume.rs` → HTML from maud views with strings from `i18n`.
- **Paths:** URLs become a `RelPath` (`fs/path.rs`: validated, no `..`, no internal names).
  Each volume holds an `O_PATH` fd of its root; every open is `openat2(RESOLVE_BENEATH)`
  under it, so the kernel stops symlink escapes. Blocking fs calls run in `spawn_blocking`.
- **Volume URLs** carry the web UI (GET) and WebDAV (other methods, `src/dav/`) on the same
  paths. `/.cc/…` is the app's own namespace, `/s/<id>` share links.
- **Background work:** `web/monitor.rs` takes a health snapshot every minute and runs the
  alert rules (`health/alerts.rs`) → `notify/`; `web/jobs.rs` runs the daily trash purge,
  upload sweep and state backup at `trash.purge_at`.
- **MCP** (`src/mcp/`): own listener (`[mcp] listen`), LAN only, agent accounts with
  bearer tokens; tools reuse the same `fs` primitives and permission checks as web/WebDAV.

## Where things are

| Area | Code |
|---|---|
| Config schema + validation | `config/mod.rs` (`deny_unknown_fields`), secrets `config/secrets.rs` |
| State DB (all tables, one file) | `db/mod.rs` |
| Accounts, sessions, passwords, backoff, tokens | `auth/` (shared account changes: `auth/accounts.rs`) |
| Admin accounts page, audit log, app passwords + sessions on the account page | `web/users.rs` |
| CLI state commands, admin socket | `cli.rs`, `web/adminsock.rs` |
| Nightly state backup | `backup.rs` (run from `web/jobs.rs`) |
| Folder listing, downloads, zip, tree listing, download slots | `web/files.rs`, `fs/zipstream.rs`, `fs/tree.rs`, `fs/sort.rs` |
| Uploads (chunked, resumable) | `fs/upload.rs`, `web/api.rs`, `assets/upload.js` |
| Move/copy/new file, no-overwrite renames | `fs/transfer.rs`, `fs/ops.rs`, `fs/names.rs`; JSON API for the UI `web/ops.rs` |
| Trash | `fs/trash.rs`, `web/trash.rs` |
| Thumbnails + viewer | `fs/thumbs/`, `fs/filetype.rs`, `web/preview.rs` |
| Shares, public uploads | `shares/`, `web/share.rs` (visitors), `web/shares.rs` (owners), `web/public.rs`, `web/guest.rs` |
| WebDAV | `dav/` |
| Health sources | `health/` (zpool, zfs, mdstat, disks, smart, host, history) |
| Alerts, channels | `health/alerts.rs`, `notify/` |
| Dashboard, status API | `web/dashboard.rs`, `web/charts.rs`, `web/status.rs` |
| Preferences, skins, backgrounds | `appearance/`, `web/account.rs` |
| copyparty account import | `import/` |
| External programs (absolute paths) | `tools.rs`, `health/cmd.rs` |
| UI strings | `i18n/en.toml`, `i18n/de.toml`, `src/i18n.rs` |
| App shell (app bar, sidebar tree, tab bar), browser view, gate pages | `web/views.rs`, `web/nav.rs` |
| CSS/JS/icons/fonts (embedded by `build.rs`) | `assets/`: `tokens.css`, `app.css`, `skins/`, `icons.svg`, `fonts/`, `backgrounds/` come from the `design` branch via `deploy/design-sync.sh` (never edit them here); own: `app.js` (menus, dialogs), `browser.js` (selection, filter, row menu), `download.js` (save to folder, separately, queue), `ops.js` (clipboard, rename, move/copy picker), `keys.js` (Vim/Standard maps), `upload.js`, `provisional.css` (screens not designed yet) |
| Deploy | `deploy/release.sh`, `deploy/install.sh`, `deploy/units/` |

## Rules a change must keep

- **Pure Rust.** Only crates that build without a C compiler: the release build runs with
  `CC=/bin/false CXX=/bin/false`. External programs only as subprocesses by absolute path
  (`/usr/sbin/zpool`, `/usr/sbin/zfs`, `/usr/sbin/smartctl`, `/usr/bin/ffmpeg`,
  `/usr/bin/curl`), with an empty environment and a timeout. Secrets go on stdin, never in
  argv.
- **Never lose user files.** Only the trash purge and the cleanup of abandoned `.part`
  uploads delete user files. Everything else renames without overwriting
  (`rename_noreplace`); replacing a file moves the old one to the trash first. (WebDAV
  PUT/MOVE with overwrite are the protocol's exception.)
- **Paths and permissions.** Parse every user-supplied path into `RelPath` and open it
  through the volume's root fd, never by joining strings onto a filesystem path. Check
  permissions for the request's origin: a volume can be home-only (`remote = false`), and
  shares never grant more than their creator has as seen from the internet.
- **Internal names** (`.chestcraft-*`, `.hist`) stay invisible everywhere: listings, zips,
  WebDAV, MCP.
- **i18n.** Every UI string lives in both `i18n/en.toml` and `i18n/de.toml` with the same
  placeholders; a test fails otherwise.
- **UI contract.** Views follow `docs/ui-handoff.md` section 5 (class names, no inline
  styles) and the design's prototypes (`design/proto/` on the `design` branch). `design/`
  is owned by a separate design branch; don't edit it on `main`, and don't edit the copies
  `deploy/design-sync.sh` puts into `assets/`. Screens the design hasn't delivered render
  inside `main.panel` with `assets/provisional.css`. JS hooks are `data-js`; scripts never
  select by class.
- **DB tables.** A new table is created in `Db::open` and copied in `Db::copy_to`; the backup
  test compares table lists.
- **Public repo.** No secrets, real hostnames, IPs, user names or paths. Tests and fixtures
  use fictional data (`example.org`, `192.0.2.0/24`), disk serials anonymized.
  Deployment-specific files go in `deploy/site/` (gitignored).
- **Install.** The installed binary is root-owned (the root SMART collector runs it) and
  never writable by the service user.

## Tests

Tests sit next to the code (`#[cfg(test)]`). The big ones run the full router against temp
dirs: `web/tests.rs` (`env()` builds volumes, config, DB and the app; helpers for cookies,
CSRF, forms, JSON), `web/tests/*.rs`, `mcp/tests.rs`, `fs/thumbs/tests.rs`. Tool output samples
(zpool, zfs, smartctl JSON) are in `fixtures/`. A change is done when `deploy/release.sh`
passes (fmt, clippy `-D warnings`, tests, C-free static build).

## Common changes

- **Config setting:** field in `config/mod.rs` (with default and validation), documented in
  `config.example.toml`; `chestcraft check` must catch bad values.
- **Page or route:** handler in `web/<area>.rs`, route in `web::router`, markup in the
  area's views file, strings in both i18n files, a test in `web/tests.rs`.
- **MCP tool:** definition and dispatch in `mcp/tools.rs`, body in `mcp/files.rs` /
  `shares.rs` / `health.rs`, row in the `docs/mcp.md` table, test in `mcp/tests.rs`.
- **Alert rule:** `health/alerts.rs` (stable key; messages are i18n keys + arguments, so
  the texts go in both i18n files),
  tests in `health/alerts/tests.rs`.
- **Status API field:** add only, never rename or remove (`api: 1`); document it in
  `docs/api.md`.
