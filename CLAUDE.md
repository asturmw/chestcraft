# chestcraft

Rust file-sharing app for a home NAS: browse, upload, share links, WebDAV, and an admin
dashboard with ZFS/SMART health and Telegram alerts. Replaces copyparty.

@deploy/site/CLAUDE.site.md

## Status

Plan steps 1–3 done (step 3 = the backend slices below) plus the deploy tooling (slice
13). Next: NAS setup (step 4), UI integration (7), staging (8).
- Slice 1 done: config schema + validation (`chestcraft check`), `RelPath`, permissions.
- Slice 2 done: `chestcraft serve` with landing page, folder listings (natural sort,
  paging, hidden files for admin), downloads (Range, ETag, sandboxed active types), streamed
  zip (folder or selection), client origin through trusted proxies, security headers,
  anonymous download limit, i18n (built-in de/en + extra files), embedded assets
  (`build.rs`). Everyone is anonymous until slice 3.
- Slice 3 done: redb state DB (accounts, sessions, audit), argon2id passwords, login with
  per-(IP, account) backoff and credential-stuffing signal, sessions (hashed ids, 30d
  sliding, remote admin 12h), forced first-login password change, CSRF + Origin checks,
  per-user settings in the page context, `chestcraft user ...` CLI (not as root).
- Slice 4 done: chunked resumable uploads (`/.cc/upload` JSON API + `assets/upload.js`,
  `.part` files in `.chestcraft-uploads` on the target's dataset, chunks recorded after
  fdatasync, client mtime, 644/755), name clash keep both / replace (old file → trash via
  RENAME_EXCHANGE) / skip, staged folder batches, new folder, delete → trash, trash page
  with restore (never overwrites) and admin "empty now", daily purge + abandoned-upload
  sweep at `trash.purge_at` (abandoned staged folders go to the trash, not deleted).
- Slice 5 done: own WebDAV (`src/dav/`, quick-xml; dav-server dropped) on the volume URLs,
  dispatched by method; LAN/Tailscale only; Basic auth with per-device app passwords
  (SHA-256 stored, revocable, dead while the account is disabled or must change its
  password, same backoff as sign-in; `chestcraft user app-password add|list|revoke`).
  PROPFIND 0/1 (root quota = roomiest readable volume, for `rclone about`/`df`),
  PROPPATCH mtime only, MKCOL, PUT via temp file + `X-OC-Mtime`, DELETE → trash, COPY/MOVE
  (`fs/transfer.rs`: no-overwrite renames, cross-filesystem move = copy + source to trash,
  temp entries recorded in the DB and swept at startup/daily), advisory LOCK/UNLOCK.
  Verified with rclone copy/check/sync/moveto/copyto/about and an rclone FUSE mount.
- Slice 6 done: shares (`src/shares/`, `web/share.rs` visitors, `web/shares.rs` owners):
  `/s/<12 base58>`, modes view / view_upload / upload (visitor sees only own uploads),
  password always (4-word passphrase from `src/shares/words/` in the creator's language,
  checked without case/separators, or own ≥ 8), expiry 1d/7d/30d/never, note, per-file size
  limit, guest session 12h (cookie path `/s/<id>`, ended by new password/revoke), visitor
  upload API under `/s/<id>/.cc/…` (same `web/api.rs` code via `Uploader`), visitors never
  overwrite. Creator's rights (as seen from the internet) checked on every request →
  paused when disabled/rights lost/volume gone/target in trash; follows moves (inside
  `transfer::move_entry`) and "(restored)" renames; otherwise "missing". Access log
  (`share_log`, IP + CF country) + audit; my shares / all shares (admin) / details with
  edit, new password, revoke; share links + badges in listings, reason on home-only volumes.
  Public write: admin `/.cc/admin/volumes` (open 1d/7d/30d/never with password, size limit,
  notify flag; reopen = new password; close), visitors unlock at `/.cc/public-write` →
  upload + new folder, never overwrite. Same backoff as sign-in for both passwords.
  Language switch `/.cc/lang` on pages without an account. Delivery of the public-upload
  notify flag, "share locked" and "target missing" notifications wait for the alerts slice.
- Slice 7 done: thumbnails (`src/fs/thumbs/`, `src/fs/filetype.rs`): `?thumb` (480 px) and
  `?thumb=view` (2048 px), never enlarged; JPEG, PNG when transparent. Sources: images
  (image crate, EXIF orientation), HEIC/AVIF/video frames (ffmpeg on the open fd as
  `/proc/self/fd/0`, demuxer + protocol whitelist, empty env, timeout), STL + 3MF (embedded
  slicer picture first, else own rasterizer: components, `p:path`, transforms), G-code/
  bgcode (embedded picture), SVG (resvg, `<image>` loads only `data:`). Disk cache
  `<cache_dir>/thumbs` keyed by dev/ino/size/mtime, failures cached empty, LRU trim at
  `cache_gib`; one render per key at a time, CPU and ffmpeg semaphores; anonymous visitors
  (public volumes, shares) limited per IP for new renders (governor). URLs carry `v=` →
  immutable caching. Viewer `?view` (`web/preview.rs`): image/rendered thumbnail, video/
  audio players, PDF iframe, text, Markdown (raw HTML shown as code, unsafe link schemes
  dropped), prev/next by name, info, download/share/delete. Listings: icon type
  (`data-type`), thumbnail `<img>`, name → viewer, download link, `?layout=grid`. Share
  folders get the same; the single-file share card shows the preview.
- Slice 8 done: health (`src/health/`): every minute the server reads `zpool status/list -j`,
  `zfs list -j` (datasets + snapshots), `/proc/mdstat`, `/sys/block` (disks; vdev paths and
  md members resolved to their disk), /proc + hwmon (host) and the SMART file that
  `chestcraft collect` (root, `deploy/units/chestcraft-collect.{service,timer}`, every
  30 min, `-n standby` keeps the last reading of sleeping disks) writes to
  `paths.smart_file` (folder and file must belong to root). Tools run with an empty env,
  a timeout, and never twice while one hangs. Absent hardware = no section. Alerts
  (`health/alerts.rs`): rules → conditions with stable keys + events (counter rises);
  fire/resolve messages, criticals repeat every 24 h, escalation, hysteresis; a source
  that fails keeps its alerts (and raises its own); thresholds + snapshot ages in
  `[alerts.thresholds]` / `[alerts.snapshots]`. State, baselines, alert log (365 d),
  hourly history (90 d) and channel status in redb. Channels (`src/notify/`): Telegram +
  ntfy via curl with config + token on stdin, retries 1/5/15/60 min, secrets from
  `secrets.toml` / `$CREDENTIALS_DIRECTORY/secrets`; `chestcraft telegram-setup`. Events
  from requests: admin from a new remote IP, credential stuffing, share/public-upload
  password lockout (6 h cooldown), public uploads with notify (one per volume per 10 min).
  Pages: `/.cc/admin/dashboard` (verdict, alerts, pools with vdev tree + fill chart and
  projection, datasets with trash, disks with temp sparklines, md, host, app; 30-s
  refresh in `assets/dashboard.js`), `/.cc/admin/alerts` (active, channels + test button,
  history); verdict line on the admin landing page; weekly digest at `alerts.digest`.
  Fixtures: anonymized zpool/zfs output from the NAS, smartctl NVMe from another host,
  ATA hand-written (no SATA disk had smartctl yet). Not yet run as root against real
  disks: `collect` needs smartmontools on the NAS (setup step 4).
- Slice 9 done: preferences + appearance (`src/appearance/`, `web/account.rs`).
  `/.cc/account`: language, theme, skin, shortcuts, rotation (POST fields optional + `next`,
  for quick switches). Background editor `/.cc/account/backgrounds?skin=`: per skin default
  / none / color or 2–3-color gradient / picture sets (several rotate: never, load, session,
  day; default day), dim + blur 0–4. Uploads (multipart, 1–3 shapes wide 16:9 / square 3:4 /
  tall 10:16, ≤ 30 MiB each) are decoded in process (EXIF orientation), stored as JPEG ≤
  3840 px without metadata + 480 px preview in `<state_dir>/backgrounds/<id>/`, one
  conversion at a time; `[appearance] max_backgrounds_per_user` / `deny_upload_roles`,
  admins unlimited; new set is selected at once; half-written sets swept at startup. Admin
  `/.cc/admin/appearance`: default skin, share skin, default rotation, default background
  per skin and share background (`?edit=default:<skin>|share`, same editor), audited. Every
  page gets `.backdrop` (`data-state` none/color/image, `data-dim`, `data-blur`, `<picture>`
  with 2/3 and 5/4 aspect breakpoints; missing shapes fall back to the closest). Colors: the
  page links `/.cc/backdrop.css?c=1a2b3c-445566&a=135`, which sets `--backdrop-color` on
  `.backdrop[data-state="color"]` (no inline styles; the design session must adopt or
  replace this). Visitors without an account get the share skin + share background. Pictures
  load for their owner, or for everyone once the appearance settings use them. Skins =
  embedded `assets/skins/*.css` (none yet → `default`).
- Slice 10 done: `chestcraft import-copyparty <conf> [--role NAME=ROLE]… [--skip NAME]…
  [--dry-run]` (`src/import/`). Reads the copyparty file (never writes it) the way copyparty
  1.20 does: `  #` comments, `% ` includes (files, folders), `[x]:`, `[accounts]`, `a:` in
  `[global]`, `[groups]`, volume `accs:` with `*`, `@group`, `@acct`; what copyparty refuses
  is refused, v1 format too. Every importable account needs a role or `--skip` (else nothing
  happens); the report puts each account's copyparty access per volume next to its role's
  (matched by name, other case, or folder) and marks differences. Passwords never appear in
  output, `Debug` or errors. Imported accounts keep their copyparty password (argon2id) with
  a forced change; `ah-alg` hashes get a one-time password; empty password, invalid name,
  case duplicate or existing account → skipped (`Db::insert_user` never overwrites); audited
  as `user_imported` (actor `cli`). `/dev/stdin` works (`ssh host cat … | chestcraft …`).
- Slice 11 done: `/healthz` + status API (`web/status.rs`, `docs/api.md`). `/healthz` sits
  outside the session layer and answers from memory: `ok`, or 503 with `starting` / `db`
  (heartbeat: synced write + read back every minute, none for 3 min) / `monitor` (snapshot
  > 5 min old) / `collector` (`smart:*` or `probe:*` alert active); the machine's own
  alerts never make it 503. `/api/v1/status` (`api: 1`, fields only added): LAN/Tailscale
  only (remote → 404), `Authorization: Bearer ccs_…` (`auth/apitoken.rs`: 257-bit base58,
  SHA-256 stored, ≤ 20, label, last used + IP, not tied to an account), wrong tokens →
  backoff scope `api:status` + audit; JSON with verdict, alerts (titles in the alert
  language), sources, pools, datasets, md, disks, host, app, channels (no destinations: an
  ntfy topic is a secret). Tokens: section on `/.cc/admin/volumes`, POST
  `/.cc/admin/api-tokens` (add → shown once / revoke), audited; `chestcraft api-token
  add|list|revoke` (service stopped). The future `install.sh` must treat `collector` as
  "not the binary's fault" (a fresh install says it until `collect` has run).
- Slice 12 done: MCP server for agents (`src/mcp/`, `docs/mcp.md`): rmcp 3.9 (C-free) on its
  own listener `[mcp] listen` at `/mcp`, stateless JSON, 2026-07-28 (`server/discover`) and
  legacy `initialize`. Gate before rmcp: LAN/Tailscale only (CF headers → 403), `Host` in
  `[mcp] hosts`, no foreign `Origin`, `Bearer cca_…` (`auth/agent.rs`: 257-bit base58,
  SHA-256 stored, label, expiry, last used, ≤ 20 per agent), wrong tokens → backoff scope
  `mcp:token` + audit, per-agent `calls_per_minute` (governor) and `max_concurrent`. Agents
  are `User.kind = agent` (no password, never admin, no web/WebDAV/app passwords; disabled →
  tokens dead, shares paused). Tools (`mcp/tools.rs`, groups shown only when the role can
  use them): volumes/list/stat/find/read_file (text, image, base64, pieces, `preview` via
  thumbs), mkdir/write_file (NewFile)/rename/move/copy (transfer primitives)/delete
  (trash)/trash_list/trash_restore, share_create (alert `agent_share` when
  `notify_shares`)/share_list/share_revoke, health_summary/alerts/pool/disk/datasets
  (`health_roles`, built on `status::build`). `on_clash` keep_both|skip only. Actor
  `agent:<key>` in audit (`mcp_<tool>`, every call), trash and temps. CLI `chestcraft agent
  add|list|role|disable|enable|token|revoke` (service stopped). Dashboard app section +
  status API: `agent_calls_24h`, last call per agent. Verified with the Python MCP SDK 2.3
  against a local `serve`; Claude Code + Hermes against staging :3926 still to do.
- Slice 13 done: deploy (`docs/install.md`). `deploy/release.sh`: fmt, clippy `-D
  warnings`, tests, C-free static musl build (`CHESTCRAFT_VERSION` = `<pkg>+<git rev>`),
  tarball in `target/dist/` (binary, install.sh, units, example config, smartd.conf,
  SHA256SUMS). `deploy/install.sh` (root, on the server): checksums → config check as the
  service user with the new binary (failure = nothing changed) → units (old file →
  `.bak-<date>`), `--user NAME` drop-in `20-user.conf`, `10-paths.conf` from `chestcraft
  check --systemd` (ReadWritePaths state/cache/backup/writable volumes, ReadOnlyPaths for
  `read_only_paths`) → `.prev` → rename → restart if running → `/healthz` (`collector`
  counts as up) → else rollback, new one kept as `.failed`. Tested in a user+mount
  namespace with stubbed systemctl/curl (all five paths), not yet on a real host. Units:
  `chestcraft.service` (Type=exec, sandboxed: ProtectSystem=strict, DevicePolicy=closed +
  `/dev/zfs`, @system-service, MemoryDenyWriteExecute; exposure 1.7),
  `chestcraft-staging.service` (staging.toml, Conflicts=chestcraft.service, same state).
  Reload: `ExecReload` = `check` then SIGHUP; the server re-validates, drains (10 s cap,
  also on SIGTERM) and re-execs `/proc/self/exe` (same pid); invalid → logged, keeps
  running. Startup warns when a writable volume is EROFS (missing in ReadWritePaths).
  Nightly state backup (`src/backup.rs`, after the 04:00 cleanup): `Db::copy_to` (one read
  txn → new redb file, all tables; a test compares table lists) → `state.redb`, plus
  `config/` and mirrored `backgrounds/`, temp + fsync + rename, 600/700; `backup_dir` is
  never created and must be on another file system than `state_dir` (an unmounted
  dataset's folder sits on the system disk), else the backup fails and `check` warns. Outcome in `settings.backup` → dashboard app line,
  status API `app.backup`, alert `backup:failed`. `deploy/smartd.conf`: self-tests only.
  Not verified under the real sandbox yet: ffmpeg/curl/zpool with MemoryDenyWriteExecute +
  the syscall filter (staging).
- Slice 14 done: admin socket (`src/cli.rs`, `web/adminsock.rs`). `user`, `agent`,
  `api-token` and the new `backup` (now) are clap+serde enums in the lib (`AdminCmd`,
  flattened into main's CLI); `cli::run` writes into `Out` (stdout, stderr, code). The CLI
  first tries `paths.admin_socket` (default `/run/chestcraft/admin.sock` =
  RuntimeDirectory, ≤ 107 bytes): JSON `{version, cmd}` → server checks SO_PEERCRED (root
  or its own uid), same VERSION, runs it in spawn_blocking, answers `Out`; no socket /
  refused → opens the DB itself (not as root). Socket 600, stale socket replaced at bind,
  removed at shutdown; folder missing → off (logged). CLI user commands are now audited
  (`user_created`, `user_role`, …, actor `cli`); `user add` uses the no-overwrite insert.
  `import-copyparty` still needs the service stopped (reads includes relative to its file).
- Open: The account page
  still lacks app passwords and "sign out everywhere", and rename/move/copy in the UI come
  with the UI integration; the primitives exist. A thumbnail that fails to load needs a JS
  fallback to the type icon (UI integration). The dashboard/alerts, volumes/API-token and
  agent-activity markup is provisional until the design session delivers those screens;
  the agents screen (Addendum 2 "UI") needs a note in its handoff.

The refined UI brief added backend work (skins, backgrounds, per-user settings, ntfy, STL/
3MF/G-code thumbnails, universal hardware discovery): see the addendum in
`deploy/site/plan-2026-11-05.md`. Addendum 2 there adds an MCP server for agents (own port,
LAN/Tailscale only, agent accounts with a role + bearer tokens) as the last backend slice.

## Rules

- **Pure Rust:** pure-Rust crates only. The release build must pass with
  `CC=/bin/false CXX=/bin/false` (any crate that compiles C fails it). External programs
  run only as subprocesses, by absolute path: `/usr/sbin/zpool`, `/usr/sbin/zfs`,
  `/usr/sbin/smartctl`, `/usr/bin/ffmpeg`, `/usr/bin/curl` (Telegram; token on stdin, never
  in argv).
- **UI:** `design/` belongs to the design session (branch `design`, worktree
  `../chestcraft-design`). Don't edit it on `main`; integrate from it. Contract:
  `docs/ui-handoff.md` section 5 and `design/NOTES.md`.
- **Public-ready repo:** no secrets; no real hostnames, IPs, users or paths outside
  `deploy/site/` (gitignored). Tests and fixtures use fictional data; anonymize disk serials.
- **Deletion:** user files are deleted only by the trash purge (`trash_days`) and by the
  cleanup of abandoned `.part` uploads. No other code path deletes or overwrites user files;
  replacing a file moves the old one to the trash first (WebDAV PUT/MOVE are the exception).
- **Install:** the binary on the server is root-owned because the root SMART collector runs
  it. It must never be writable by the service user.
- **Commits:** one per green slice (tests, clippy, C-free build) on `main`, staged by explicit
  path. Never commit the design session's files (`design/`, its edits to `docs/`).
- **Push:** Claude may push `main` to `origin` (GitHub, public) without asking when it is a
  good moment, e.g. after a green slice commit. Before each push, check the new commits for
  secrets and lab specifics (see "Public-ready repo"). Never force-push, never rewrite
  pushed history, never push `design` (the design session's branch) (Tibor, 2026-11-07).

## Layout (planned)

- `src/`: `config/`, `db/` (redb), `appearance/` (skins, backgrounds), `auth/`, `fs/` (paths,
  trash, upload, zip, thumbs), `web/` (axum routes, maud views), `dav/`, `mcp/`, `shares/`,
  `health/` (zpool, zfs, smart, mdstat, host, history, alerts), `notify/`, `i18n.rs`
- `assets/`: CSS, JS, fonts, icons (integrated from `design/`)
- `i18n/`: `de.toml`, `en.toml` (a missing key fails the tests)
- `deploy/`: `release.sh`, `install.sh`, `units/`, `smartd.conf`, `site/` (gitignored)
- `tests/`, `fixtures/`: integration tests; zpool/zfs/smartctl JSON samples
- `config.example.toml`: config schema (draft)

## Build and test (once the toolchain is installed)

- `cargo test`
- `deploy/release.sh`: fmt, clippy, tests, C-free static musl release build
