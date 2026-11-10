# chestcraft

A self-hosted file manager and sharing app for a home NAS, written in Rust.

- Browse, preview, upload and organize files in configured volumes
- Password-protected share links for folders and files (view, view + upload, upload only)
- Optional public volumes, with password-protected public uploads
- WebDAV for desktop mounts
- Admin dashboard: ZFS pool, datasets, disks (SMART), boot mirror, host, and charts of
  processor, memory, network and disk activity over 24 hours, 7 or 90 days; alerts via Telegram
- `/healthz` and a read-only JSON status API for watchdogs ([docs/api.md](docs/api.md))
- One static binary with pure-Rust dependencies and no JavaScript build step

Status: early development, not usable yet. Building, installing, updates and restoring
from the nightly backup: [docs/install.md](docs/install.md). Working on the code (by hand
or with a coding agent): [AGENTS.md](AGENTS.md) has the project map, a local dev setup and
the rules a change must keep.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as
above, without any additional terms or conditions.

The English passphrase word list (`src/shares/words/en.txt`) is the EFF Short Wordlist by
the Electronic Frontier Foundation, under CC BY 3.0; see `src/shares/words/README.md`.

The built-in background pictures (`assets/backgrounds/*.jpg`) were generated with the Anima
model and upscaled with RealESRGAN x2plus; the Anima license restricts use of the model,
not of its outputs.
