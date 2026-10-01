//! Filesystem layout: config root resolution, paths, and slugification
//! (`docs/architecture/persistence-model.md` §1).

use std::io;
use std::path::{Path, PathBuf};

/// File name of the app-level pointer file directly under the root.
pub const POINTER_FILE_NAME: &str = "prismcast.toml";
/// File name of a profile inside `profiles/<slug>/`.
pub const PROFILE_FILE_NAME: &str = "profile.toml";
/// File name of a scene collection inside `collections/<slug>`.
pub const COLLECTION_FILE_NAME: &str = "collection.json";
/// Suffix of the last-known-good backup sibling.
pub const BACKUP_SUFFIX: &str = ".bak";

/// The resolved configuration root (`$XDG_CONFIG_HOME/prismcast` by default).
///
/// The root is resolved once at startup and injected everywhere; nothing reads
/// the environment mid-run, so tests can point a store at a tempdir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRoot {
    root: PathBuf,
}

impl ConfigRoot {
    /// Uses an explicit root directory (tests, embedding).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolves the default root: `$XDG_CONFIG_HOME/prismcast`, falling back
    /// to `~/.config/prismcast`. Panics never; if neither variable is set the
    /// root is the relative `prismcast` directory (callers in tests always
    /// inject an explicit root).
    pub fn resolve() -> Self {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|home| PathBuf::from(home).join(".config"))
            })
            .unwrap_or_default();
        Self::new(base.join("prismcast"))
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/prismcast.toml` — the pointer file.
    pub fn pointer_file(&self) -> PathBuf {
        self.root.join(POINTER_FILE_NAME)
    }

    /// `<root>/profiles/`.
    pub fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }

    /// `<root>/collections/`.
    pub fn collections_dir(&self) -> PathBuf {
        self.root.join("collections")
    }

    /// `<root>/profiles/<slug>/`.
    pub fn profile_dir(&self, slug: &str) -> PathBuf {
        self.profiles_dir().join(slug)
    }

    /// `<root>/profiles/<slug>/profile.toml`.
    pub fn profile_file(&self, slug: &str) -> PathBuf {
        self.profile_dir(slug).join(PROFILE_FILE_NAME)
    }

    /// `<root>/collections/<slug>/`.
    pub fn collection_dir(&self, slug: &str) -> PathBuf {
        self.collections_dir().join(slug)
    }

    /// `<root>/collections/<slug>/collection.json`.
    pub fn collection_file(&self, slug: &str) -> PathBuf {
        self.collection_dir(slug).join(COLLECTION_FILE_NAME)
    }

    /// Creates the root, `profiles/`, and `collections/` directories with
    /// mode `0700` (they sit next to files carrying stream keys).
    pub fn ensure_dirs(&self) -> io::Result<()> {
        for dir in [
            self.root.clone(),
            self.profiles_dir(),
            self.collections_dir(),
        ] {
            create_dir_private(&dir)?;
        }
        Ok(())
    }
}

/// Creates a directory (and parents) with mode `0700` on Unix.
pub fn create_dir_private(dir: &Path) -> io::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Slugifies an entity name into a directory-safe handle: lowercase ASCII,
/// spaces → `-`, every other character outside `[a-z0-9-_]` stripped. An
/// empty result becomes `"unnamed"`. Pure and total (persistence-model §1).
pub fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for ch in name.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-' {
            slug.push(ch);
        } else if ch == ' ' {
            slug.push('-');
        }
        // Anything else (punctuation, non-ASCII) is stripped.
    }
    if slug.is_empty() {
        "unnamed".to_string()
    } else {
        slug
    }
}

/// Picks a slug for `name` that does not collide with `existing`, suffixing
/// `-2`, `-3`, … on collision (persistence-model §1).
pub fn unique_slug<'a>(name: &str, existing: impl IntoIterator<Item = &'a String>) -> String {
    let base = slugify(name);
    let taken: Vec<&String> = existing.into_iter().collect();
    if !taken.iter().any(|s| s.as_str() == base) {
        return base;
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken.iter().any(|s| s.as_str() == candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// The `.bak` sibling path of a file.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut bak = path.as_os_str().to_owned();
    bak.push(BACKUP_SUFFIX);
    PathBuf::from(bak)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_rules() {
        assert_eq!(slugify("Twitch 1080p"), "twitch-1080p");
        assert_eq!(slugify("Development Stream"), "development-stream");
        assert_eq!(slugify("already-slugged_1"), "already-slugged_1");
        assert_eq!(slugify("Ümläut Çhars!"), "mlut-hars");
        assert_eq!(slugify("!!!"), "unnamed");
        assert_eq!(slugify(""), "unnamed");
        assert_eq!(slugify("a  b"), "a--b");
    }

    #[test]
    fn unique_slug_suffixes_collisions() {
        let existing = ["cam".to_string(), "cam-2".to_string()];
        assert_eq!(unique_slug("Cam", existing.iter()), "cam-3");
        assert_eq!(unique_slug("Other", existing.iter()), "other");
    }

    #[test]
    fn backup_path_is_sibling() {
        assert_eq!(
            backup_path(Path::new("/x/y/profile.toml")),
            PathBuf::from("/x/y/profile.toml.bak")
        );
    }

    #[test]
    fn config_root_paths() {
        let root = ConfigRoot::new("/tmp/cfg");
        assert_eq!(
            root.pointer_file(),
            PathBuf::from("/tmp/cfg/prismcast.toml")
        );
        assert_eq!(
            root.profile_file("p"),
            PathBuf::from("/tmp/cfg/profiles/p/profile.toml")
        );
        assert_eq!(
            root.collection_file("c"),
            PathBuf::from("/tmp/cfg/collections/c/collection.json")
        );
    }
}
