# chestcraft

Rust file-sharing app for a home NAS: browse, upload, share links, WebDAV, and an admin
dashboard with ZFS/SMART health and Telegram alerts. Replaces copyparty.

Project map, build/run, rules and common changes (for every agent):

@AGENTS.md

@deploy/site/CLAUDE.site.md

## Status

Plan steps 1–3 done (step 3 = the backend slices below) plus the deploy tooling (slice 13).
Backend brief 2026-11-07 (`deploy/site/backend-brief-2026-11-07.md`): slices A, B and C done
(slices 17–19). UI integration (step 7) in slices on `main`: slices 20–22 (D, E, F) done:
everything the design has delivered so far (browser, gate pages, skins) is live; the other
screens wait for the design session's mocks (`design/pages/`). NAS setup (step 4) and the
account import (6) ran on 2026-11-07; staging (8) runs on the NAS since then. Telegram (5)
is set up there since 2026-11-09 (test message sent; the token reaches the service only at
start, so a restart, not a reload). Next: Tibor's own staging checks (web UI, WebDAV/rclone,
public write, shares from the UI), and the next UI slices as the design delivers screens.
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
  ATA hand-written (no SATA disk had smartctl yet). On the NAS since 2026-11-07: `collect`
  readings match `smartctl -A`.
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
  against a local `serve`, and every tool with a plain JSON-RPC client against staging
  :3926 (2026-11-08); Claude Code + Hermes still to do.
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
  Verified on staging (2026-11-08): ffmpeg, zpool and zfs under MemoryDenyWriteExecute +
  the syscall filter, reload (re-exec), the nightly backup. curl (channels) not yet: no
  channel configured.
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
- Slice 15 done: access management on the web (`web/users.rs`, `auth/accounts.rs`).
  `accounts::{create_person, reset_password, set_role, set_disabled}` shared by the CLI and
  `/.cc/admin/users` (people: add → one-time password shown once, new one-time password,
  role, block/unblock, never on oneself; agents: add, role (never admin), block, tokens with
  30/90/365 days or never → shown once, revoke). `/.cc/admin/audit` (newest 200 of 5000,
  filter by actor/action substring). Account page: WebDAV app passwords (create → shown
  once, revoke) and "sign out on the other devices" (`Db::sessions_of`); account page now
  `no-store`. Secret pages `no-store`, never in URLs; everything audited (actor
  `user:<key>`). Header links "Accounts" for admins. Markup provisional.
- Slice 16 done: JSON API for rename/move/copy (`web/ops.rs`), one item per call like
  uploads: `POST /.cc/rename {vol, path, name}`, `/.cc/move` and `/.cc/copy {vol, path,
  to_vol, to_dir}`, `clash` = ask (409 + the upload `clash` object) | keep_both | replace
  (in the way → trash, needs `d` there) | skip. Rights like MCP (rename/move `m` on the
  item, `m` at the target in the same volume, `w` in another; copy `r` + `w`); signed-in
  only, `X-CSRF-Token`; audited (`rename`/`move`/`copy`, target → detail); shares follow.
  The UI dialogs and the clipboard (`yy`/`dd`/`p`) still need the JS (UI integration).
- Slice 17 done (backend brief 2026-11-07, slice A; design requests 1, 1a, 9, 11, 12, 13):
  appearance. Per skin `Panels {glass: frosted|clear, opacity: 1–5}` in
  `UserSettings.panels`, `Appearance.panels` + `share_panels`; each value falls back on its
  own (account → admin default for the skin → frosted / the skin's own); `<html
  data-glass data-opacity>` (opacity only when set). `Background.dim` is `Option` (None =
  auto: frosted 1, clear 2 for colors, pictures and skin pictures; 0 for the skin color).
  Four gradient presets (`appearance::PRESETS`, per-skin order) as swatch buttons in their
  own form (keep dim/blur/panels), stored as a color; swatch colors from
  `/.cc/presets.css`. Built-in sets `builtin:<name>` (`mountains`, `sakura`, `garden`) from
  `assets/backgrounds/<name>-<slot>.jpg` + `.small.jpg` previews (copied from the `design`
  branch), served as assets, never in the store's records (remove/sweep can't touch them),
  usable by everyone, mixable, not deletable. Skin pictures: `AppState.skin_pictures` from
  embedded `backgrounds/default-<skin>-{dark,light}.svg` (sleek, gaming, anime from the design's
  round 5; they show once `skins/<name>.css` is embedded); explicit theme → that file, system → `<source media="(prefers-color-
  scheme: light)">` + dark `<img>`, no light file → dark. `pick_skin` falls back to
  `modern` before the first skin. i18n `[skin]` names, `[presets]`.
- Slice 18 done (brief slice B; requests 2, 3): the volume note is gone (config schema, so
  an old `note.*` fails `check` through `deny_unknown_fields`; landing, listing banner, MCP
  `volumes`). Visitor password pages (share password, public-upload unlock) render through
  `Ctx::gate()`: no brand, no language switch; language from the cookie, else
  `Accept-Language`. The sign-in page keeps the brand and now has the language switch
  (`/.cc/lang` serves it, public volumes and share contents).
- Slice 19 done (brief slice C; requests 7, 8): `GET <folder>/?tree` and `POST <folder>/`
  `op=tree` + `n` → streamed JSON lines `{path, dir, size, mtime}` (`fs/tree.rs`, same walk
  rules as the zip through `zipstream::listed_names`; same rights, `hidden=1` for admins,
  selection check and anonymous download slot as `?zip`); shares `/s/<id>/…?tree` / `op=tree`.
  `GET /.cc/dl-slots` → `{"max":N,"free":n}` for anonymous visitors (per-IP limiter),
  `{"max":null}` signed in; `/s/<id>/.cc/dl-slots` after the password (always limited).
  Documented in the `web/files.rs` module docs, like uploads (`web/api.rs`) and ops.
- Slice 20 done (UI slice D; design requests 4, 5): the design's `tokens.css`, `app.css`,
  `skins/*.css`, `icons.svg` and `fonts/` ship unchanged in `assets/`, copied from the `design`
  branch by `deploy/design-sync.sh` (rerun it when the design changes; it also lists files
  the branch dropped). All five skins are live (`modern` by default). App shell
  (`views::app`): app bar (brand, crumbs, admin link, account button + popover menu,
  sign-in link and language switch for visitors), sidebar (`web/nav.rs`: readable volumes,
  the current one opened along the path, tags public/home only, free space of the current
  volume, links with the own active share count) on landing, folders, viewer, trash, my
  shares and account; phone tab bar for accounts. Folder page = `proto/browser.html`: title
  row (summary "N items, size", filter, phone download, list/grid), action bar (Upload →
  file picker, New folder → dialog, Download ▾ with the zip as a GET form so it works
  without JS; save/each rendered hidden until slice E), `table.files` (folder item counts
  via `VolumeRoot::count`, date-only phone meta line, `<time>`, share badge with expiry
  tooltip, `aria-sort`, caption), status bar idle/select, hidden `form#sel` (zip, delete).
  Gate pages (`views::gate`): sign-in (brand + language switch), password change, share
  password, public-upload unlock; fields with reveal button. `assets/app.js` (all pages):
  popover menus placed next to their button, arrow keys, reveal, dialog cancel.
  `assets/browser.js`: selection (checkbox, row click, shift range, long-press on phones,
  select all), status count/size, filter (no case/accents), row menu, delete confirm, new
  folder, selection zip. Undesigned screens render in `main.panel` with
  `assets/provisional.css`. Referrer-Policy is now `same-origin`: with `no-referrer`
  browsers send `Origin: null` on form POSTs and every form failed the origin check.
  Checked in headless Firefox (WebDriver BiDi) at 1280 and phone width, dark and light.
- Slice 21 done (UI slice E; design requests 6, 10): `assets/download.js` runs the download
  menu's script-only ways in every listing (volumes, public volumes, share folders). "Save
  to folder" (only where `showDirectoryPicker` exists): `?tree` / `op=tree`, folders
  created, files fetched with `?dl` and piped into the picked folder (whole folder → a
  subfolder named after it; selection → straight in), never overwrites (same size =
  already there, else `name (2).ext`), summary saved / already there / renamed / failed.
  "Separately": files `?dl`, folders `?zip` (whole folder: its top level from `?tree`),
  more than 20 ask first (`dl-many-dialog`). Both at most 4 at a time; "separately" asks
  `dl-slots` before each start. Accounts' downloads are now counted per IP
  (`AppState.account_downloads`, no limit): `/.cc/dl-slots` signed in answers
  `{"max":null,"active":n}`. Queue toast `.queue` (count, bar, stop/close). Checked in
  headless Firefox (real downloads; save with a mocked picker, Firefox has none).
- Slice 22 done (UI slice F): keyboard maps, clipboard, rename/move/copy in the browser.
  `?dirs` JSON on folders (`{"dirs","write","move"}`, no dot-names, ≤ 5000) and `/?dirs`
  (readable volumes) for the folder picker. `assets/ops.js` (accounts only: the browser
  element carries `data-vol` + `data-csrf`): in-app clipboard in localStorage
  (`cc-clipboard`, survives folder changes, syncs across tabs) with the action bar's
  `.actionbar__paste` chip (count, paste, clear); paste/move/copy one item per call over
  `web/ops.rs`, 409 → `op-clash` dialog (keep both / replace / skip, apply to all), progress
  in the queue toast, result shown after the reload (sessionStorage flash); cut items
  already in the target stay. Rename dialog (stem preselected, taken name → field error);
  folder picker dialog (path crumbs from all volumes, enter folders, "Move/Copy here"
  enabled by the target's rights, a folder never into itself, move into the same folder
  off). Status bar: Move, Copy and Rename (`status__overflow`, Rename only for one item),
  More menu (`#sel-more`) for phone/tablet; row menu gets Rename/Move/Copy (`cc:row-menu`
  event). `assets/keys.js`: Vim (j/k, gg/G, l/Enter, h/Backspace, /, n/N, Esc, Space, V,
  yy/dd/p, D, r, o, s, u, z, ?) and Standard (arrows, Home/End, Enter, Alt+↑, Ctrl+A,
  Shift+↑/↓, Space, Ctrl+C/X/V, Del, F2, /, Esc, ?) maps, Off = nothing; never in text
  fields, dialogs or menus; cursor = `data-state~="cursor"` with focus on its link; filter
  Enter → first match; viewer h/l/q and ←/→/Esc (`data-js="viewer-*"`). Shortcut overview
  dialog (`keys_help`, key names localized: Strg, Entf, …). Checked in headless Firefox
  with real key input (both maps, paste/clash/rename/picker flows).
- Staging check 1 (2026-11-08, step 8): status API numbers match `zpool`, `zfs list` and
  `smartctl`; MCP on the Sandbox (scope, writes, keep-both, cross-filesystem copy, trash +
  restore, previews of PNG/MP4/SVG, share → visitor unlock, download, zip, `?tree`, revoke →
  410); anonymous public volume (thumbnails, `dl-slots`, writes refused). Fix: dataset quotas
  count `refquota` too (`Dataset::quota_use`: `used`/`quota` and `referenced`/`refquota`,
  the fuller one wins; dashboard, alert `ds:<name>:quota`, status API adds `refquota` and
  `referenced`).
- Thumbnail fallback (2026-11-09): a thumbnail that fails to load (404, or 429 for an
  anonymous visitor past the render burst) gives way to a server-rendered `<template
  data-js="thumb-fallback">` in its `data-js="thumb-box"`: the type icon in listings, the
  "no preview" stage in the viewer (`data-state="none"`). `assets/app.js` swaps it in and
  asks for the picture again after 5/15/45 s (jittered); on success the picture comes back.
  Checked in headless Firefox (burst used up → icons, back after the retry; broken JPEG
  stays an icon; viewer).
- Open: For the design session (not in its NOTES yet): tablet nav (the `appbar__menu`
  drawer; 720–1023 px has no nav besides crumbs and the account menu), the account menu, a
  folder-upload entry (only drag and drop now), an `i-arrow-down` icon for descending sort,
  `a.menu__item` (links in menus), dialogs, upload tray, banners, pager, grid view CSS,
  share of several items (Share shows for exactly one), the rename/picker/clash/shortcut
  dialogs; at 1280 px the selection bar wraps to two rows while Rename shows (one item
  selected); "keep both" on the server names `x (1).ext`, save-to-folder (design)
  `x (2).ext`. The dashboard/alerts, volumes/API-token, accounts/agents, audit and
  account-access markup is provisional until the design session delivers those screens; the
  agents part (Addendum 2 "UI") needs a note in its handoff.

The refined UI brief added backend work (skins, backgrounds, per-user settings, ntfy, STL/
3MF/G-code thumbnails, universal hardware discovery): see the addendum in
`deploy/site/plan-2026-11-05.md`. Addendum 2 there adds an MCP server for agents (own port,
LAN/Tailscale only, agent accounts with a role + bearer tokens) as the last backend slice.

## Rules

The code rules (pure Rust, deletion, paths, i18n, public repo, install) are in `AGENTS.md`.
On top of those, for this repo's own sessions:

- **UI:** `design/` belongs to the design session (branch `design`, worktree
  `../chestcraft-design`). Don't edit it on `main`; integrate from it. Contract:
  `docs/ui-handoff.md` section 5 and `design/NOTES.md`.
- **Commits:** one per green slice (tests, clippy, C-free build) on `main`, staged by explicit
  path. Never commit the design session's files (`design/`, its edits to `docs/`).
- **Push:** Claude may push `main` to `origin` (GitHub, public) without asking when it is a
  good moment, e.g. after a green slice commit. Before each push, check the new commits for
  secrets and lab specifics (see "Public repo" in `AGENTS.md`). Never force-push, never rewrite
  pushed history, never push `design` (the design session's branch) (Tibor, 2026-11-07).
