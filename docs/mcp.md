# MCP server for agents

AI agents (Claude Code, Hermes, any MCP client) use chestcraft through MCP: Streamable
HTTP at `/mcp` on its own port. It speaks the 2026-07-28 revision (no handshake,
`server/discover`) and the older ones with `initialize` (2025-11-25 and earlier),
statelessly: JSON responses only, no session ids, no server-sent streams.

## Turning it on

```toml
[mcp]
listen = "0.0.0.0:3925"
hosts = ["nas.home.arpa:3925", "100.64.0.5:3925"]   # Host names clients use
health_roles = ["agenten"]                         # roles that may read pools/disks/alerts
```

- **Local networks only.** Requests from outside `server.local_networks` (Tailscale is
  inside by default), through a remote proxy, or with Cloudflare headers get `403`. Don't
  point a tunnel or reverse proxy at this port.
- **Host and Origin.** The `Host` header must be in `[mcp] hosts` (an entry without a port
  matches any port), and a request with an `Origin` header not in `[mcp] origins` gets
  `403`. That stops a web page from reaching the server through DNS rebinding.
- **Tokens.** `Authorization: Bearer cca_…`, else `401`. Wrong tokens count like wrong
  passwords: after 5 the address waits (`429`, `Retry-After`), a minute at most on the LAN.
- **Limits.** Per agent: `calls_per_minute` (default 120) and `max_concurrent` running calls
  (default 4; a call waits up to 30 s for a slot), else `429`. `read_file` returns at most
  `max_read_mib` (default 4) per call, `write_file` writes at most `max_write_mib` (default
  16).

## Agents

An agent is an account of its own: a name and one role from `[roles]`, never `admin`. It has
no password and can't sign in on the web or use WebDAV; it only has tokens, one per host it
runs on. Run these as the service user while the service is stopped:

```sh
chestcraft agent add hermes --role agenten
chestcraft agent token hermes "desktop" --expires 90d # prints the token once
chestcraft agent list                                  # agents, tokens, last use
chestcraft agent role hermes leser
chestcraft agent revoke hermes desktop                 # by label or id
chestcraft agent disable hermes                        # tokens stop, its shares pause
chestcraft agent enable hermes
```

**The role is the wall.** Assume an agent can be steered by what it reads (a file can
contain instructions), and that an agent on a cloud model sends what it reads to its
provider. Give each agent the smallest role it needs, and `s` only if it should publish
files to the internet.

## Clients

Claude Code:

```sh
claude mcp add --transport http chestcraft http://nas.home.arpa:3925/mcp \
  --header "Authorization: Bearer cca_…"
```

Hermes (`mcp_servers` in its config, then `/reload-mcp`):

```yaml
mcp_servers:
  chestcraft:
    url: "http://nas.home.arpa:3925/mcp"
    headers:
      Authorization: "Bearer cca_…"
```

## Tools

Paths look like `/Volume/folder/file.txt` (`list` on `/` returns the volumes). Every call
uses the same path rules and role × volume permissions as the web and WebDAV, as seen from
the LAN. Dot-names are invisible to agents; chestcraft's own folders (`.chestcraft-*`,
`.hist`) to everyone. An agent sees only the tool groups its role can use.

| Tool | Needs | What it does |
|---|---|---|
| `volumes` | | Volumes the agent may read, its letters, free space, flags |
| `list` | r | One folder: folders first, natural order or by `modified`/`size`, `offset`/`limit` (≤ 1000) |
| `stat` | r | Type, size, time, media type, letters |
| `find` | r | Names in one folder matching `*`/`?` or a substring (not recursive) |
| `read_file` | r | Text as text, PNG/JPEG/GIF/WebP as images, else base64; `offset`/`length`, `next_offset`; `preview=true`: a picture (≤ 2048 px) of images, videos, STL/3MF, G-code, SVG |
| `mkdir` | w | One new folder in an existing folder |
| `write_file` | w | A new file (text or base64), written as a temporary file, synced, renamed into place (644) |
| `rename` | m | New name in the same folder |
| `move` | m (+ w on another volume) | Into another folder; across filesystems a copy plus the original in the trash |
| `copy` | r + w | Into another folder; dot-names inside folders are left out |
| `delete` | d | To the trash |
| `trash_list`, `trash_restore` | w/m/d somewhere | The agent's own trash; restoring never overwrites (`name (restored)`) |
| `share_create` | s | Password link, same rules as the share dialog (volumes with `remote = true`, never beyond the role); default `view`, `7d`, generated passphrase; link and password returned once; **every agent share raises an alert** (`notify_shares`) |
| `share_list`, `share_revoke` | s | The agent's own shares |
| `health_summary`, `alerts`, `pool`, `disk`, `datasets` | role in `health_roles` | The dashboard's data, read-only |

**Never overwrites.** Writes, copies, moves and renames take `on_clash`: `keep_both`
(default, a free name like `name (1)`) or `skip`. There is no replace, no append and no
"empty trash", so a file only leaves through the trash purge.

Errors come back as tool results with `isError: true` and
`{"error": "<code>", "message": "…"}`; codes: `not_found` (also for things the agent may
not see), `denied`, `exists`, `bad_path`, `bad_arguments`, `not_a_folder`, `not_a_file`,
`too_large`, `no_preview`, `no_space`, `read_only`, `busy`, `invalid`, `too_many`,
`no_data`, `failed`.

## Audit

Every call is in the audit log, reads included: actor `agent:<name>`, action
`mcp_<tool>`, the paths (or share/trash ids), the token label, the result and the bytes
read or written. Wrong tokens are `mcp_token_failed`. The dashboard's app section shows the
agent calls of the last 24 hours and each agent's last call; the status API has
`app.agent_calls_24h`.
