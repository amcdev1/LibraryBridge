//! What the operating system can tell us: which filesystem a path sits on,
//! whether Steam is running, and how much room the destination has.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    /// A Linux filesystem that already works. Nothing to repair.
    Native,
    /// Symlinks are possible, but Proton needs its data elsewhere.
    NeedsRepair,
    /// No symlink support at all. The repair cannot work here.
    NoSymlinks,
    /// Not identified. Happens off Linux and on unusual mounts.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Mount {
    pub mount_point: PathBuf,
    pub fs_type: String,
    pub source: String,
    pub read_only: bool,
    pub capability: Capability,
}

impl Mount {
    pub fn describe(&self) -> String {
        let mut text = self.fs_type.clone();
        if !self.source.is_empty() {
            text.push_str(&format!(" on {}", self.source));
        }
        if self.read_only {
            text.push_str(", mounted read-only");
        }
        text
    }
}

fn classify(fs_type: &str) -> Capability {
    match fs_type {
        "ext2" | "ext3" | "ext4" | "btrfs" | "xfs" | "f2fs" | "zfs" | "bcachefs" | "reiserfs"
        | "jfs" | "nilfs2" | "overlay" | "apfs" | "hfs" => Capability::Native,
        "ntfs" | "ntfs3" | "ntfs-3g" => Capability::NeedsRepair,
        "exfat" | "vfat" | "msdos" | "fat" | "fat32" | "iso9660" | "udf" => Capability::NoSymlinks,
        _ => Capability::Unknown,
    }
}

/// The mount covering `path`, from `/proc/self/mountinfo`. Returns `None`
/// where that file does not exist, which includes every development machine
/// that is not Linux.
pub fn mount_for(path: &Path) -> Option<Mount> {
    let text = fs::read_to_string("/proc/self/mountinfo").ok()?;
    let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let mut best: Option<Mount> = None;
    let mut best_len = 0usize;

    for line in text.lines() {
        let Some(parsed) = parse_mountinfo_line(line) else {
            continue;
        };
        if !target.starts_with(&parsed.mount_point) {
            continue;
        }
        let len = parsed.mount_point.as_os_str().len();
        if best.is_none() || len > best_len {
            best_len = len;
            best = Some(parsed);
        }
    }
    if let Some(mount) = best {
        // udisks2 mounts ntfs-3g and exfat-fuse with a plain device node as
        // the source, which says nothing about what is down there. Ask the
        // device what it is. The parser cannot, which is why this lives here.
        if mount.fs_type == "fuseblk" && mount.capability == Capability::Unknown {
            if let Some(name) = identify_fuseblk(Path::new(&mount.source)) {
                let mut identified = mount.clone();
                identified.capability = classify(&name);
                identified.fs_type = name;
                return Some(identified);
            }
        }
        Some(mount)
    } else {
        None
    }
}

/// mountinfo lines look like:
/// `36 35 98:0 /root /mount/point rw,noatime shared:1 - ext4 /dev/sda1 rw,data=ordered`
/// Everything between the optional fields and the filesystem type is
/// terminated by a lone `-`.
fn parse_mountinfo_line(line: &str) -> Option<Mount> {
    let fields: Vec<&str> = line.split(' ').collect();
    if fields.len() < 10 {
        return None;
    }
    let mount_point = unescape_octal(fields.get(4)?);
    let options = *fields.get(5)?;
    let separator = fields.iter().position(|f| *f == "-")?;
    let fs_type = fields.get(separator + 1)?.to_string();
    let source = unescape_octal(fields.get(separator + 2)?);
    let super_options = fields.get(separator + 3).copied().unwrap_or("");

    let read_only =
        options.split(',').any(|o| o == "ro") || super_options.split(',').any(|o| o == "ro");

    // A FUSE filesystem on a block device reports `fuseblk` and says nothing
    // about what is actually down there. ntfs-3g and exfat-fuse look the same
    // from here, and treating them alike would offer an exFAT volume a repair
    // that cannot work. Only a mount whose source names NTFS is taken as NTFS
    // here; anything else stays unidentified, and `mount_for` asks the device
    // itself before acting on it.
    let fs_type = if fs_type == "fuseblk" {
        let hint = source.to_lowercase();
        if hint.contains("ntfs") {
            "ntfs-3g".to_string()
        } else {
            "fuseblk".to_string()
        }
    } else {
        fs_type
    };
    let capability = classify(&fs_type);

    Some(Mount {
        mount_point: PathBuf::from(mount_point),
        fs_type,
        source,
        read_only,
        capability,
    })
}

/// What is beneath a FUSE filesystem mounted from a block device.
///
/// ntfs-3g and exfat-fuse both report `fuseblk` with a plain device node as
/// the source, so the mount table cannot tell them apart. The device itself
/// can. Returns a filesystem type name the classifier understands, or nothing
/// when the device cannot be asked.
fn identify_fuseblk(device: &Path) -> Option<String> {
    let output = Command::new("lsblk").args(["-no", "FSTYPE"]).arg(device).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    fuseblk_type_from_lsblk(text.trim())
}

/// The part of `identify_fuseblk` that does not touch the system.
fn fuseblk_type_from_lsblk(output: &str) -> Option<String> {
    match output.trim() {
        "" => None,
        "ntfs" => Some("ntfs-3g".to_string()),
        other => Some(str::to_string(other)),
    }
}

fn unescape_octal(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let digits = &input[i + 1..i + 4];
            if let Ok(value) = u8::from_str_radix(digits, 8) {
                out.push(value as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Processes that must not be running during a repair. Steam writing into a
/// prefix while it is being moved aside would lose those writes.
pub const BLOCKING_PROCESSES: [&str; 7] = [
    "steam",
    "steamwebhelper",
    "wineserver",
    "wine64-preloader",
    "proton",
    "pressure-vessel-wrap",
    "gameoverlayui",
];

/// Which blocking processes are running.
///
/// An error means the question could not be answered. Callers refuse rather
/// than treating an unanswerable question as a no: an empty list used to mean
/// both "nothing is running" and "nothing could be seen".
pub fn running_steam_processes() -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let comm = entry.path().join("comm");
            if let Ok(name) = fs::read_to_string(&comm) {
                let name = name.trim();
                if BLOCKING_PROCESSES.contains(&name) && !found.contains(&name.to_string()) {
                    found.push(name.to_string());
                }
            }
        }
        return Ok(found);
    }

    // Not Linux. Fall back to ps so development hosts still get a real answer.
    let output = Command::new("ps")
        .args(["-A", "-o", "comm="])
        .output()
        .map_err(|e| format!("could not list running processes: {e}"))?;
    if !output.status.success() {
        return Err("could not list running processes".to_string());
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let name = Path::new(line.trim())
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if BLOCKING_PROCESSES.contains(&name.as_str()) && !found.contains(&name) {
            found.push(name);
        }
    }
    Ok(found)
}

/// Free bytes on the filesystem holding `path`, via `df`. Returns `None` if
/// `df` is missing or its output cannot be read, in which case the caller
/// reports the space check as unavailable rather than guessing.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            return None;
        }
    }
    let output = Command::new("df").args(["-Pk"]).arg(&probe).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().nth(1)?;
    let available_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(available_kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_mountinfo_line() {
        let line = "36 35 8:1 / /run/media/alice/Games rw,nosuid,relatime shared:1 - ntfs3 /dev/sdb1 rw,uid=1000";
        let mount = parse_mountinfo_line(line).unwrap();
        assert_eq!(mount.mount_point, PathBuf::from("/run/media/alice/Games"));
        assert_eq!(mount.fs_type, "ntfs3");
        assert_eq!(mount.capability, Capability::NeedsRepair);
        assert!(!mount.read_only);
    }

    #[test]
    fn a_fuse_mount_is_only_ntfs_when_it_says_so() {
        // ntfs-3g names itself in the mount source.
        let line = "40 35 0:45 / /mnt/games rw,relatime - fuseblk /dev/sdb1#ntfs rw,user_id=0";
        let mount = parse_mountinfo_line(line).unwrap();
        assert_eq!(mount.capability, Capability::NeedsRepair);
        assert_eq!(mount.fs_type, "ntfs-3g");

        // Any other source could be exfat-fuse, which cannot hold a symlink.
        // The parser leaves this unidentified; `mount_for` asks the device
        // itself, because a plain device node does not say what is down there.
        let line = "40 35 0:45 / /mnt/games rw,relatime - fuseblk /dev/sdb1 rw,user_id=0";
        let mount = parse_mountinfo_line(line).unwrap();
        assert_eq!(mount.capability, Capability::Unknown);
    }

    #[test]
    fn maps_what_lsblk_reports_for_a_fuse_device() {
        assert_eq!(fuseblk_type_from_lsblk("ntfs\n"), Some("ntfs-3g".to_string()));
        assert_eq!(fuseblk_type_from_lsblk("exfat\n"), Some("exfat".to_string()));
        assert_eq!(fuseblk_type_from_lsblk(""), None);
        assert_eq!(fuseblk_type_from_lsblk("   \n"), None);
    }

    #[test]
    fn notices_read_only_mounts() {
        let line = "36 35 8:1 / /mnt/x ro,relatime - exfat /dev/sdb1 ro";
        let mount = parse_mountinfo_line(line).unwrap();
        assert!(mount.read_only);
        assert_eq!(mount.capability, Capability::NoSymlinks);
    }

    #[test]
    fn decodes_escaped_mount_points() {
        let line = "36 35 8:1 / /mnt/My\\040Games rw,relatime - ext4 /dev/sdb1 rw";
        let mount = parse_mountinfo_line(line).unwrap();
        assert_eq!(mount.mount_point, PathBuf::from("/mnt/My Games"));
        assert_eq!(mount.capability, Capability::Native);
    }

    #[test]
    fn handles_optional_fields_being_absent() {
        let line = "36 35 8:1 / /mnt/x rw,relatime - btrfs /dev/sdb1 rw";
        assert!(parse_mountinfo_line(line).is_some());
    }
}
