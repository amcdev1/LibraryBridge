//! Inventory, copy and verification of a compatdata tree.
//!
//! Two rules govern everything here:
//!
//! 1. Symlinks are never followed. A Wine prefix contains `dosdevices/z:`
//!    pointing at `/`, so a copier that follows links would try to copy the
//!    entire root filesystem into the destination.
//! 2. Nothing is ever removed from the source. This module only reads the
//!    source and writes to a fresh destination.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::sha256::{hex, Sha256};

const MAX_DEPTH: usize = 128;
const COPY_BUFFER: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File {
        size: u64,
        digest: Option<[u8; 32]>,
        links: u64,
        /// Device and inode. Two entries sharing these are one file with two
        /// names, and the copy has to reproduce that rather than duplicate it.
        identity: (u64, u64),
        /// Bytes actually allocated. Less than `size` means the file is
        /// stored sparsely and a plain copy will occupy more room.
        allocated: u64,
    },
    Symlink {
        target: PathBuf,
    },
}

impl Kind {
    pub fn label(&self) -> &'static str {
        match self {
            Kind::Dir => "directory",
            Kind::File { .. } => "file",
            Kind::Symlink { .. } => "symlink",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub rel: PathBuf,
    pub kind: Kind,
    pub mode: u32,
    pub mtime: Option<SystemTime>,
}

#[derive(Debug, Default, Clone)]
pub struct Manifest {
    pub entries: Vec<Entry>,
    pub bytes: u64,
    pub dirs: usize,
    pub files: usize,
    pub symlinks: usize,
    /// Files that share their contents with another name.
    pub hard_linked: Vec<PathBuf>,
    /// Files stored with holes, and how many bytes those holes account for.
    pub sparse: Vec<PathBuf>,
    pub sparse_saving: u64,
}

impl Manifest {
    fn push(&mut self, entry: Entry) {
        match &entry.kind {
            Kind::Dir => self.dirs += 1,
            Kind::File { size, links, allocated, .. } => {
                self.files += 1;
                self.bytes += size;
                if *links > 1 {
                    self.hard_linked.push(entry.rel.clone());
                }
                if allocated < size {
                    self.sparse.push(entry.rel.clone());
                    self.sparse_saving += size - allocated;
                }
            }
            Kind::Symlink { .. } => self.symlinks += 1,
        }
        self.entries.push(entry);
    }
}

/// Walk a tree without following symlinks, recording every entry.
///
/// With `hash` set, regular files are read and digested. Without it, only
/// metadata is collected, which is what the post-copy source recheck needs.
pub fn inventory(root: &Path, hash: bool) -> Result<Manifest, String> {
    let mut manifest = Manifest::default();
    walk(root, Path::new(""), 0, hash, &mut manifest)?;
    manifest.entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(manifest)
}

fn walk(
    root: &Path,
    rel: &Path,
    depth: usize,
    hash: bool,
    out: &mut Manifest,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "{}: directory nesting is deeper than {MAX_DEPTH} levels, refusing to continue",
            root.join(rel).display()
        ));
    }
    let dir = root.join(rel);
    let reader = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let mut children: Vec<PathBuf> = Vec::new();
    for item in reader {
        let item = item.map_err(|e| format!("{}: {e}", dir.display()))?;
        children.push(rel.join(item.file_name()));
    }
    children.sort();

    for child_rel in children {
        let abs = root.join(&child_rel);
        let meta = fs::symlink_metadata(&abs).map_err(|e| format!("{}: {e}", abs.display()))?;
        let mode = meta.permissions().mode();
        let mtime = meta.modified().ok();
        let file_type = meta.file_type();

        if file_type.is_symlink() {
            let target = fs::read_link(&abs).map_err(|e| format!("{}: {e}", abs.display()))?;
            out.push(Entry {
                rel: child_rel,
                kind: Kind::Symlink { target },
                mode,
                mtime,
            });
        } else if file_type.is_dir() {
            out.push(Entry {
                rel: child_rel.clone(),
                kind: Kind::Dir,
                mode,
                mtime,
            });
            walk(root, &child_rel, depth + 1, hash, out)?;
        } else if file_type.is_file() {
            let digest = if hash { Some(hash_file(&abs)?) } else { None };
            out.push(Entry {
                rel: child_rel,
                kind: Kind::File {
                    size: meta.len(),
                    digest,
                    links: meta.nlink(),
                    identity: (meta.dev(), meta.ino()),
                    allocated: meta.blocks() * 512,
                },
                mode,
                mtime,
            });
        } else {
            return Err(format!(
                "{}: not a regular file, directory or symlink. LibraryBridge will not copy \
                 sockets, pipes or device nodes, so this tree needs manual attention.",
                abs.display()
            ));
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<[u8; 32], String> {
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUFFER];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finish())
}

/// Copy `src` to `dst`, digesting file contents as the bytes stream past so
/// the source is read exactly once. `dst` must not already exist.
pub fn copy_tree(
    src: &Path,
    dst: &Path,
    progress: &mut dyn FnMut(&Path, u64),
) -> Result<Manifest, String> {
    // Containment judged by real directories, not by comparing strings. Two
    // names can lead to one place, and a lexical check misses that.
    if crate::safefs::contains(src, dst)? || crate::safefs::contains(dst, src)? {
        return Err(format!(
            "refusing to copy {} into {}: one path contains the other",
            src.display(),
            dst.display()
        ));
    }
    if fs::symlink_metadata(dst).is_ok() {
        return Err(format!("{}: already exists", dst.display()));
    }
    fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;

    let mut manifest = Manifest::default();
    let mut links: HashMap<(u64, u64), (PathBuf, [u8; 32])> = HashMap::new();
    copy_dir(src, dst, Path::new(""), 0, &mut manifest, &mut links, progress)?;
    manifest.entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(manifest)
}

#[allow(clippy::too_many_arguments)]
fn copy_dir(
    src: &Path,
    dst: &Path,
    rel: &Path,
    depth: usize,
    out: &mut Manifest,
    links: &mut HashMap<(u64, u64), (PathBuf, [u8; 32])>,
    progress: &mut dyn FnMut(&Path, u64),
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "{}: directory nesting is deeper than {MAX_DEPTH} levels, refusing to continue",
            src.join(rel).display()
        ));
    }
    let dir = src.join(rel);
    let reader = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let mut children: Vec<PathBuf> = Vec::new();
    for item in reader {
        let item = item.map_err(|e| format!("{}: {e}", dir.display()))?;
        children.push(rel.join(item.file_name()));
    }
    children.sort();

    for child_rel in children {
        let from = src.join(&child_rel);
        let to = dst.join(&child_rel);
        let meta = fs::symlink_metadata(&from).map_err(|e| format!("{}: {e}", from.display()))?;
        let mode = meta.permissions().mode();
        let mtime = meta.modified().ok();
        let file_type = meta.file_type();

        if file_type.is_symlink() {
            let target = fs::read_link(&from).map_err(|e| format!("{}: {e}", from.display()))?;
            std::os::unix::fs::symlink(&target, &to)
                .map_err(|e| format!("{}: {e}", to.display()))?;
            copy_times(&from, &to)?;
            out.push(Entry {
                rel: child_rel,
                kind: Kind::Symlink { target },
                mode,
                mtime,
            });
        } else if file_type.is_dir() {
            fs::create_dir(&to).map_err(|e| format!("{}: {e}", to.display()))?;
            out.push(Entry {
                rel: child_rel.clone(),
                kind: Kind::Dir,
                mode,
                mtime,
            });
            copy_dir(src, dst, &child_rel, depth + 1, out, links, progress)?;
            fs::set_permissions(&to, fs::Permissions::from_mode(mode & 0o7777))
                .map_err(|e| format!("{}: {e}", to.display()))?;
            // After the children, because writing them moves the directory's
            // own modification time.
            copy_times(&from, &to)?;
        } else if file_type.is_file() {
            let identity = (meta.dev(), meta.ino());
            // A file with more than one name is copied once and linked again,
            // so the copy holds one file with two names, as the source did.
            let digest = if meta.nlink() > 1 {
                match links.get(&identity) {
                    Some((first, digest)) => {
                        fs::hard_link(first, &to).map_err(|e| {
                            format!("{} -> {}: {e}", first.display(), to.display())
                        })?;
                        *digest
                    }
                    None => {
                        let digest = copy_file(&from, &to)?;
                        links.insert(identity, (to.clone(), digest));
                        digest
                    }
                }
            } else {
                copy_file(&from, &to)?
            };
            copy_times(&from, &to)?;
            progress(&child_rel, meta.len());
            out.push(Entry {
                rel: child_rel,
                kind: Kind::File {
                    size: meta.len(),
                    digest: Some(digest),
                    links: meta.nlink(),
                    identity,
                    allocated: meta.blocks() * 512,
                },
                mode,
                mtime,
            });
        } else {
            return Err(format!(
                "{}: not a regular file, directory or symlink. LibraryBridge will not copy \
                 sockets, pipes or device nodes, so this tree needs manual attention.",
                from.display()
            ));
        }
    }
    Ok(())
}

/// Carry a file's access and modification times across, to the precision the
/// destination filesystem keeps. Applied without following links, so a
/// symlink gets its own times rather than its target's.
fn copy_times(from: &Path, to: &Path) -> Result<(), String> {
    use rustix::fs::{utimensat, AtFlags, Timestamps};

    let meta = fs::symlink_metadata(from).map_err(|e| format!("{}: {e}", from.display()))?;
    let stamp = |time: std::io::Result<std::time::SystemTime>| match time {
        Ok(time) => match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(since) => rustix::fs::Timespec {
                tv_sec: since.as_secs() as i64,
                tv_nsec: since.subsec_nanos() as _,
            },
            // Before 1970. Rare, and not worth failing a repair over.
            Err(_) => rustix::fs::Timespec { tv_sec: 0, tv_nsec: 0 },
        },
        Err(_) => rustix::fs::Timespec { tv_sec: 0, tv_nsec: 0 },
    };

    let times = Timestamps {
        last_access: stamp(meta.accessed()),
        last_modification: stamp(meta.modified()),
    };
    utimensat(rustix::fs::CWD, to, &times, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|e| format!("{}: could not set timestamps: {e}", to.display()))
}

fn copy_file(from: &Path, to: &Path) -> Result<[u8; 32], String> {
    let mut input = fs::File::open(from).map_err(|e| format!("{}: {e}", from.display()))?;
    let mut output = fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUFFER];
    loop {
        let read = input
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", from.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        output
            .write_all(&buf[..read])
            .map_err(|e| format!("{}: {e}", to.display()))?;
    }
    output
        .flush()
        .map_err(|e| format!("{}: {e}", to.display()))?;
    output
        .sync_all()
        .map_err(|e| format!("{}: {e}", to.display()))?;

    let mode = fs::symlink_metadata(from)
        .map_err(|e| format!("{}: {e}", from.display()))?
        .permissions()
        .mode();
    fs::set_permissions(to, fs::Permissions::from_mode(mode & 0o7777))
        .map_err(|e| format!("{}: {e}", to.display()))?;
    Ok(hasher.finish())
}

/// Compare a copy against the manifest recorded while it was written.
/// Contents, sizes, link text and the shape of the tree must all match.
pub fn verify_against(expected: &Manifest, actual: &Manifest) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();

    if expected.entries.len() != actual.entries.len() {
        problems.push(format!(
            "entry count differs: source has {}, copy has {}",
            expected.entries.len(),
            actual.entries.len()
        ));
    }

    let mut actual_by_path: std::collections::HashMap<&Path, &Entry> =
        std::collections::HashMap::new();
    for entry in &actual.entries {
        actual_by_path.insert(entry.rel.as_path(), entry);
    }

    for want in &expected.entries {
        let Some(got) = actual_by_path.remove(want.rel.as_path()) else {
            problems.push(format!("missing from the copy: {}", want.rel.display()));
            continue;
        };
        // Permission bits matter: Proton writes into these trees, and a
        // directory copied without its write bit would break the prefix.
        if !matches!(want.kind, Kind::Symlink { .. }) && want.mode & 0o7777 != got.mode & 0o7777 {
            problems.push(format!(
                "{}: permissions are {:o} in the source and {:o} in the copy",
                want.rel.display(),
                want.mode & 0o7777,
                got.mode & 0o7777
            ));
        }

        match (&want.kind, &got.kind) {
            (Kind::Dir, Kind::Dir) => {}
            (
                Kind::File {
                    size: want_size,
                    digest: want_digest,
                    ..
                },
                Kind::File {
                    size: got_size,
                    digest: got_digest,
                    ..
                },
            ) => {
                if want_size != got_size {
                    problems.push(format!(
                        "{}: size {want_size} in the source, {got_size} in the copy",
                        want.rel.display()
                    ));
                } else if want_digest != got_digest {
                    let want_hex = want_digest.map(|d| hex(&d)).unwrap_or_default();
                    let got_hex = got_digest.map(|d| hex(&d)).unwrap_or_default();
                    problems.push(format!(
                        "{}: contents differ (source {want_hex}, copy {got_hex})",
                        want.rel.display()
                    ));
                }
            }
            (
                Kind::Symlink {
                    target: want_target,
                },
                Kind::Symlink { target: got_target },
            ) => {
                if want_target != got_target {
                    problems.push(format!(
                        "{}: link points at {} in the source and {} in the copy",
                        want.rel.display(),
                        want_target.display(),
                        got_target.display()
                    ));
                }
            }
            (want_kind, got_kind) => problems.push(format!(
                "{}: {} in the source, {} in the copy",
                want.rel.display(),
                want_kind.label(),
                got_kind.label()
            )),
        }
    }

    for leftover in actual_by_path.keys() {
        problems.push(format!(
            "unexpected extra entry in the copy: {}",
            leftover.display()
        ));
    }

    // Files that shared their contents in the source must still share them
    // in the copy. Matching bytes is not the same as still being one file.
    problems.extend(compare_link_groups(expected, actual));

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// Group entries by the file they are, then check the copy groups the same
/// paths together. A relationship, not a checksum.
fn compare_link_groups(expected: &Manifest, actual: &Manifest) -> Vec<String> {
    fn groups(manifest: &Manifest) -> Vec<Vec<&Path>> {
        let mut by_file: HashMap<(u64, u64), Vec<&Path>> = HashMap::new();
        for entry in &manifest.entries {
            if let Kind::File { identity, links, .. } = &entry.kind {
                if *links > 1 {
                    by_file.entry(*identity).or_default().push(&entry.rel);
                }
            }
        }
        let mut shapes: Vec<Vec<&Path>> = by_file
            .into_values()
            .map(|mut paths| {
                paths.sort();
                paths
            })
            .collect();
        shapes.sort();
        shapes
    }

    let wanted = groups(expected);
    let got = groups(actual);
    if wanted == got {
        return Vec::new();
    }
    vec![format!(
        "shared files were not reproduced as shared: the source has {} group(s) of names \
         pointing at one file, the copy has {}",
        wanted.len(),
        got.len()
    )]
}

/// Detect a source that changed while it was being copied. Compares shape,
/// sizes, link text and modification times, without rereading file contents.
pub fn changed_since(before: &Manifest, after: &Manifest) -> Vec<String> {
    let mut changes = Vec::new();
    let mut after_by_path: std::collections::HashMap<&Path, &Entry> =
        std::collections::HashMap::new();
    for entry in &after.entries {
        after_by_path.insert(entry.rel.as_path(), entry);
    }

    for want in &before.entries {
        let Some(got) = after_by_path.remove(want.rel.as_path()) else {
            changes.push(format!(
                "disappeared during the copy: {}",
                want.rel.display()
            ));
            continue;
        };
        let same_kind = match (&want.kind, &got.kind) {
            (Kind::Dir, Kind::Dir) => true,
            (Kind::File { size: a, .. }, Kind::File { size: b, .. }) => a == b,
            (Kind::Symlink { target: a }, Kind::Symlink { target: b }) => a == b,
            _ => false,
        };
        if !same_kind {
            changes.push(format!("changed during the copy: {}", want.rel.display()));
        } else if want.mtime != got.mtime {
            changes.push(format!("modified during the copy: {}", want.rel.display()));
        }
    }
    for leftover in after_by_path.keys() {
        changes.push(format!("appeared during the copy: {}", leftover.display()));
    }
    changes
}

/// A digest over everything a verified manifest establishes: which entries
/// exist, what they are, how big they are, and what they contain.
///
/// Recovery compares this against a destination to decide whether it is the
/// copy this tool checked, without reading the source again.
pub fn manifest_digest(manifest: &Manifest) -> String {
    let mut entries: Vec<&Entry> = manifest.entries.iter().collect();
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));

    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update(entry.rel.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        match &entry.kind {
            Kind::Dir => hasher.update(b"d"),
            Kind::Symlink { target } => {
                hasher.update(b"l");
                hasher.update(target.to_string_lossy().as_bytes());
            }
            Kind::File { size, digest, .. } => {
                hasher.update(b"f");
                hasher.update(&size.to_le_bytes());
                if let Some(digest) = digest {
                    hasher.update(digest);
                }
            }
        }
        hasher.update(b"\0");
    }
    hex(&hasher.finish())
}

/// Relative symlinks that point out of the tree.
///
/// A relative link resolves against the directory holding it. Copy the tree
/// somewhere else and a link that pointed outside it now resolves somewhere
/// else again, so preserving the link text exactly, which the copier does,
/// still changes what the link means. Absolute links keep their meaning and
/// are left alone, which is why `dosdevices/z:` is not a problem.
pub fn escaping_relative_links(root: &Path, manifest: &Manifest) -> Vec<(PathBuf, PathBuf)> {
    let root = lexically_normal(root);
    let mut escaping = Vec::new();
    for entry in &manifest.entries {
        let Kind::Symlink { target } = &entry.kind else {
            continue;
        };
        if target.is_absolute() {
            continue;
        }
        let holder = root.join(&entry.rel);
        let parent = holder.parent().unwrap_or(&root);
        let resolved = lexically_normal(&parent.join(target));
        if !resolved.starts_with(&root) {
            escaping.push((entry.rel.clone(), target.clone()));
        }
    }
    escaping
}

/// Resolve `.` and `..` without touching the filesystem. The link target may
/// not exist, and canonicalizing would follow links we must not follow.
fn lexically_normal(path: &Path) -> PathBuf {
    let mut parts: Vec<std::path::Component> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(parts.last(), Some(std::path::Component::Normal(_))) {
                    parts.pop();
                } else {
                    parts.push(component);
                }
            }
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

/// Prove that this directory's filesystem can hold the symlink the repair
/// depends on. Creates one link, reads it back, and removes only what it made.
pub fn probe_symlink_support(dir: &Path) -> Result<(), String> {
    // Unique per run, and never written over something already there. The
    // probe removes only the link it created, because a name is not proof of
    // ownership.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let name = format!(".librarybridge-probe-{}-{stamp}", std::process::id());
    let probe = dir.join(&name);
    if fs::symlink_metadata(&probe).is_ok() {
        return Err(format!(
            "{}: something is already at the name this check needs. Remove it yourself and \
             try again; LibraryBridge will not delete a file it did not create.",
            probe.display()
        ));
    }

    let sentinel = Path::new("librarybridge-probe-target");
    std::os::unix::fs::symlink(sentinel, &probe).map_err(|e| {
        format!(
            "cannot create a symlink in {}: {e}. This filesystem cannot support the repair.",
            dir.display()
        )
    })?;

    let result = (|| {
        let meta = fs::symlink_metadata(&probe).map_err(|e| format!("{}: {e}", probe.display()))?;
        if !meta.file_type().is_symlink() {
            return Err(format!(
                "{}: the filesystem accepted a symlink but did not store it as one",
                probe.display()
            ));
        }
        let read_back = fs::read_link(&probe).map_err(|e| format!("{}: {e}", probe.display()))?;
        if read_back != sentinel {
            return Err(format!(
                "{}: symlink read back as {} instead of {}",
                probe.display(),
                read_back.display(),
                sentinel.display()
            ));
        }
        Ok(())
    })();

    // Remove it only if it is still the link this function made.
    if fs::read_link(&probe).map(|text| text == sentinel).unwrap_or(false) {
        let _ = fs::remove_file(&probe);
    }
    result
}

/// Total size of a tree, following nothing.
pub fn tree_size(root: &Path) -> Result<u64, String> {
    Ok(inventory(root, false)?.bytes)
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
