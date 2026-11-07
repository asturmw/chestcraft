# Health check and status API

Two endpoints for watchdogs and monitoring tools. Neither one changes anything. (Agents use
the MCP server instead: `docs/mcp.md`.)

## `GET /healthz`

No sign-in. Answers from memory, so it stays quick even when the state database or a pool
hangs. Plain text, `Cache-Control: no-store`.

| Status | Body | Meaning |
|---|---|---|
| 200 | `ok` | chestcraft and its monitoring work |
| 503 | one or more words, space-separated | see below |

| Word | Meaning |
|---|---|
| `starting` | Started less than 2 minutes ago and the first database heartbeat or health snapshot is still missing. |
| `db` | The state database took no write in the last 3 minutes (heartbeat once a minute: a synced write, read back). |
| `monitor` | The newest health snapshot is older than 5 minutes: the monitor loop is stuck. |
| `collector` | A health source fails: the SMART collector's file (`chestcraft collect`) is missing, unreadable or older than `alerts.thresholds.stale_minutes`, or `zpool`/`zfs`/`/proc/mdstat` can't be read. Same condition as the `smart:*` and `probe:*` alerts. |

The machine's own trouble (a degraded pool, a hot disk, a full dataset) is **not** a 503:
chestcraft alerts about that itself, and the status API reports it. A 503 means the
alerting itself can't be trusted.

For deploy scripts: after a restart, poll until `ok`. Only `db`, `monitor` or no answer at
all point at the new binary; `collector` is the SMART timer or a pool (a fresh install
says `collector` until `chestcraft collect` has run once).

## `GET /api/v1/status`

JSON with the same verdict and alerts as the admin dashboard and a summary of what the
monitor sees.

- **Reachable only from the local networks** (`server.local_networks`, which include
  Tailscale by default). Requests from the internet (through a remote proxy, with
  Cloudflare headers, or from an address outside the local networks) get `404`.
- **Token:** `Authorization: Bearer ccs_…`. The admin creates tokens on the volumes page
  (`/.cc/admin/volumes`, section "Status API tokens") or with
  `chestcraft api-token add LABEL` (service stopped); each is shown once, stored as a
  SHA-256, revocable. A token is read-only and not tied to an account. At most 20.
- `401` + `WWW-Authenticate: Bearer` without a valid token (`{"error": "token required"}`
  or `{"error": "invalid token"}`). Wrong tokens count like wrong passwords: after 5 the
  address waits (`429`, `Retry-After`), a minute at most on the LAN.
- `Cache-Control: no-store`. Successful calls aren't audited; the token's "last used" (time
  and address) is updated at most every 10 minutes. Failed calls are audited
  (`api_token_failed`).

```sh
curl -H "Authorization: Bearer ccs_…" http://nas.home.arpa:3923/api/v1/status
```

### Response (`api: 1`)

Times are Unix seconds, sizes bytes. Within `api: 1` fields are only added, never renamed
or removed; anything else gets a new version and URL.

```jsonc
{
  "api": 1,
  "version": "0.1.0",          // chestcraft version
  "now": 1762448000,
  "started": 1762440000,       // when the server started
  "healthy": true,             // /healthz would answer 200
  "problems": [],              // /healthz words: "starting", "db", "monitor", "collector"
  "verdict": "ok",             // dashboard verdict: ok, warning, critical, stale
  "as_of": 1762447990,         // newest snapshot; null before the first one
  "sources": {                 // empty before the first snapshot
    "pools":    { "state": "ok" },              // ok, absent (no ZFS), failed (+ "error")
    "datasets": { "state": "ok" },
    "md":       { "state": "absent" },
    "smart":    { "state": "ok", "collected": 1762446200 }  // ok, stale, missing, failed (+ "error")
  },
  "alerts": [                  // active alerts, most severe first
    { "key": "disk:WD-XXXX:temp", "severity": "warning",    // info, warning, critical
      "title": "Bay 2 (…) is at 53 °C", "since": 1762440500 }  // title in alerts.language
  ],
  "pools": [
    { "name": "tank", "state": "ONLINE", "size": 0, "alloc": 0, "free": 0,
      "capacity": 61,          // percent used
      "frag": 3,               // percent or null
      "read_errors": 0, "write_errors": 0, "cksum_errors": 0,   // summed over all vdevs
      "data_errors": 0,        // files with permanent errors
      "vdevs_not_online": [],  // vdev names (any level) not ONLINE (idle spares don't count)
      "scan": { "function": "SCRUB", "state": "FINISHED", "start": 0, "end": 0,
                "errors": 0, "percent": 100.0, "paused": false } }   // or null
  ],
  "datasets": [
    { "name": "tank/data", "pool": "tank", "used": 0, "avail": 0, "quota": 0,  // 0 = none
      "snapshots": 30, "snap_used": 0, "newest_snapshot": 1762444800,
      "volumes": ["Data"],     // chestcraft volumes on it
      "trash": 0 }             // bytes in chestcraft's trash there
  ],
  "md": [
    { "name": "md0", "state": "active", "level": "raid1", "status": "[UU]",
      "degraded": false, "members": ["nvme0n1p2", "nvme1n1p2"], "faulty": [],
      "sync": null }           // or { "action": "resync", "percent": 37.5 }
  ],
  "disks": [
    { "name": "sda", "label": "Bay 1", "kind": "hdd",   // hdd, ssd, nvme
      "model": "…", "serial": "…", "size": 0,
      "pools": ["tank"], "md": [],
      "smart": "ok",           // ok, standby (last reading while awake), error (+ "smart_error"),
                               // unsupported, none (not in the collector's file)
      "checked": 1762446200,   // when the SMART data was read
      "passed": true, "temp": 38, "power_on_hours": 21000,
      "realloc": 0, "pending": 0, "uncorrectable": 0, "crc": 0,
      "wear": null, "media_errors": null,   // NVMe / SSD
      "self_test": { "kind": "short", "passed": true, "result": "Completed without error" } }
  ],
  "host": {                    // null before the first snapshot; each field null if unknown
    "uptime": 86400, "load": [0.1, 0.2, 0.3], "cpus": 4,
    "mem_total": 0, "mem_available": 0, "cpu_temp": 45.0,
    "arc_size": 0, "arc_hit_rate": 98.5,
    "root_fs": { "total": 0, "used": 0, "avail": 0, "percent": 12 } },
  "app": { "sessions": 3, "transfers": 0, "shares_active": 5, "failed_logins_24h": 0,
           "agent_calls_24h": 12,      // MCP tool calls by agents
           "backup": {                 // nightly state backup; null until the first run
             "attempt": 1762398000, "last_ok": 1762398000, "error": null, "bytes": 2097152 } },
  "channels": [                // configured alert channels
    { "kind": "telegram", "on": true, "problem": null,
      "last_ok": 1762400000, "last_error": null, "last_error_at": null } ]
}
```

Sections for hardware the machine doesn't have are empty lists. A watchdog that wants one
answer reads `healthy` (can chestcraft be trusted to alert?) and `verdict` (does it see a
problem?).
