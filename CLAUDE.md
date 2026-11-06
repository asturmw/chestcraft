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
- Next: slice 7, thumbnails/previews (incl. STL/3MF/G-code/SVG). The account page (app
  passwords in the UI) and rename/move/copy in the UI come with the UI integration; the
  primitives exist.

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
