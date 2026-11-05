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
- Next: slice 3, auth/sessions/backoff (redb).

The refined UI brief added backend work (skins, backgrounds, per-user settings, ntfy, STL/
3MF/G-code thumbnails, universal hardware discovery): see the addendum in
`deploy/site/plan-2026-11-05.md`.

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
  `web/` (axum routes, maud views), `dav/`, `shares/`, `health/` (zpool, zfs, smart, mdstat,
  host, history, alerts), `notify/`, `i18n.rs`
- `assets/`: CSS, JS, fonts, icons (integrated from `design/`)
- `i18n/`: `de.toml`, `en.toml` (a missing key fails the tests)
- `deploy/`: `release.sh`, `install.sh`, `units/`, `site/` (gitignored)
- `tests/`, `fixtures/`: integration tests; zpool/zfs/smartctl JSON samples
- `config.example.toml`: config schema (draft)

## Build and test (once the toolchain is installed)

- `cargo test`
- `deploy/release.sh`: fmt, clippy, tests, C-free static musl release build
