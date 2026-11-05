# chestcraft — UI design handoff

You are the design session for chestcraft's web UI. This file is the whole brief. Another
session builds the backend in parallel: it reproduces your markup in server templates and
ships your CSS, icons and fonts unchanged. Read everything before proposing anything.

## 0. How to work

1. Work in your own git worktree on branch `design` and write only under `design/`:
   `git -C ~/Homelab/chestcraft worktree add -b design ~/Homelab/chestcraft-design`
   (reuse it if it exists). The backend session never edits `design/`; you never edit
   anything outside it.
2. Load the `frontend-design` skill before any design work, and the `dataviz` skill before
   designing charts, stat blocks or meters for the admin dashboard.
3. The brief (section 2) is confirmed by Tibor, the owner. The visual direction is not.
   Propose 2–3 directions grounded in the brief: palette as named hex values, typefaces and
   their roles, a layout concept with ASCII wireframes of the folder browser and the share
   page, and the one memorable element. Tibor picks before you build screens.
4. Build order: tokens + core components → key screens (folder browser, share password +
   share view, dashboard, landing) → review with Tibor → remaining screens → copy in German
   and English → self-critique pass → `design/NOTES.md`.
5. Look at your pages in a real browser (Claude-in-Chrome) at the widths in section 4, in
   both themes, and fix what you see.

## 1. What chestcraft is

A self-hosted file manager and sharing app for a home NAS (one ZFS pool on 4 hard disks,
plus a mirrored pair of NVMe boot drives), written in Rust. It replaces copyparty. One
binary serves:

- browsing, previewing, uploading and organizing files in a few volumes (top-level folders
  such as Dokumente, Medien, Projekte);
- password-protected share links for folders and single files;
- optional public volumes that anyone can read without an account;
- WebDAV for a desktop mount (no UI involved);
- an admin dashboard with storage health (pool, disks, SMART) and alerts, which also go to
  Telegram.

Pages are rendered on the server (Rust, maud templates). Navigation is full page loads;
small vanilla JS adds uploads, multi-select, dialogs and live progress. No SPA, no npm, no
CDN, no external requests of any kind: fonts and icons are embedded in the binary.

## 2. Brief (confirmed)

- **Subject:** chestcraft is the family NAS as a chest: about 5 TB of documents, photos,
  videos, anime, projects and backups. The owner is a maker (3D printing and laser cutting,
  also as a small business) and a gamer. The name is chest + craft.
- **Audiences:**
  1. The owner (admin). Power user. Desktop with three monitors, plus a Galaxy Z Fold:
     narrow cover screen when folded, near-square inner screen when unfolded. Uses it daily,
     at home and remotely.
  2. Family and friends with accounts. Rare visits, browse and download, little patience.
  3. Share recipients. Strangers, including business customers. One visit, mostly on a
     phone. They must get from "password" to "download" in about 5 seconds, and the page has
     to look trustworthy. The share pages are what outsiders see of chestcraft.
- **Primary jobs:** find and move files fast; share a folder in 3 clicks; see at a glance
  whether the NAS is healthy (the dashboard verdict is the owner's most important glance).
- Light and dark theme. The default follows the system; the user can override it.
- German and English UI.
- **Hard no:** anything taken from Minecraft: textures, fonts, sprites, block art, GUI look.
  The name evokes it; the design must be original.

## 3. Technical constraints

- Plain HTML and CSS. Modern CSS is fine (custom properties, nesting, `:has()`, container
  queries, `color-mix()`). Targets: current Chrome/Edge, Firefox, Safari (iOS 17+), Samsung
  Internet.
- **Fonts:** at most two families, self-hosted WOFF2, under a license that allows embedding
  and redistribution (SIL OFL or similar), with the license file next to the fonts. Subset to
  Latin + Latin-1 + German punctuation („ “ ‚ ‘ – —). Keep total font weight under ~300 KB.
- **Icons:** one inline SVG sprite `design/icons.svg` with `<symbol id="i-…">`; your own
  drawings or an MIT/ISC-licensed set (say which). File-type icons needed: folder, image,
  video, audio, PDF, text/Markdown, office document, spreadsheet, archive, code, 3D model
  (STL/STEP/3MF), disk image, generic file. Plus the UI icons your screens need.
- **Theme tokens:** define them on `:root` and also on `[data-theme="light"]` and
  `[data-theme="dark"]`, so a theme can be applied to any subtree (`components.html` shows
  both side by side). Without an override, `@media (prefers-color-scheme: dark)` applies the
  dark tokens unless `data-theme="light"` is set. The server writes `data-theme` on `<html>`
  from a cookie, so there is no flash.
- **Accessibility floor:** WCAG 2.2 AA contrast in both themes; visible keyboard focus; full
  keyboard operation; semantic elements (`<nav>`, `<main>`, `<table>` for tabular lists,
  `<dialog>` for dialogs, real `<form>`s); `prefers-reduced-motion` respected; touch targets
  ≥ 44 px on touch layouts; live regions for upload progress and toasts.
- **Without JavaScript** these must still work: landing, login, folder browsing (list view
  and paging), file download, folder zip download, share password page, share browsing and
  download. Everything else may require JS.
- **Weight:** `tokens.css` + `app.css` ideally under ~80 KB unminified; no raster images for
  decoration.
- All UI text comes from the key files (section 7). Only fictional sample data is written
  into the mock markup.

## 4. Widths

| Context | CSS width | Notes |
|---|---|---|
| Fold cover screen, typical phones | ~360 px | primary for share recipients |
| Fold unfolded, small tablets | ~800 px, near-square | check the aspect ratio, not only width |
| Desktop | 1280–2560 px | owner's main use: long file lists, dashboard |

Design mobile first.

## 5. Contract with the backend

The backend reproduces your markup in maud templates and ships `tokens.css`, `app.css`,
`icons.svg` and `fonts/` as they are.

- **Classes:** BEM style. Block `browser`, element `browser__row`, modifier `browser--grid`.
  One block per component.
- **States:** `data-state="…"` on the block, e.g. `<li class="upload-item"
  data-state="paused">`. No state classes. List every state value in `NOTES.md`.
- **JS hooks:** `data-js="…"` attributes. Never style `[data-js]`; JS never relies on classes.
- **Asset paths:** `tokens.css`, `app.css` and `icons.svg` sit side by side with `fonts/` next
  to them; reference fonts with relative URLs (`url(fonts/x.woff2)`). The server serves the
  same layout under `/.cc/assets/`.
- **Dialogs:** native `<dialog>`. Multi-step dialogs switch steps with `data-state`.
- **Forms:** include `<input type="hidden" name="csrf" value="…">`. Field errors use
  `aria-describedby` and `data-state="error"` on the field's block.
- **Thumbnails:** state the sizes your grid and viewer need. Default assumption: 256 px and
  640 px on the longest edge, aspect ratio kept.
- **Charts:** generated on the server as inline SVG. Deliver a styled example of every chart
  type with the classes the backend should emit (axes, gridlines, series, thresholds,
  projection, labels). Colors come from tokens.
- **Prototype JS** (state switchers, fake progress) is allowed for demos, lives in
  `design/proto/`, and is not shipped.
- **Formats:** the backend formats dates, sizes and numbers per language; show them in the
  mocks. Examples: German `05.11.2026, 18:30` and `2,4 TB`; English `5 Nov 2026, 18:30` and
  `2.4 TB`.

URLs, for context only:

- `/` landing; `/<Volume>/<path>` folder or file (a file GET downloads it; `?view` opens
  the viewer; `?zip` downloads a folder as zip)
- `/s/<id>` share links
- `/.cc/login`, `/.cc/account`, `/.cc/shares`, `/.cc/trash`, `/.cc/admin/…` app pages
- `/.cc/assets/…` static files

## 6. Screens

For each screen: who sees it, the data, the actions, the states. "Owner" means the admin.
Mock file names: `design/pages/<screen-id>.html`.

### Public (no account)

- **`public-landing`** — anonymous `/`. Public volumes (name, note, size), sign-in link.
  States: some public volumes; none (only sign-in).
- **`login`** — username and password. States: default, wrong credentials, slowed down after
  failures ("try again in 2 minutes"), signed out.
- **`password-change`** — forced after the first sign-in of migrated accounts; also reached
  from the account page. New password twice (≥ 10 characters); current password when it's a
  voluntary change. States: default, too short, mismatch, saved.
- **`share-password`** — `/s/<id>` before unlock. **Shows nothing about the content:** no
  name, no owner, no note. Only that this is a protected share, and the password field.
  States: default, wrong password, slowed down / locked (with time).
- **`share-view`** — unlocked share. Folder share: name, who shared it, note, "available
  until …", breadcrumb inside the share, list/grid, preview, file download, "download all"
  (zip). Single-file share: one large file card with preview and download. Mode
  "view + upload" adds an upload area. States: folder, single file, empty folder, upload in
  progress, upload done, paging (more than 2000 items).
- **`share-dropbox`** — mode "upload only": note, drop zone / file picker, list of what this
  visitor uploaded in this session, size limit if set. States: idle, uploading, done, too
  large, renamed because the name existed ("saved as Angebot (1).pdf").
- **`share-unavailable`** — variants: expired, revoked, paused, target missing, not found,
  locked after too many attempts.
- **`public-volume`** — anonymous browsing of a public volume: the folder browser without
  account actions (download, zip, preview). If the owner opened uploads: an "Upload" button
  that asks for the upload password (`public-write-prompt`, a dialog), then the upload tray.
  States: uploads closed, uploads open, wrong password, locked.

### Signed-in users

- **`landing`** — volumes the user can access: name, note, free space (bar); links to My
  shares and Trash. The owner also sees the health verdict (OK / needs attention /
  critical / monitoring stale) linking to the dashboard. States: normal; no volumes ("ask the
  owner for access").
- **`browser`** — the core screen. Breadcrumb; volume note as a banner (e.g. "Files here are
  sorted automatically every 15 minutes"); free space; toolbar: upload, new folder,
  list/grid, sort, filter field. List view: checkbox, icon or thumbnail, name, size,
  modified. Grid view: thumbnails. Selection bar when items are selected: download as zip,
  move, copy, rename (single item), delete, share. Per-row menu. Drag-and-drop overlay.
  Paging above 2000 items. States: list, grid, selection, dragging over, empty folder (with an
  upload hint when allowed), read-only (no upload rights, or a read-only area), very long
  names, 2000+ items.
- **`upload-tray`** — panel or bottom sheet. Per file: name, size, progress, speed, time
  left, state (queued, uploading, paused, done, failed, waiting for a name-clash decision).
  Overall progress, pause/resume all, clear finished, minimized chip with percent. After a
  page reload the tray lists unfinished uploads and asks to add the same files again to
  continue.
- **`name-clash`** — dialog: "‘Angebot.pdf’ already exists here", with both files' size and
  date. Keep both (default; saves as "Angebot (1).pdf") / Replace (the existing file goes to
  the trash) / Skip. "Apply to the remaining N".
- **`viewer`** — full-screen preview: image (zoom, previous/next, swipe), video and audio
  (native controls), text and Markdown (rendered), PDF (browser viewer), unsupported file or
  codec (icon + download). Info panel: size, modified, location, type. Actions: download,
  share, rename, delete, close.
- **`dialogs`** — new folder; rename; move/copy (folder picker across the volumes the user may
  write to); delete ("Moves 3 items to the trash. They are deleted for good after 30 days.").
- **`share-create`** — step 1: target (name, location); mode (view / view + upload / upload
  only — only the modes the user's rights allow); expiry (1 day, 7 days, 30 days = default,
  never); password (generated passphrase like `tanne-kiesel-ofen-raupe` with a regenerate
  button, or their own, ≥ 8 characters); note for the recipient; "Notify me on Telegram when
  it's opened or downloaded"; size limit for upload modes. Step 2, shown once: link,
  password, copy buttons, "Copy as message" (ready-to-send text with link, password and
  expiry), and a clear note that the password can't be shown again.
- **`my-shares`** — list: target, mode, created, expires, status (active, expired, revoked,
  paused, target missing), last opened, opens/downloads. Actions: open, edit (expiry, mode,
  note, notify), new password (shown once), revoke. Detail: access log (time, IP, country,
  action, file). Empty state.
- **`trash`** — items the user deleted (the owner sees everyone's), per volume: name,
  original location, deleted at, deleted by (owner view), size, "deleted for good in N days".
  Restore (if the name is taken again it restores as "… (restored)"). Owner: "Empty trash now"
  per volume, with confirmation. Empty state.
- **`account`** — username and role (read-only); change password; language; theme; WebDAV
  app passwords (label, created, last used; create → shown once; revoke); sign out
  everywhere.

### Owner (admin)

- **`dashboard`** — the verdict and any active alerts at the top, then:
  - Pool: name, state, used/free with capacity bar, fragmentation, vdev tree (4 disks in
    raidz1) with read/write/checksum error counts, last scrub (date, result, duration),
    progress of a running scrub, resilver or expansion.
  - Datasets (~14): name, volume, used, available, quota with %, snapshots (count, space),
    newest snapshot age, trash size.
  - Disks (4 HDD + 2 NVMe): bay label, model, short serial, role (pool member / boot
    mirror), SMART verdict, temperature with a 90-day sparkline, power-on hours,
    reallocated / pending / uncorrectable sectors, CRC errors, NVMe wear % and media errors,
    last self-test (type, date, result).
  - Boot mirror: state, members; root filesystem use.
  - Host: uptime, load, RAM, ZFS ARC size and hit rate, CPU temperature.
  - App: active sessions, running transfers, active shares, failed sign-ins (24 h).
  - Charts over 90 days: pool fill with projection ("full in about 14 months"), disk
    temperatures, reallocated-sector trend.
  - States: all OK; warning (a disk at 52 °C, pool 82 % full); critical (a disk failing SMART,
    pool degraded and resilvering at 37 %); monitoring stale ("disk data is 3 hours old");
    fresh install (no history yet).
- **`alerts`** — active alerts (severity, title, since, details), history (fired/resolved),
  Telegram status, "Send test message".
- **`users`** — list: name, role, last sign-in, status (active, disabled, must change
  password). Create (name, role, generated initial password shown once), change role, reset
  password (shown once), disable/enable, end sessions.
- **`all-shares`** — `my-shares` for everyone, with an owner column and filters.
- **`audit`** — table: time, who (user, anonymous IP, share visitor), action, target,
  IP/country. Filters: person, action type, volume, date range. Paging.
- **`volumes`** — read-only view of the configuration: volumes with path and flags (public,
  reachable from the internet, read-only areas, trash days, note); roles × volumes permission
  matrix; users per role. Per public volume: public upload switch with password (set/rotate,
  shown once), auto-off (1 day, 7 days, 30 days, until switched off), size limit, notify. API
  tokens: label, created, last used; create (shown once); revoke.

### Global

- App shell: header with brand, location (breadcrumb), upload status, account menu
  (language, theme, sign out), link to admin for the owner; navigation on phones.
- Toasts (success, error, info) and inline banners (volume note, read-only, warning,
  critical).
- Error pages: 403, 404, 429 ("too many requests, try again in …"), 500.
- Empty and loading patterns.

## 7. Copy and i18n

- Files `design/i18n/de.toml` and `design/i18n/en.toml` with identical key sets, grouped by
  screen (`[browser]`, `[share_create]`, …). Placeholders in braces: `{name}`, `{count}`,
  `{date}`, `{size}`. Plurals as `key_one` / `key_other`.
- Follow the writing guidance in the frontend-design skill: plain words, active voice,
  sentence case; errors say what happened and how to fix it; a button says what it does and
  keeps that name through the flow.
- Settle with Tibor: German "du" or "Sie"? One option is "du" in the app and "Sie" on the
  share pages, which business customers see.
- Include the "Copy as message" template for shares in both languages.

## 8. Sample data

Fictional only (the repo will be public): no real names, paths, hostnames or files from the
owner's NAS. Use a believable family + maker set: users `admin`, `jana`, `mika`; volumes
Dokumente, Fotos, Filme, Projekte, Archiv, Austausch (public), Eingang (auto-sorted),
Backups; files like `Rechnung 2026-014.pdf`, `Halterung_v3.step`,
`Urlaub Gardasee/IMG_2041.jpg`, `Bauplan Werkbank.pdf`; domain `files.example.org`. Use
realistic sizes and dates, long names, umlauts and ß.

## 9. Deliverables (all under `design/`)

```
design/
  tokens.css  app.css  icons.svg  fonts/      WOFF2 files + license
  pages/<screen-id>.html   one per screen in section 6; state variants as extra files
                           or via a proto switcher
  components.html          every component × every state × both themes, side by side
  i18n/de.toml  i18n/en.toml
  proto/                   demo-only JS, not shipped
  NOTES.md                 chosen direction, tokens, type and icon choices with licenses,
                           and the contract index: every block with its elements,
                           modifiers, data-state values and data-js hooks
```

## 10. Done when

- Tibor approved the direction and the final pages.
- Every screen in section 6 exists with its states, in both themes, at ~360, ~800 and
  ≥ 1280 px.
- No external requests (check the network panel), no console errors.
- AA contrast in both themes; keyboard-only use works; focus is visible; reduced motion is
  respected.
- The no-JS list in section 3 works with JavaScript disabled.
- `de.toml` and `en.toml` have the same keys.
- Fonts and icons are licensed for embedding; nothing is derived from Minecraft.
- `NOTES.md` contains the full contract index.

Not your job: behavior JS, backend routes, data loading. If a screen needs data or an
action that isn't listed here, add it to `NOTES.md` under "Requests for the backend".
