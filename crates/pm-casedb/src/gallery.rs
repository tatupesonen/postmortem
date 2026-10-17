//! Files of one type for the Image Gallery (Autopsy `ImageGalleryController`'s
//! drawable files) and File Discovery: by detected MIME type, or by
//! extension while file type detection has not run, leaving out NTFS
//! system files (`$UpCase` starts like an icon, and Tika's magic says so)
//! and, optionally, files known from hash sets.
//!
//! Rows are kept small (no times, no full record): a large case has
//! hundreds of thousands of pictures.

use crate::{CaseDb, Known, Result};

/// Extensions taken as pictures when a file has no detected MIME type
/// (Autopsy `ImageUtils` supported extensions that `image` decodes).
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "tif", "tiff", "webp", "ico", "tga",
];

/// A file of the requested type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedFile {
    pub id: i64,
    pub ds_id: i64,
    /// Address of the file system's volume (Autopsy's `vol_N` path part).
    pub volume: Option<i64>,
    /// `/`-terminated folder.
    pub parent_path: String,
    pub name: String,
    pub size: u64,
    pub deleted: bool,
    pub md5: Option<[u8; 16]>,
    /// Modified time (Unix seconds).
    pub modified: Option<i64>,
}

impl CaseDb {
    /// Every picture in the case, ordered by data source, folder and name.
    pub fn gallery_files(&self) -> Result<Vec<TypedFile>> {
        self.files_of_type(Some("image/"), IMAGE_EXTENSIONS, true)
    }

    /// Regular, non-empty files whose MIME type starts with `mime_prefix`
    /// (`"type/"`), or — when the MIME type is unknown or no prefix is
    /// given — whose extension is in `extensions`; ordered by data source,
    /// volume, folder and name. `skip_known` leaves out files known from
    /// hash sets.
    pub fn files_of_type(
        &self,
        mime_prefix: Option<&str>,
        extensions: &[&str],
        skip_known: bool,
    ) -> Result<Vec<TypedFile>> {
        let exts = extensions
            .iter()
            .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(|e| format!("'{e}'"))
            .collect::<Vec<_>>()
            .join(",");
        let known = if skip_known {
            format!("AND known != {}", Known::Known as i64)
        } else {
            String::new()
        };
        let cols = "id, ds_id, fs_id, parent_path, name, size, name_alloc AND meta_alloc AS alloc, md5, mtime";
        let common = format!(
            "kind = 1 AND size > 0 AND stream IS NULL {known} \
             AND NOT (parent_path = '/' AND name LIKE '$%')"
        );
        let by_ext = if exts.is_empty() {
            "0".to_owned()
        } else {
            format!("extension IN ({exts})")
        };
        let inner = match mime_prefix {
            // `type/` .. `type0` is the prefix range ('0' sorts after '/').
            Some(p)
                if p.ends_with('/') && p.chars().all(|c| c.is_ascii_lowercase() || c == '/') =>
            {
                let upper = format!("{}0", p.trim_end_matches('/'));
                format!(
                    "SELECT {cols} FROM files WHERE mime >= '{p}' AND mime < '{upper}' AND {common} \
                     UNION ALL \
                     SELECT {cols} FROM files WHERE mime IS NULL AND {by_ext} AND {common}"
                )
            }
            _ => format!("SELECT {cols} FROM files WHERE {by_ext} AND {common}"),
        };
        let sql = format!(
            "SELECT x.id, x.ds_id, x.parent_path, x.name, x.size, x.alloc, x.md5, v.addr, x.mtime \
             FROM ({inner}) x \
             LEFT JOIN file_systems f ON f.id = x.fs_id \
             LEFT JOIN volumes v ON v.id = f.volume_id \
             ORDER BY 2, 8, 3, 4"
        );
        self.read(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map([], |r| {
                    let md5: Option<Vec<u8>> = r.get(6)?;
                    Ok(TypedFile {
                        id: r.get(0)?,
                        ds_id: r.get(1)?,
                        volume: r.get(7)?,
                        parent_path: r.get(2)?,
                        name: r.get(3)?,
                        size: r.get::<_, i64>(4)?.cast_unsigned(),
                        deleted: !r.get::<_, bool>(5)?,
                        md5: md5.and_then(|m| <[u8; 16]>::try_from(m).ok()),
                        modified: r.get(8)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}
