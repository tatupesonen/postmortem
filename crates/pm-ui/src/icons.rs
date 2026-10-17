//! Sidebar tree icons. Only glyphs present in egui's bundled fonts
//! (Ubuntu, Noto Emoji, emoji-icon-font, the proportional
//! family's fallback chain) are used, so nothing renders as
//! a missing-glyph box; `tests::glyphs_are_in_bundled_fonts` checks this.
//! Glyphs were picked by how egui renders them: emoji-icon-font draws
//! some codepoints as unrelated symbols (📋 as a tag, 🔗 as a pen).

pub(crate) const DATA_SOURCES: &str = "🖴";
pub(crate) const DATA_SOURCE: &str = "🖴";
pub(crate) const VOLUME: &str = "🗄";
pub(crate) const UNALLOCATED: &str = "⛶";
pub(crate) const FOLDER: &str = "🗀";
/// A file with derived children (archive members, extracted files).
pub(crate) const ARCHIVE: &str = "📦";
pub(crate) const VIEWS: &str = "👁";
pub(crate) const FILE_TYPES: &str = "🗐";
pub(crate) const BY_EXTENSION: &str = "🔤";
pub(crate) const BY_MIME: &str = "🏷";
pub(crate) const DELETED: &str = "🗑";
pub(crate) const FILE_SIZE: &str = "⚖";
pub(crate) const DATA_ARTIFACTS: &str = "📚";
pub(crate) const ANALYSIS_RESULTS: &str = "📊";
pub(crate) const TAGS: &str = "🏷";
pub(crate) const SCORE: &str = "⚠";
/// S column / Score tree: notable ("Bad Items").
pub(crate) const SCORE_NOTABLE: &str = "🔴";
/// S column / Score tree: likely notable ("Suspicious Items").
pub(crate) const SCORE_SUSPICIOUS: &str = "🔶";
/// C column: tagged, with a comment.
pub(crate) const COMMENT: &str = "💬";
/// C column: tagged, no comment.
pub(crate) const TAGGED: &str = "🏷";
pub(crate) const OS_ACCOUNTS: &str = "👥";
pub(crate) const HOST: &str = "🖥";

/// By Extension groups (`views::EXT_GROUPS` / `DOC_GROUPS` names).
pub(crate) fn file_group(name: &str) -> &'static str {
    match name {
        "Images" => "🖼",
        "Videos" => "🎞",
        "Audio" => "🎵",
        "Archives" => "📦",
        "Databases" => "⛃",
        "Executable" => "⚙",
        "HTML" => "🌐",
        "PDF" => "📕",
        "Office" => "💼",
        "Plain Text" => "🗋",
        "Rich Text" => "📝",
        _ => "🖹",
    }
}

/// MIME top-level media types.
pub(crate) fn media_type(top: &str) -> &'static str {
    match top {
        "image" => "🖼",
        "video" => "🎞",
        "audio" => "🎵",
        "text" => "🖹",
        "font" => "🔤",
        "message" => "✉",
        "multipart" => "📎",
        "model" => "📐",
        _ => "⚙",
    }
}

/// Artifact types by name; unknown (e.g. module-defined) types fall back
/// to their category's icon.
pub(crate) fn artifact(type_name: &str, analysis_result: bool) -> &'static str {
    match type_name {
        "TSK_GEN_INFO" | "TSK_METADATA" => "ℹ",
        "TSK_WEB_BOOKMARK" => "🔖",
        "TSK_WEB_COOKIE" => "🍪",
        "TSK_WEB_HISTORY" => "🕘",
        "TSK_WEB_DOWNLOAD" | "TSK_DOWNLOAD_SOURCE" => "📥",
        "TSK_WEB_SEARCH_QUERY" => "🔍",
        "TSK_WEB_CACHE" => "💾",
        "TSK_WEB_FORM_AUTOFILL" | "TSK_EXTRACTED_TEXT" => "📝",
        "TSK_WEB_FORM_ADDRESS" => "🏠",
        "TSK_WEB_CATEGORIZATION" | "TSK_TAG_FILE" | "TSK_TAG_ARTIFACT" => "🏷",
        "TSK_SERVICE_ACCOUNT" | "TSK_WEB_ACCOUNT_TYPE" | "TSK_OS_ACCOUNT" => "👤",
        "TSK_RECENT_OBJECT" => "🖹",
        "TSK_INSTALLED_PROG" => "📦",
        "TSK_PROG_RUN" => "🚀",
        "TSK_DELETED_PROG" => "🗑",
        "TSK_PROG_NOTIFICATIONS" => "🔔",
        "TSK_OS_INFO" => "💻",
        "TSK_DEVICE_ATTACHED" => "🔌",
        "TSK_DEVICE_INFO" | "TSK_SIM_ATTACHED" | "TSK_USER_DEVICE_EVENT" => "📱",
        "TSK_BLUETOOTH_PAIRING" | "TSK_BLUETOOTH_ADAPTER" => "📡",
        "TSK_WIFI_NETWORK" => "📶",
        "TSK_REMOTE_DRIVE" => "🖧",
        "TSK_DATA_SOURCE_USAGE" => "🖴",
        "TSK_BACKUP_EVENT" => "💿",
        "TSK_EMAIL_MSG" => "✉",
        "TSK_MESSAGE" => "💬",
        "TSK_CONTACT" => "👥",
        "TSK_CALLLOG" => "📞",
        "TSK_SPEED_DIAL_ENTRY" => "☎",
        "TSK_CALENDAR_ENTRY" => "📅",
        "TSK_ACCOUNT" => "💳",
        "TSK_GPS_TRACKPOINT" | "TSK_GPS_BOOKMARK" | "TSK_GPS_SEARCH" => "📍",
        "TSK_GPS_ROUTE" | "TSK_GPS_TRACK" | "TSK_GPS_AREA" => "🗺",
        "TSK_METADATA_EXIF" => "📷",
        "TSK_SCREEN_SHOTS" => "🖥",
        "TSK_CLIPBOARD_CONTENT" => "📎",
        "TSK_ASSOCIATED_OBJECT" => "📌",
        "TSK_TOOL_OUTPUT" => "⚙",
        "TSK_TL_EVENT" => "⏱",
        "PM_EVENT_LOG" => "📜",
        "TSK_KEYWORD_HIT" => "🔑",
        "TSK_HASHSET_HIT" => "#",
        "TSK_INTERESTING_FILE_HIT" | "TSK_INTERESTING_ARTIFACT_HIT" | "TSK_INTERESTING_ITEM" => "★",
        "TSK_ENCRYPTION_DETECTED" => "🔐",
        "TSK_ENCRYPTION_SUSPECTED" => "🔓",
        "TSK_FACE_DETECTED" | "TSK_OBJECT_DETECTED" | "TSK_PREVIOUSLY_SEEN" => "👁",
        "TSK_PREVIOUSLY_UNSEEN" => "☆",
        "TSK_PREVIOUSLY_NOTABLE" => "🚩",
        "TSK_VERIFICATION_FAILED" => "❌",
        "TSK_YARA_HIT" => "🔬",
        "TSK_MALWARE" => "☣",
        _ if analysis_result => ANALYSIS_RESULTS,
        _ => "🖹",
    }
}

#[cfg(test)]
mod tests {
    use skrifa::MetadataProvider;

    use super::*;

    /// Every icon has a glyph in egui's default fonts.
    #[test]
    fn glyphs_are_in_bundled_fonts() {
        let mut icons = vec![
            DATA_SOURCES,
            DATA_SOURCE,
            VOLUME,
            UNALLOCATED,
            FOLDER,
            ARCHIVE,
            VIEWS,
            FILE_TYPES,
            BY_EXTENSION,
            BY_MIME,
            DELETED,
            FILE_SIZE,
            DATA_ARTIFACTS,
            ANALYSIS_RESULTS,
            TAGS,
            SCORE,
            SCORE_NOTABLE,
            SCORE_SUSPICIOUS,
            COMMENT,
            TAGGED,
            OS_ACCOUNTS,
            HOST,
        ];
        for (name, _) in crate::views::EXT_GROUPS
            .iter()
            .chain(crate::views::DOC_GROUPS)
        {
            icons.push(file_group(name));
        }
        for top in [
            "image",
            "video",
            "audio",
            "text",
            "font",
            "message",
            "multipart",
            "model",
            "application",
        ] {
            icons.push(media_type(top));
        }
        for (_, name, ..) in pm_casedb::catalog::ARTIFACT_TYPES {
            icons.push(artifact(name, false));
        }
        icons.push(artifact("PM_EVENT_LOG", false));
        icons.push(artifact("PM_UNKNOWN", true));
        // egui's `has_glyph` reports false negatives for glyphs of the face
        // that also holds the replacement character, so read the charmaps.
        let faces: Vec<skrifa::FontRef<'_>> = [
            epaint_default_fonts::UBUNTU_LIGHT,
            epaint_default_fonts::NOTO_EMOJI_REGULAR,
            epaint_default_fonts::EMOJI_ICON,
        ]
        .into_iter()
        .map(|data| skrifa::FontRef::new(data).unwrap())
        .collect();
        let missing: Vec<&str> = icons
            .into_iter()
            .filter(|icon| {
                !icon
                    .chars()
                    .all(|c| faces.iter().any(|f| f.charmap().map(c).is_some()))
            })
            .collect();
        assert!(missing.is_empty(), "no glyph for {missing:?}");
    }
}
