//! The Image Gallery's file list (Autopsy `ImageGalleryController`'s
//! drawable files): pictures by detected MIME type, or by extension while
//! file type detection has not run, without files known from hash sets
//! and NTFS system files (`$UpCase` starts like an icon, and Tika's magic
//! says so).
//!
//! Rows are kept small (no times, no full record): a gallery of a large
//! case lists hundreds of thousands of pictures.

use crate::{CaseDb, Known, Result};

/// Extensions taken as pictures when a file has no detected MIME type
/// (Autopsy `ImageUtils` supported extensions that `image` decodes).
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "tif", "tiff", "webp", "ico", "tga",
];

/// A picture for the gallery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GalleryFile {
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
}

impl CaseDb {
    /// Every picture in the case, ordered by data source, folder and name.
    pub fn gallery_files(&self) -> Result<Vec<GalleryFile>> {
        let exts = IMAGE_EXTENSIONS
            .iter()
            .map(|e| format!("'{e}'"))
            .collect::<Vec<_>>()
            .join(",");
        let known = Known::Known as i64;
        let cols =
            "id, ds_id, fs_id, parent_path, name, size, name_alloc AND meta_alloc AS alloc, md5";
        let common = format!(
            "kind = 1 AND size > 0 AND stream IS NULL AND known != {known} \
             AND NOT (parent_path = '/' AND name LIKE '$%')"
        );
        let sql = format!(
            "SELECT x.id, x.ds_id, x.parent_path, x.name, x.size, x.alloc, x.md5, v.addr FROM ( \
               SELECT {cols} FROM files \
                WHERE mime >= 'image/' AND mime < 'image0' AND {common} \
               UNION ALL \
               SELECT {cols} FROM files \
                WHERE mime IS NULL AND extension IN ({exts}) AND {common} \
             ) x \
             LEFT JOIN file_systems f ON f.id = x.fs_id \
             LEFT JOIN volumes v ON v.id = f.volume_id \
             ORDER BY 2, 8, 3, 4"
        );
        self.read(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map([], |r| {
                    let md5: Option<Vec<u8>> = r.get(6)?;
                    Ok(GalleryFile {
                        id: r.get(0)?,
                        ds_id: r.get(1)?,
                        volume: r.get(7)?,
                        parent_path: r.get(2)?,
                        name: r.get(3)?,
                        size: r.get::<_, i64>(4)?.cast_unsigned(),
                        deleted: !r.get::<_, bool>(5)?,
                        md5: md5.and_then(|m| <[u8; 16]>::try_from(m).ok()),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}
