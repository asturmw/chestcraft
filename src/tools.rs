//! External programs. Always called by absolute path: the service user's PATH has no
//! /usr/sbin on Debian.

pub const ZPOOL: &str = "/usr/sbin/zpool";
pub const ZFS: &str = "/usr/sbin/zfs";
pub const SMARTCTL: &str = "/usr/sbin/smartctl";
pub const FFMPEG: &str = "/usr/bin/ffmpeg";
pub const CURL: &str = "/usr/bin/curl";

pub const ALL: &[&str] = &[ZPOOL, ZFS, SMARTCTL, FFMPEG, CURL];
