# Installing and running chestcraft

chestcraft is one static binary (`/usr/local/bin/chestcraft`, x86_64 Linux) plus systemd
units. The examples use the default paths; `config.example.toml` explains every setting.

## Build a release

On the build machine (Rust stable with the `x86_64-unknown-linux-musl` target):

```sh
deploy/release.sh
```

It runs `cargo fmt --check`, clippy, the tests and a release build with
`CC=/bin/false CXX=/bin/false` (no dependency may compile C), checks that the binary is
static, and writes `target/dist/chestcraft-<version>.tar.gz`: the binary, `install.sh`,
the units, `config.example.toml`, `smartd.conf` and `SHA256SUMS`.

## First install

On the server, as root:

1. **Service user.** The units run as `chestcraft`:
   `useradd --system --no-create-home --shell /usr/sbin/nologin chestcraft`. To use an
   existing user that already owns the files, pass `--user NAME` to `install.sh` below.
2. **Config.** Write `/etc/chestcraft/config.toml` (root-owned, readable by the service
   user) from `config.example.toml`. Volume folders must be readable (and, unless
   `read_only`, writable) by the service user.
3. **Backup folder.** Create `paths.backup_dir` (default `/srv/chestcraft`), owned by the
   service user, mode 700, on another disk than the state (best a dataset on the pool,
   with snapshots). The backup refuses to run when it is on the same file system as
   `state_dir`, which is also what an unmounted dataset's empty folder looks like.
4. **Install:** `tar xzf chestcraft-<version>.tar.gz && ./chestcraft-<version>/install.sh`.
   It checks the config as the service user, installs the units, writes
   `chestcraft.service.d/10-paths.conf` (the folders the service may write to), creates an
   empty `/etc/chestcraft/secrets.toml` and the state and cache folders, puts the binary
   in place and enables
   `chestcraft-collect.timer` (SMART readings as root every 30 minutes).
5. **Start:** `systemctl enable --now chestcraft`, then
   `curl http://127.0.0.1:3923/healthz` (`ok`, or `collector` until the first SMART
   reading).
6. **First admin account:** `chestcraft user add NAME --role admin` (as root or the service
   user). It prints a one-time password to change at the first sign-in. Further accounts
   and agents: the admin's Accounts page (`/.cc/admin/users`) or the CLI. While the server
   runs, `user`, `agent`, `api-token` and `backup` commands go through its admin socket
   (`/run/chestcraft/admin.sock`); otherwise they open the database themselves, as the
   service user. Accounts from copyparty: `chestcraft import-copyparty` (see `--help`;
   it needs the service stopped).
7. **Disks:** install smartmontools and use `smartd.conf` from the release as
   `/etc/smartd.conf` (self-test schedule only; chestcraft sends the alerts).
8. **Alerts:** put the bot token into `secrets.toml` (`telegram_token = "…"`), open the
   bot in Telegram and press Start (the bot sees no chat before that), run `chestcraft
   telegram-setup` as root (it sends a test message), then set `alerts.telegram.chat_id`
   and restart. ntfy: `[alerts.ntfy]` in the config, an access token as `ntfy_token` if
   the server needs one.

## Updates

Build a new release and run its `install.sh` the same way. It keeps the running binary as
`chestcraft.prev`, renames the new one into place, restarts the service if it was running
and waits up to 60 s for `/healthz`. If the new version doesn't come up, the previous one
goes back and the new one stays as `chestcraft.failed`. Nothing is changed when the
release's checksums or the config check fail.

To go back by hand: `cp -p /usr/local/bin/chestcraft.prev /usr/local/bin/.cc && mv
/usr/local/bin/.cc /usr/local/bin/chestcraft && systemctl restart chestcraft`.

## Config changes

`systemctl reload chestcraft` runs `chestcraft check` first. A config that fails it is
rejected (the reload fails with the error; the running server keeps the old one). A good
one restarts the server in place: downloads in progress break off, uploads resume, nobody
is signed out. After adding a volume or changing `[paths]`, the new folders must also be
writable for the service: run `install.sh` again, or write
`chestcraft check --systemd > /etc/systemd/system/chestcraft.service.d/10-paths.conf`,
then `systemctl daemon-reload && systemctl restart chestcraft`. The server warns at start
about a writable volume it can't write to.

`secrets.toml` reaches the service through `LoadCredential=`, which systemd copies when the
service starts: after changing a token, restart instead of reload.

## Staging

`chestcraft-staging.service` runs the same binary with `/etc/chestcraft/staging.toml`
(another port, the real volumes with `read_only = true`, one writable sandbox volume) and
the same state. It conflicts with `chestcraft.service`: starting one stops the other.
`install.sh --staging` checks and installs for it; read its `10-paths.conf` afterwards,
which must list only the sandbox besides the state, cache and backup folders.

## Backup and restore

Every night at `trash.purge_at` the server copies its state to `paths.backup_dir`
(`chestcraft backup` does it now):

```text
state.redb               the database: accounts, sessions, shares, trash index, audit log,
                         alert state, history, settings (mode 600)
config/config.toml       the config file, plus config/i18n/*.toml
backgrounds/<set>/…      background pictures
```

The secrets file is not copied; thumbnails are a cache and come back by themselves. A
failed backup raises the alert "backup failed" and shows on the dashboard; the status API
reports it under `app.backup`.

To restore, with the service stopped:

```sh
systemctl stop chestcraft
cd /var/lib/chestcraft
mv state.redb state.redb.broken                 # keep the old file
cp -p /srv/chestcraft/state.redb state.redb      # or from /srv/chestcraft/.zfs/snapshot/<name>/
cp -rpn /srv/chestcraft/backgrounds .
chown -R chestcraft: state.redb backgrounds
systemctl start chestcraft
```

Items moved to the trash after that backup are missing from the restored trash list; they
stay in the volumes' `.chestcraft-trash` folders, and the purge doesn't touch them.
