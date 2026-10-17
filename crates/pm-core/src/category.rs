//! File categories by extension (Autopsy's Data Source Summary slices,
//! Discovery result types, timeline file-type filter).

/// File categories (Autopsy's Data Source Summary pie-chart slices).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileCategory {
    Images,
    Videos,
    Audio,
    Documents,
    Executables,
    Other,
}

impl FileCategory {
    pub const ALL: [Self; 6] = [
        Self::Images,
        Self::Videos,
        Self::Audio,
        Self::Documents,
        Self::Executables,
        Self::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Images => "Images",
            Self::Videos => "Videos",
            Self::Audio => "Audio",
            Self::Documents => "Documents",
            Self::Executables => "Executables",
            Self::Other => "Other",
        }
    }

    /// Autopsy's slice colours (RGB).
    pub fn color(self) -> [u8; 3] {
        match self {
            Self::Images => [156, 39, 176],
            Self::Videos => [255, 235, 59],
            Self::Audio => [33, 150, 243],
            Self::Documents => [76, 175, 80],
            Self::Executables => [0, 188, 212],
            Self::Other => [121, 85, 72],
        }
    }

    /// Classifies by extension (MIME detection replaces this in M4).
    /// Extensions of each category (lower case, without the dot); every
    /// other extension is [`Self::Other`].
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Images => &[
                "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif", "ico",
                "psd", "cr2", "nef", "dng", "svg",
            ],
            Self::Videos => &[
                "mp4", "mov", "avi", "mkv", "wmv", "flv", "m4v", "3gp", "3g2", "webm", "mpg",
                "mpeg",
            ],
            Self::Audio => &[
                "mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "mid", "midi", "opus", "amr",
            ],
            Self::Documents => &[
                "doc", "docx", "xls", "xlsx", "ppt", "pptx", "pdf", "txt", "rtf", "odt", "ods",
                "odp", "csv", "html", "htm", "xml", "json", "md", "log", "css", "js", "wpd",
            ],
            Self::Executables => &[
                "exe", "dll", "sys", "com", "scr", "msi", "bat", "cmd", "ps1", "vbs", "elf", "so",
                "dylib", "sh",
            ],
            Self::Other => &[],
        }
    }

    pub fn from_name(name: &str) -> Self {
        let ext = extension(name).to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|c| c.extensions().contains(&ext.as_str()))
            .unwrap_or(Self::Other)
    }
}

/// Extension of a file name (without the dot), or `""`.
pub fn extension(name: &str) -> &str {
    let base = name.rsplit('/').next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.contains(':') => ext,
        _ => "",
    }
}
