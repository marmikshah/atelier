use atelier_core::document::{Document, MAX_DOCUMENT_CEL_PIXELS};
use atelier_core::source::{Asset, MAX_SOURCE_BYTES, compile};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

/// One recipe and the exact bytes of all its image resources.
pub struct Source {
    pub(super) asset: Asset,
    pub(super) text: String,
    pub(super) files: BTreeMap<String, Vec<u8>>,
}
impl Source {
    /// Capture an older native snapshot that has no replay construction.
    pub fn from_native(path: &Path) -> Result<Self, String> {
        if !fs::symlink_metadata(path)
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            return Err("native snapshot must be a real directory".into());
        }
        let doc = Document::load(path)?;
        super::edit::current(&doc, path, None, None)
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let path = manifest_path(path);
        let text =
            String::from_utf8(read_file(&path, MAX_SOURCE_BYTES)?).map_err(|e| e.to_string())?;
        let asset: Asset =
            super::parse::decode(&text).map_err(|e| format!("invalid recipe: {e}"))?;
        asset.validate()?;
        let root = path.parent().unwrap_or(Path::new("."));
        let mut files = BTreeMap::new();
        let mut total = text.len() as u64;
        for name in resource_names(&asset) {
            let bytes = read_file(&resource_path(root, name)?, 256 * 1024 * 1024)?;
            total += bytes.len() as u64;
            if total > 256 * 1024 * 1024 {
                return Err("recipe resources exceed 256 MiB".into());
            }
            files.insert(name.to_owned(), bytes);
        }
        Self::from_parts(text, asset, files)
    }
    pub(super) fn from_parts(
        text: String,
        asset: Asset,
        mut files: BTreeMap<String, Vec<u8>>,
    ) -> Result<Self, String> {
        if text.len() as u64 > MAX_SOURCE_BYTES {
            return Err("recipe manifest exceeds 16 MiB".into());
        }
        asset.validate()?;
        if text.len() as u64 + files.values().map(|v| v.len() as u64).sum::<u64>()
            > 256 * 1024 * 1024
        {
            return Err("recipe resources exceed 256 MiB".into());
        }
        let used = resource_names(&asset);
        files.retain(|name, _| used.contains(name.as_str()));
        let mut pixels = 0;
        for name in used {
            let data = files
                .get(name)
                .ok_or_else(|| format!("missing image '{name}'"))?;
            let (w, h) =
                image::ImageReader::with_format(Cursor::new(data), image::ImageFormat::Png)
                    .into_dimensions()
                    .map_err(|e| e.to_string())?;
            pixels += w as u64 * h as u64;
            if w == 0 || h == 0 || pixels > MAX_DOCUMENT_CEL_PIXELS {
                return Err("recipe image pixel budget exceeded".into());
            }
            let mut reader =
                image::ImageReader::with_format(Cursor::new(data), image::ImageFormat::Png);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(w);
            limits.max_image_height = Some(h);
            limits.max_alloc = Some(MAX_DOCUMENT_CEL_PIXELS * 4);
            reader.limits(limits);
            reader.decode().map_err(|e| e.to_string())?;
        }
        Ok(Self { asset, text, files })
    }
    pub fn compile(&self) -> Result<Document, String> {
        compile(&self.asset)
    }
    pub fn asset(&self) -> &Asset {
        &self.asset
    }
    pub fn encoded_bytes(&self) -> u64 {
        self.text.len() as u64 + self.files.values().map(|v| v.len() as u64).sum::<u64>()
    }
    pub(crate) fn resource_names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }
    pub(super) fn write_to(&self, root: &Path) -> Result<(), String> {
        fs::create_dir(root).map_err(|e| format!("cannot create recipe bundle: {e}"))?;
        fs::write(root.join("recipe.atelier"), &self.text).map_err(|e| e.to_string())?;
        for (name, bytes) in &self.files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().ok_or("resource has no parent")?)
                .map_err(|e| e.to_string())?;
            fs::write(path, bytes).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
fn resource_names(asset: &Asset) -> BTreeSet<&str> {
    asset.reference.as_deref().into_iter().collect()
}
pub(super) fn manifest_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("recipe.atelier")
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
