use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

use atelier_core::document::{Document, MAX_DOCUMENT_CEL_PIXELS};
use atelier_core::source::{Asset, MAX_SOURCE_BYTES, compile};
use image::RgbaImage;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Parsed source plus an immutable snapshot of every declared dependency.
/// Hashing and compilation use these same bytes, including external edits.
pub struct Source {
    pub(super) asset: Asset,
    pub(super) revision: String,
    pub(super) text: String,
    pub(super) files: BTreeMap<String, Vec<u8>>,
    images: BTreeMap<String, RgbaImage>,
}

impl Source {
    /// Read `source.toml`, or a bundle directory containing it.
    pub fn load(path: &Path) -> Result<Self, String> {
        let path = manifest_path(path);
        let bytes = read_file(&path, MAX_SOURCE_BYTES)?;
        let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
        Self::from_text(text, path.parent().unwrap_or(Path::new(".")))
    }

    pub(super) fn from_text(text: String, root: &Path) -> Result<Self, String> {
        if text.len() as u64 > MAX_SOURCE_BYTES {
            return Err("source manifest exceeds 16 MiB".into());
        }
        let asset: Asset =
            toml_edit::de::from_str(&text).map_err(|e| format!("invalid source: {e}"))?;
        asset.validate()?;
        let mut files = BTreeMap::new();
        let mut images = BTreeMap::new();
        let mut pixels = 0;
        let mut bytes = text.len() as u64;
        let paths: BTreeSet<_> = asset
            .parts
            .values()
            .filter_map(|p| p.image.as_ref())
            .chain(asset.reference.iter())
            .collect();
        for name in paths {
            let data = read_file(&resource_path(root, name)?, 256 * 1024 * 1024)?;
            bytes += data.len() as u64;
            if bytes > 256 * 1024 * 1024 {
                return Err("source resources exceed 256 MiB".into());
            }
            let reader =
                image::ImageReader::with_format(Cursor::new(&data), image::ImageFormat::Png);
            let (w, h) = reader
                .into_dimensions()
                .map_err(|e| format!("'{name}': {e}"))?;
            for part in asset
                .parts
                .values()
                .filter(|p| p.image.as_ref() == Some(name))
            {
                if part.origin[0] as u64 + w as u64 > part.size[0] as u64
                    || part.origin[1] as u64 + h as u64 > part.size[1] as u64
                {
                    return Err(format!(
                        "'{name}': image and origin exceed the part's logical size"
                    ));
                }
            }
            pixels += w as u64 * h as u64;
            if w == 0 || h == 0 || pixels > MAX_DOCUMENT_CEL_PIXELS {
                return Err(format!("'{name}': source image pixel budget exceeded"));
            }
            let mut reader =
                image::ImageReader::with_format(Cursor::new(&data), image::ImageFormat::Png);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(w);
            limits.max_image_height = Some(h);
            limits.max_alloc = Some(MAX_DOCUMENT_CEL_PIXELS * 4);
            reader.limits(limits);
            images.insert(
                name.clone(),
                reader
                    .decode()
                    .map_err(|e| format!("'{name}': {e}"))?
                    .to_rgba8(),
            );
            files.insert(name.clone(), data);
        }
        let mut digest = Sha256::new();
        digest.update(b"atelier-source-1-renderer-1\0");
        hash_entry(&mut digest, "source.toml", text.as_bytes());
        for (name, bytes) in &files {
            hash_entry(&mut digest, name, bytes);
        }
        let revision = format!("{:x}", digest.finalize());
        Ok(Self {
            asset,
            text,
            files,
            images,
            revision,
        })
    }

    pub fn compile(&self) -> Result<Document, String> {
        compile(&self.asset, &self.images)
    }

    pub fn asset(&self) -> &Asset {
        &self.asset
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn encoded_bytes(&self) -> u64 {
        self.text.len() as u64 + self.files.values().map(|v| v.len() as u64).sum::<u64>()
    }

    pub(crate) fn resource_names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    /// Small structural view; a selected part returns only its own definition.
    pub fn inspect(&self, part: Option<&str>) -> Result<Value, String> {
        if let Some(name) = part {
            let part = self
                .asset
                .parts
                .get(name)
                .ok_or_else(|| format!("unknown part '{name}'"))?;
            let inks: BTreeMap<_, _> = part
                .legend
                .values()
                .filter_map(|name| self.asset.inks.get_key_value(name))
                .collect();
            return Ok(
                json!({"revision": self.revision, "name": name, "part": part, "inks": inks}),
            );
        }
        Ok(
            json!({"revision": self.revision, "name": self.asset.name, "canvas": self.asset.canvas,
            "frames": self.asset.frames, "layers": self.asset.layers, "cels": self.asset.cels,
            "manifest_bytes": self.text.len(), "resource_bytes": self.files.values().map(Vec::len).sum::<usize>(),
            "parts": self.asset.parts.iter().map(|(name,p)| json!({"name": name, "kind": p.kind(), "size": p.size,
                "dependencies": p.dependencies(), "resource_bytes": p.image.as_ref().map(|n| self.files[n].len())})).collect::<Vec<_>>() }),
        )
    }

    pub(super) fn write_to(&self, root: &Path) -> Result<(), String> {
        fs::create_dir(root).map_err(|e| format!("cannot create source bundle: {e}"))?;
        fs::write(root.join("source.toml"), &self.text).map_err(|e| e.to_string())?;
        for (name, bytes) in &self.files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().ok_or("resource has no parent")?)
                .map_err(|e| e.to_string())?;
            fs::write(path, bytes).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

fn hash_entry(hash: &mut Sha256, name: &str, bytes: &[u8]) {
    hash.update((name.len() as u64).to_le_bytes());
    hash.update(name.as_bytes());
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

pub(super) fn manifest_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("source.toml")
    } else {
        path.into()
    }
}

pub(super) fn resource_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = Path::new(name);
    if name.is_empty()
        || !name.is_ascii()
        || name.len() > 200
        || !name.ends_with(".png")
        || name.contains('\\')
        || !path.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(format!(
            "resource '{name}' must be a relative path inside the bundle"
        ));
    }
    let mut result = root.to_path_buf();
    for component in path.components() {
        result.push(component);
        let meta = fs::symlink_metadata(&result).map_err(|e| format!("resource '{name}': {e}"))?;
        if meta.file_type().is_symlink() {
            return Err(format!("resource '{name}': symlinks are refused"));
        }
    }
    Ok(result)
}

pub(super) fn read_file(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > limit {
        return Err(format!(
            "{} must be a regular file of at most {limit} bytes",
            path.display()
        ));
    }
    let mut options = File::options();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0o400000);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let open = file.metadata().map_err(|e| e.to_string())?;
        if open.dev() != meta.dev() || open.ino() != meta.ino() {
            return Err("source changed while opening".into());
        }
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("source grew beyond its size limit".into());
    }
    Ok(bytes)
}

pub(super) fn sync_tree(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            sync_tree(&path)?;
        } else {
            File::open(path)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
    }
    File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
