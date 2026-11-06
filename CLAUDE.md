# chestcraft

Rust file-sharing app for a home NAS: browse, upload, share links, WebDAV, and an admin
dashboard with ZFS/SMART health and Telegram alerts. Replaces copyparty.

@deploy/site/CLAUDE.site.md

## Status

Plan steps 1–2 done. Step 3 (backend slices):
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
- Next: slice 9, user preferences + appearance (skins, backgrounds, rotation). The account
  page (app passwords in the UI) and rename/move/copy in the UI come with the UI
  integration; the primitives exist. A thumbnail that fails to load needs a JS fallback to
  the type icon (UI integration). The dashboard/alerts markup is provisional until the
  design session delivers those screens.

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
  path. Never commit the design session's files (`design/`, its edits to `docs/`), never push
  (Tibor, 2026-11-05).

## Layout (planned)

- `src/`: `config/`, `db/` (redb), `auth/`, `fs/` (paths, trash, upload, zip, thumbs),
  `web/` (axum routes, maud views), `dav/`, `mcp/`, `shares/`, `health/` (zpool, zfs, smart, mdstat,
  host, history, alerts), `notify/`, `i18n.rs`
- `assets/`: CSS, JS, fonts, icons (integrated from `design/`)
- `i18n/`: `de.toml`, `en.toml` (a missing key fails the tests)
- `deploy/`: `release.sh`, `install.sh`, `units/`, `site/` (gitignored)
- `tests/`, `fixtures/`: integration tests; zpool/zfs/smartctl JSON samples
- `config.example.toml`: config schema (draft)

## Build and test (once the toolchain is installed)

- `cargo test`
- `deploy/release.sh`: fmt, clippy, tests, C-free static musl release build
