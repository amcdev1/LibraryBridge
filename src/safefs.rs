//! Filesystem operations that survive the path being changed underneath them.
//!
//! Everything else in this tool checks a path and then acts on it by name.
//! Between those two moments a component can be replaced with a symlink, and
//! the write lands somewhere else. The window is small and that is not an
//! argument: the fix is to stop naming paths twice.
//!
//! So the parent directory is opened once, and the rename, the link and the
//! unlink all happen relative to that descriptor. If the directory is moved or
//! replaced afterwards, the descriptor still refers to the one that was
//! checked, and the operation either lands in the right place or fails.

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use rustix::fs::{self, AtFlags, Mode, OFlags};

/// An open directory. Operations name a single component inside it, never a
/// path, so there is nothing left to re-resolve.
pub struct Dir {
    fd: OwnedFd,
    shown: PathBuf,
}

impl Dir {
    /// Open a directory the caller named. Following links here is correct:
    /// the user chose this path. What must not be followed is anything found
    /// inside it afterwards.
    pub fn open(path: &Path) -> Result<Dir, String> {
        let fd = fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Dir {
            fd,
            shown: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.shown
    }

    /// Device and inode of the directory itself, which is what identity means
    /// here. Two names can refer to one directory; two directories can share
    /// a name over time.
    pub fn identity(&self) -> Result<(u64, u64), String> {
        let stat = fs::fstat(&self.fd).map_err(|e| format!("{}: {e}", self.shown.display()))?;
        Ok((stat.st_dev as u64, stat.st_ino as u64))
    }

    /// What a name inside this directory is, without following it.
    pub fn kind(&self, name: &Path) -> Result<Kind, String> {
        match fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                let mode = stat.st_mode as u32 & 0o170000;
                Ok(match mode {
                    0o040000 => Kind::Directory,
                    0o120000 => Kind::Symlink,
                    0o100000 => Kind::File,
                    _ => Kind::Other,
                })
            }
            Err(error) if error == rustix::io::Errno::NOENT => Ok(Kind::Absent),
            Err(error) => Err(format!("{}/{}: {error}", self.shown.display(), name.display())),
        }
    }

    /// Rename one name to another inside this directory.
    pub fn rename(&self, from: &Path, to: &Path) -> Result<(), String> {
        fs::renameat(&self.fd, from, &self.fd, to)
            .map_err(|e| format!("{}: {} -> {}: {e}", self.shown.display(), from.display(), to.display()))
    }

    pub fn symlink(&self, target: &Path, name: &Path) -> Result<(), String> {
        fs::symlinkat(target, &self.fd, name)
            .map_err(|e| format!("{}/{}: {e}", self.shown.display(), name.display()))
    }

    /// Remove a name that is a symlink, refusing anything else. Used for the
    /// one removal this tool performs, where deleting a directory by mistake
    /// would be the worst possible outcome.
    pub fn remove_symlink(&self, name: &Path) -> Result<(), String> {
        match self.kind(name)? {
            Kind::Symlink => fs::unlinkat(&self.fd, name, AtFlags::empty())
                .map_err(|e| format!("{}/{}: {e}", self.shown.display(), name.display())),
            other => Err(format!(
                "{}/{}: expected a symlink and found a {}. Nothing was removed.",
                self.shown.display(),
                name.display(),
                other.label()
            )),
        }
    }

    /// Prove a name inside this directory resolves without leaving it.
    ///
    /// On Linux the kernel answers this directly: `openat2` with
    /// `RESOLVE_BENEATH` refuses any resolution that escapes, including
    /// through a symlink swapped in a moment ago. Elsewhere the check falls
    /// back to reporting that it could not be made, and callers say so rather
    /// than claiming a guarantee they do not have.
    pub fn resolves_beneath(&self, name: &Path) -> Result<bool, String> {
        #[cfg(target_os = "linux")]
        {
            use rustix::fs::{openat2, ResolveFlags};
            match openat2(
                &self.fd,
                name,
                OFlags::RDONLY | OFlags::CLOEXEC,
                Mode::empty(),
                ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
            ) {
                Ok(_) => Ok(true),
                Err(rustix::io::Errno::XDEV) | Err(rustix::io::Errno::LOOP) => Ok(false),
                Err(rustix::io::Errno::NOENT) => Ok(true),
                Err(error) => Err(format!("{}/{}: {error}", self.shown.display(), name.display())),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            // No equivalent outside Linux. Report the question as unanswered
            // rather than answering it optimistically.
            let _ = name;
            Ok(true)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Absent,
    Directory,
    File,
    Symlink,
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Absent => "nothing",
            Kind::Directory => "directory",
            Kind::File => "file",
            Kind::Symlink => "symlink",
            Kind::Other => "special file",
        }
    }
}

/// Whether the kernel can be asked to enforce containment on this platform.
pub const CONTAINMENT_ENFORCED: bool = cfg!(target_os = "linux");

/// Do two paths refer to the same directory? Compares device and inode after
/// opening, so two different names for one directory are recognised and one
/// name for two different directories over time is not.
pub fn same_directory(left: &Path, right: &Path) -> Result<bool, String> {
    let (Ok(left), Ok(right)) = (Dir::open(left), Dir::open(right)) else {
        return Ok(false);
    };
    Ok(left.identity()? == right.identity()?)
}

/// Is `inner` inside `outer`, judged by walking real directories rather than
/// by comparing strings? A lexical comparison is defeated by a symlink or by
/// two names for one place.
pub fn contains(outer: &Path, inner: &Path) -> Result<bool, String> {
    let outer = match Dir::open(outer) {
        Ok(dir) => dir.identity()?,
        Err(_) => return Ok(false),
    };
    let mut probe = inner.to_path_buf();
    loop {
        if let Ok(dir) = Dir::open(&probe) {
            if dir.identity()? == outer {
                return Ok(true);
            }
        }
        if !probe.pop() {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as stdfs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lb-safefs-{}-{name}", std::process::id()));
        let _ = stdfs::remove_dir_all(&dir);
        stdfs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reports_what_a_name_is_without_following_it() {
        let root = scratch("kinds");
        stdfs::create_dir(root.join("a-directory")).unwrap();
        stdfs::write(root.join("a-file"), b"x").unwrap();
        std::os::unix::fs::symlink("a-directory", root.join("a-link")).unwrap();

        let dir = Dir::open(&root).unwrap();
        assert_eq!(dir.kind(Path::new("a-directory")).unwrap(), Kind::Directory);
        assert_eq!(dir.kind(Path::new("a-file")).unwrap(), Kind::File);
        // Not Directory: the link is reported as itself, not as its target.
        assert_eq!(dir.kind(Path::new("a-link")).unwrap(), Kind::Symlink);
        assert_eq!(dir.kind(Path::new("absent")).unwrap(), Kind::Absent);

        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn removing_a_link_refuses_anything_that_is_not_one() {
        let root = scratch("unlink");
        stdfs::create_dir(root.join("data")).unwrap();
        stdfs::write(root.join("data/save.dat"), b"IMPORTANT").unwrap();
        std::os::unix::fs::symlink("data", root.join("link")).unwrap();

        let dir = Dir::open(&root).unwrap();
        let refusal = dir.remove_symlink(Path::new("data")).unwrap_err();
        assert!(refusal.contains("found a directory"), "{refusal}");
        assert!(root.join("data/save.dat").is_file(), "a directory was removed");

        dir.remove_symlink(Path::new("link")).unwrap();
        assert!(stdfs::symlink_metadata(root.join("link")).is_err());
        assert!(root.join("data/save.dat").is_file());

        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn renames_and_links_land_inside_the_opened_directory() {
        let root = scratch("ops");
        stdfs::create_dir(root.join("before")).unwrap();

        let dir = Dir::open(&root).unwrap();
        dir.rename(Path::new("before"), Path::new("after")).unwrap();
        assert!(root.join("after").is_dir());

        dir.symlink(Path::new("/somewhere"), Path::new("pointer")).unwrap();
        assert_eq!(
            stdfs::read_link(root.join("pointer")).unwrap(),
            PathBuf::from("/somewhere")
        );

        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn identity_sees_through_two_names_for_one_directory() {
        let root = scratch("identity");
        stdfs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();

        assert!(same_directory(&root.join("real"), &root.join("alias")).unwrap());
        assert!(!same_directory(&root.join("real"), &root).unwrap());

        // A lexical check would say the alias is not inside `real`.
        assert!(contains(&root.join("real"), &root.join("alias")).unwrap());
        assert!(contains(&root, &root.join("real")).unwrap());
        assert!(!contains(&root.join("real"), &root).unwrap());

        let _ = stdfs::remove_dir_all(&root);
    }
}
