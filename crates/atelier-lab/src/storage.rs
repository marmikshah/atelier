//! Byte storage behind a deliberately tiny trait (lab.md A3): the local
//! filesystem impl ([`crate::ArtifactStore`]) is today's, an S3 impl is the
//! future swap. Three methods — nothing to over-design — so the swap touches
//! one file and no episode code.

/// Content-addressed byte storage: `put` keys the bytes by their content
/// (the local impl uses SHA-256), so identical payloads dedupe naturally and
/// a returned key is a integrity check on every later `get`.
pub trait Storage {
    /// Store `bytes`, returning the content key they were stored under.
    fn put(&self, bytes: &[u8]) -> Result<String, String>;
    /// Fetch the bytes stored under `key`. A missing key is an error, never
    /// a silent empty read — a dangling artifact reference means the store
    /// lost data or the key was wrong, and both must be loud.
    fn get(&self, key: &str) -> Result<Vec<u8>, String>;
    /// True when `key` is present. Existence is checked before reads that
    /// would otherwise fail mid-report (e.g. replay with pruned artifacts).
    fn exists(&self, key: &str) -> bool;
}
