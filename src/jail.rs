//! The folder Athena was given, and the rule that keeps it there.
//!
//! Every path Athena asks about is resolved against the folder and checked
//! *after* following `..`, symbolic links and (on Windows) junctions, drive
//! letters and upper/lower case. Anything that ends up outside is refused.
//! This is enforced here, on the machine being accessed, so it holds whatever
//! the server or the model asks for.

use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub struct Jail {
    /// The folder, fully resolved.
    root: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The path leads outside the folder.
    Outside,
    /// The path itself is malformed (empty, NUL bytes).
    Invalid,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Outside => write!(f, "that path is outside the folder Athena was given access to"),
            Refusal::Invalid => write!(f, "that isn't a valid path"),
        }
    }
}

impl std::error::Error for Refusal {}

impl Jail {
    pub fn new(folder: &Path) -> io::Result<Jail> {
        let root = dunce(std::fs::canonicalize(folder)?);
        if !root.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "not a folder"));
        }
        Ok(Jail { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a path Athena gave (relative to the folder, or absolute) to the real
    /// path on disk, or refuse it. Works for paths that don't exist yet (a new
    /// file): the deepest part that exists is resolved, and the rest can't contain
    /// links because it doesn't exist.
    pub fn resolve(&self, asked: &str) -> Result<PathBuf, Refusal> {
        if asked.is_empty() || asked.contains('\0') {
            return Err(Refusal::Invalid);
        }
        let asked = Path::new(asked);
        let joined = if asked.is_absolute() || has_prefix(asked) { asked.to_path_buf() } else { self.root.join(asked) };

        // Walk up to the deepest part that exists and resolve it (this follows every link on the way).
        let mut existing = joined.clone();
        let mut rest: Vec<std::ffi::OsString> = Vec::new();
        let resolved_base = loop {
            match std::fs::canonicalize(&existing) {
                Ok(p) => break dunce(p),
                Err(_) => {
                    let name = existing.file_name().map(|n| n.to_os_string());
                    match (name, existing.parent()) {
                        (Some(n), Some(parent)) => {
                            rest.push(n);
                            existing = parent.to_path_buf();
                        }
                        // ".." or a root that doesn't exist: resolve lexically from what we have.
                        _ => break lexical(&existing),
                    }
                }
            }
        };

        // Re-attach the parts that don't exist yet; ".." among them is resolved lexically,
        // which is safe because nothing below an existing folder that doesn't exist can be a link.
        let mut full = resolved_base;
        for part in rest.iter().rev() {
            if part == ".." {
                full.pop();
            } else if part != "." {
                full.push(part);
            }
        }
        if self.contains(&full) {
            Ok(full)
        } else {
            Err(Refusal::Outside)
        }
    }

    /// Whether a fully resolved path is the folder or inside it.
    pub fn contains(&self, path: &Path) -> bool {
        let mut p = path.components();
        for r in self.root.components() {
            match p.next() {
                Some(c) if same_component(c, r) => {}
                _ => return false,
            }
        }
        true
    }

    /// A path for showing to Athena: relative to the folder, with forward slashes.
    pub fn display(&self, path: &Path) -> String {
        match path.strip_prefix(&self.root) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => path.to_string_lossy().into_owned(),
        }
    }
}

/// Windows: `C:foo` and `\\server\share` paths carry a prefix even when not "absolute".
fn has_prefix(p: &Path) -> bool {
    matches!(p.components().next(), Some(Component::Prefix(_)))
}

/// Resolve `.` and `..` without touching the disk (for paths that don't exist at all).
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(windows)]
fn same_component(a: Component, b: Component) -> bool {
    a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
}
#[cfg(not(windows))]
fn same_component(a: Component, b: Component) -> bool {
    a == b
}

/// Windows canonical paths start with `\\?\`; compare and show them without it.
#[cfg(windows)]
fn dunce(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}
#[cfg(not(windows))]
fn dunce(p: PathBuf) -> PathBuf {
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup() -> (tempfile::TempDir, Jail, PathBuf) {
        let base = tempfile::tempdir().unwrap();
        let folder = base.path().join("project");
        fs::create_dir_all(folder.join("src")).unwrap();
        fs::write(folder.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(base.path().join("secret.txt"), "outside").unwrap();
        let jail = Jail::new(&folder).unwrap();
        let outside = base.path().to_path_buf();
        (base, jail, outside)
    }

    #[test]
    fn paths_inside_are_allowed_existing_or_new() {
        let (_b, jail, _o) = setup();
        assert!(jail.resolve("src/main.rs").unwrap().ends_with("src/main.rs"));
        assert!(jail.resolve("./src/../src/main.rs").unwrap().ends_with("src/main.rs"));
        assert!(jail.resolve("new/dir/file.txt").unwrap().ends_with("new/dir/file.txt"));
        assert_eq!(jail.resolve(".").unwrap(), jail.root());
        let abs = jail.root().join("src/main.rs");
        assert!(jail.resolve(abs.to_str().unwrap()).is_ok());
    }

    #[test]
    fn dot_dot_and_absolute_paths_outside_are_refused() {
        let (_b, jail, outside) = setup();
        assert_eq!(jail.resolve("../secret.txt"), Err(Refusal::Outside));
        assert_eq!(jail.resolve("src/../../secret.txt"), Err(Refusal::Outside));
        assert_eq!(jail.resolve("new/../../../x"), Err(Refusal::Outside));
        assert_eq!(jail.resolve(outside.join("secret.txt").to_str().unwrap()), Err(Refusal::Outside));
        assert_eq!(jail.resolve("/etc/passwd"), Err(Refusal::Outside));
        assert_eq!(jail.resolve(""), Err(Refusal::Invalid));
        assert_eq!(jail.resolve("a\0b"), Err(Refusal::Invalid));
    }

    #[test]
    fn a_sibling_folder_with_the_same_prefix_is_outside() {
        let (base, jail, _o) = setup();
        fs::create_dir_all(base.path().join("project-other")).unwrap();
        let sibling = base.path().join("project-other/x");
        assert_eq!(jail.resolve(sibling.to_str().unwrap()), Err(Refusal::Outside));
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_pointing_outside_are_refused() {
        let (_b, jail, outside) = setup();
        std::os::unix::fs::symlink(outside.join("secret.txt"), jail.root().join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, jail.root().join("door")).unwrap();
        assert_eq!(jail.resolve("link.txt"), Err(Refusal::Outside));
        assert_eq!(jail.resolve("door/secret.txt"), Err(Refusal::Outside));
        // Writing a new file through a linked folder is refused too.
        assert_eq!(jail.resolve("door/new.txt"), Err(Refusal::Outside));
        // A link that stays inside is fine.
        std::os::unix::fs::symlink(jail.root().join("src"), jail.root().join("code")).unwrap();
        assert!(jail.resolve("code/main.rs").is_ok());
    }
}
