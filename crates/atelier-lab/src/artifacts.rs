//! Content-addressed artifact storage on the local filesystem (lab.md item
//! 10). Renders and other large/binary payloads live here, keyed by SHA-256;
//! episode events reference `{"sha256": ..., "kind": ...}` instead of
//! embedding bytes. Layout: `<root>/sha256/<2-hex-prefix>/<full-hash>` — the
//! prefix fan-out keeps a single directory from holding every artifact.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::storage::Storage;

/// The artifact directory inside an episode dir (lab.md item 7: a separate
/// artifact directory per episode).
pub const ARTIFACTS_DIR: &str = "artifacts";

/// What a stored blob IS — carried next to the hash in every reference so a
/// reader knows how to interpret the bytes without guessing from context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    RenderNative,
    RenderEnlarged,
    RenderGrayscale,
    RenderNotan,
    /// The episode's closing render — the replay gate compares against it.
    FinalImage,
}

/// A reference to a stored artifact: the event-log stand-in for the bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub sha256: String,
    pub kind: ArtifactKind,
}

/// Lowercase hex SHA-256 of `bytes` — the content key.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Local-filesystem [`Storage`] with the `sha256/<prefix>/<hash>` layout.
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    /// A store rooted at `root` (created if missing).
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|e| format!("cannot create {}: {e}", root.display()))?;
        Ok(ArtifactStore { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The file a hash maps to. Hashes arrive as untrusted strings (parsed
    /// from event logs) and are joined onto the store path, so validate the
    /// exact shape we mint — 64 lowercase hex — before touching disk; the
    /// same discipline atelier applies to document/checkpoint ids.
    fn path_for(&self, hash: &str) -> Result<PathBuf, String> {
        let valid = hash.len() == 64
            && hash
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        if !valid {
            return Err(format!("invalid artifact hash '{hash}'"));
        }
        Ok(self.root.join("sha256").join(&hash[..2]).join(hash))
    }
}

impl Storage for ArtifactStore {
    fn put(&self, bytes: &[u8]) -> Result<String, String> {
        let hash = sha256_hex(bytes);
        let path = self.path_for(&hash)?;
        if path.exists() {
            // Content-addressed dedupe: the bytes are already stored.
            return Ok(hash);
        }
        let parent = path.parent().unwrap();
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        // Temp file + same-dir rename: a crash mid-write leaves a torn tmp,
        // never a truncated artifact under a valid hash.
        let tmp = parent.join(format!(".{hash}.tmp"));
        std::fs::write(&tmp, bytes).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| format!("cannot store {}: {e}", path.display()))?;
        Ok(hash)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        let path = self.path_for(key)?;
        std::fs::read(&path)
            .map_err(|e| format!("cannot read artifact {key} ({}): {e}", path.display()))
    }

    fn exists(&self, key: &str) -> bool {
        self.path_for(key).is_ok_and(|p| p.is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (PathBuf, ArtifactStore) {
        let dir = std::env::temp_dir().join(format!(
            "atelier-lab-artifacts-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let s = ArtifactStore::new(&dir).unwrap();
        (dir, s)
    }

    #[test]
    fn put_get_roundtrip_and_dedupe() {
        let (dir, s) = store("roundtrip");
        let a = s.put(b"hello pixels").unwrap();
        let b = s.put(b"hello pixels").unwrap();
        assert_eq!(a, b, "identical bytes dedupe to one hash");
        assert_eq!(a.len(), 64);
        assert!(s.exists(&a));
        assert_eq!(s.get(&a).unwrap(), b"hello pixels");
        let other = s.put(b"different").unwrap();
        assert_ne!(a, other);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn layout_is_sha256_prefix_hash() {
        let (dir, s) = store("layout");
        let hash = s.put(b"x").unwrap();
        let expected = dir.join("sha256").join(&hash[..2]).join(&hash);
        assert!(expected.is_file(), "artifact at {}", expected.display());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_and_malformed_keys_are_loud() {
        let (dir, s) = store("missing");
        let missing = "a".repeat(64);
        assert!(!s.exists(&missing));
        assert!(s.get(&missing).is_err());
        // A traversal-shaped key must be rejected before any path join.
        for bad in ["../etc/passwd", &"A".repeat(64), "short", &"g".repeat(64)] {
            assert!(!s.exists(bad), "{bad}");
            assert!(s.get(bad).is_err(), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
