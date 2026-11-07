#!/bin/bash
# Install or update chestcraft on this machine. Run it as root from the unpacked release
# folder (deploy/release.sh builds it):
#
#   sudo ./install.sh [--staging] [--user NAME] [--no-restart]
#
#   --staging     chestcraft-staging.service with /etc/chestcraft/staging.toml instead of
#                 chestcraft.service with /etc/chestcraft/config.toml
#   --user NAME   run the services as NAME (writes <service>.service.d/20-user.conf);
#                 without it they run as the user in the unit (chestcraft) or an earlier
#                 drop-in
#   --no-restart  install only; restart yourself
#
# Steps; a failure in steps 1 and 2 changes nothing:
#   1. verify the release (SHA256SUMS, --version)
#   2. check the config as the service user with the new binary
#   3. install units that changed (an old file is kept as <file>.bak-<date>), the user
#      drop-in and <service>.service.d/10-paths.conf (`chestcraft check --systemd`)
#   4. keep the current binary as /usr/local/bin/chestcraft.prev, rename the new one into
#      place (root-owned, mode 755: the root SMART collector runs it, so the service user
#      must never be able to write it)
#   5. restart the service if it runs and wait for /healthz; if it doesn't come up, the
#      previous binary goes back (the new one stays as chestcraft.failed)
#
# It creates no user and no config and never touches the volumes; it creates the state and
# cache folders and an empty secrets file if missing, and the only files it replaces are its
# own: units, drop-ins and the older binaries (.prev, .failed).
# A first install needs the service user and the config file first (config.example.toml).
set -euo pipefail

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
    exit 2
}
die() {
    echo "error: $*" >&2
    exit 1
}

here=$(cd "$(dirname "$0")" && pwd)
service=chestcraft
config=/etc/chestcraft/config.toml
user=
restart=1
while (($#)); do
    case "$1" in
        --staging) service=chestcraft-staging config=/etc/chestcraft/staging.toml ;;
        --user) user=${2:-}; [[ -n $user ]] || usage; shift ;;
        --no-restart) restart=0 ;;
        *) usage ;;
    esac
    shift
done

[[ $EUID == 0 ]] || die "run this as root"
bindir=/usr/local/bin
live=$bindir/chestcraft
new=$bindir/.chestcraft.new
unitdir=/etc/systemd/system
today=$(date +%F)
reload_units=0

# Write $2 to $1 unless it already says that (returns 1 then); an old, different file is
# kept as $1.bak-<date> (the first one of the day). Called in `put … && x=1`, where bash
# ignores `set -e`, so every step checks for itself.
put() {
    local file=$1 content=$2
    if [[ -f $file ]] && [[ "$(cat -- "$file")" == "$content" ]]; then
        return 1
    fi
    mkdir -p -- "$(dirname -- "$file")" || die "can't create $(dirname -- "$file")"
    if [[ -e $file && ! -e $file.bak-$today ]]; then
        cp -p -- "$file" "$file.bak-$today" || die "can't back up $file"
    fi
    printf '%s\n' "$content" >"$file.new" || die "can't write $file.new"
    chmod 644 -- "$file.new" || die "can't chmod $file.new"
    mv -f -- "$file.new" "$file" || die "can't rename $file.new"
    echo "wrote $file"
}

# --- 1. release -------------------------------------------------------------------------
(cd "$here" && sha256sum --quiet --strict -c SHA256SUMS) || die "SHA256SUMS don't match"
version=$("$here/chestcraft" --version) || die "the new binary doesn't run here"
echo "new: $version"
if [[ -x $live ]]; then
    echo "now: $("$live" --version 2>/dev/null || echo unknown)"
fi

# --- 2. config, checked as the service user ---------------------------------------------
svc_user=$user
if [[ -z $svc_user ]]; then
    svc_user=$(systemctl show --property=User --value "$service.service" 2>/dev/null || true)
fi
if [[ -z $svc_user ]]; then
    svc_user=$(sed -n 's/^User=//p' "$here/units/$service.service" | tail -n 1)
fi
[[ -n $svc_user ]] || die "can't tell which user $service runs as; pass --user NAME"
id -u -- "$svc_user" >/dev/null 2>&1 ||
    die "the service user $svc_user doesn't exist: create it (useradd --system --no-create-home --shell /usr/sbin/nologin $svc_user) or pass --user NAME"
echo "service: $service as $svc_user, config $config"
[[ -f $config ]] || die "$config doesn't exist yet: write it (see config.example.toml) and run this again"
install -o root -g root -m 755 "$here/chestcraft" "$new"
trap 'rm -f -- "$new"' EXIT
runuser -u "$svc_user" -- "$new" --config "$config" check ||
    die "the config check failed; nothing was changed"

# --- 3. units and drop-ins --------------------------------------------------------------
for unit in "$here"/units/*; do
    put "$unitdir/$(basename -- "$unit")" "$(cat -- "$unit")" && reload_units=1
done
if [[ -n $user ]]; then
    group=$(id -gn -- "$user")
    for svc in chestcraft chestcraft-staging; do
        put "$unitdir/$svc.service.d/20-user.conf" \
            "$(printf '# Written by install.sh --user\n[Service]\nUser=%s\nGroup=%s' "$user" "$group")" &&
            reload_units=1
    done
fi
paths=$("$new" --config "$config" check --systemd) || die "can't list the writable folders"
put "$unitdir/$service.service.d/10-paths.conf" "$paths" && reload_units=1
((reload_units)) && systemctl daemon-reload
running_as=$(systemctl show --property=User --value "$service.service")
[[ $running_as == "$svc_user" ]] ||
    die "$service.service runs as \"$running_as\", not $svc_user: check $unitdir/$service.service.d/"
# StateDirectory= / CacheDirectory= of the units: systemd makes them at the first start, but
# account commands (import-copyparty, user add) may run before that.
for dir in /var/lib/chestcraft /var/cache/chestcraft; do
    if [[ ! -e $dir ]]; then
        install -d -o "$svc_user" -g "$(id -gn -- "$svc_user")" -m 700 "$dir"
        echo "created $dir"
    fi
done
secrets=/etc/chestcraft/secrets.toml
if [[ ! -e $secrets ]]; then
    # LoadCredential= needs the file; tokens go in later (chestcraft telegram-setup).
    install -o root -g root -m 600 /dev/null "$secrets"
    echo "created empty $secrets"
fi

# --- 4. swap ----------------------------------------------------------------------------
if [[ -e $live ]]; then
    cp -p -- "$live" "$bindir/.chestcraft.prev.new"
    mv -f -- "$bindir/.chestcraft.prev.new" "$bindir/chestcraft.prev"
fi
mv -f -- "$new" "$live"
trap - EXIT
echo "installed $live ($version)"

systemctl enable --quiet chestcraft-collect.timer
systemctl start chestcraft-collect.timer
# A first reading now instead of at the next half hour.
systemctl start --no-block chestcraft-collect.service

# --- 5. restart -------------------------------------------------------------------------
# /healthz: "ok"; "starting" for a moment; "collector" means the SMART collector or a
# probe has a problem (smartmontools missing, timer not run yet): not the binary's fault.
healthy() {
    local url body i
    url=$("$live" --config "$config" check --healthz-url)
    for i in $(seq 60); do
        sleep 1
        if ! systemctl is-active --quiet "$service"; then
            echo "$service is not running" >&2
            return 1
        fi
        body=$(curl --silent --max-time 3 "$url" || true)
        case "$body" in
            ok) echo "healthz: ok"; return 0 ;;
            collector) echo "healthz: collector (see the dashboard; not the binary)"; return 0 ;;
        esac
    done
    echo "healthz after 60 s: ${body:-no answer} ($url)" >&2
    return 1
}

if ((restart == 0)); then
    echo "not restarted (--no-restart)"
elif ! systemctl is-active --quiet "$service"; then
    echo "$service isn't running; start it with: systemctl enable --now $service"
else
    systemctl restart "$service"
    if healthy; then
        echo "done: $version"
        exit 0
    fi
    if [[ ! -e $bindir/chestcraft.prev ]]; then
        die "the new version doesn't come up and there is no previous one: journalctl -u $service"
    fi
    echo "rolling back to $("$bindir/chestcraft.prev" --version 2>/dev/null || echo 'the previous binary')" >&2
    mv -f -- "$live" "$bindir/chestcraft.failed"
    cp -p -- "$bindir/chestcraft.prev" "$bindir/.chestcraft.back"
    mv -f -- "$bindir/.chestcraft.back" "$live"
    systemctl restart "$service"
    if healthy; then
        die "rolled back; the new version is kept as $bindir/chestcraft.failed (journalctl -u $service)"
    fi
    die "the previous version doesn't come up either: journalctl -u $service"
fi
