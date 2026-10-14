//! Standard artifact and attribute types (Autopsy/TSK blackboard catalogue).
//!
//! Artifact type ids equal TSK's numeric ids so Autopsy cases can be
//! imported without remapping. Attribute ids are postmortem's own (TSK's are
//! not reliably known); names are TSK's, which is what an importer keys on.

/// Data artifact (extracted fact) or analysis result (scored judgement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    DataArtifact = 0,
    AnalysisResult = 1,
}

/// How an attribute's value is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueType {
    String = 0,
    Integer = 1,
    Long = 2,
    Double = 3,
    Bytes = 4,
    /// Unix seconds.
    DateTime = 5,
    Json = 6,
}

impl ValueType {
    pub fn from_code(c: i64) -> Option<Self> {
        Some(match c {
            0 => Self::String,
            1 => Self::Integer,
            2 => Self::Long,
            3 => Self::Double,
            4 => Self::Bytes,
            5 => Self::DateTime,
            6 => Self::Json,
            _ => return None,
        })
    }
}

use Category::{AnalysisResult as A, DataArtifact as D};

/// `(id, name, display name, category)` for every standard artifact type.
pub const ARTIFACT_TYPES: &[(i64, &str, &str, Category)] = &[
    (1, "TSK_GEN_INFO", "General Info", D),
    (2, "TSK_WEB_BOOKMARK", "Web Bookmarks", D),
    (3, "TSK_WEB_COOKIE", "Web Cookies", D),
    (4, "TSK_WEB_HISTORY", "Web History", D),
    (5, "TSK_WEB_DOWNLOAD", "Web Downloads", D),
    (6, "TSK_RECENT_OBJECT", "Recent Documents", D),
    (7, "TSK_GPS_TRACKPOINT", "GPS Trackpoints", D),
    (8, "TSK_INSTALLED_PROG", "Installed Programs", D),
    (9, "TSK_KEYWORD_HIT", "Keyword Hits", A),
    (10, "TSK_HASHSET_HIT", "Hashset Hits", A),
    (11, "TSK_DEVICE_ATTACHED", "USB Device Attached", D),
    (12, "TSK_INTERESTING_FILE_HIT", "Interesting Files", A),
    (13, "TSK_EMAIL_MSG", "E-Mail Messages", D),
    (14, "TSK_EXTRACTED_TEXT", "Extracted Text", D),
    (15, "TSK_WEB_SEARCH_QUERY", "Web Search", D),
    (16, "TSK_METADATA_EXIF", "EXIF Metadata", A),
    (17, "TSK_TAG_FILE", "Tagged Files", A),
    (18, "TSK_TAG_ARTIFACT", "Tagged Results", A),
    (19, "TSK_OS_INFO", "Operating System Information", D),
    (20, "TSK_OS_ACCOUNT", "Operating System User Account", D),
    (21, "TSK_SERVICE_ACCOUNT", "Web Accounts", D),
    (22, "TSK_TOOL_OUTPUT", "Raw Tool Output", D),
    (23, "TSK_CONTACT", "Contacts", D),
    (24, "TSK_MESSAGE", "Messages", D),
    (25, "TSK_CALLLOG", "Call Logs", D),
    (26, "TSK_CALENDAR_ENTRY", "Calendar Entries", D),
    (27, "TSK_SPEED_DIAL_ENTRY", "Speed Dial Entries", D),
    (28, "TSK_BLUETOOTH_PAIRING", "Bluetooth Pairings", D),
    (29, "TSK_GPS_BOOKMARK", "GPS Bookmarks", D),
    (
        30,
        "TSK_GPS_LAST_KNOWN_LOCATION",
        "GPS Last Known Location",
        D,
    ),
    (31, "TSK_GPS_SEARCH", "GPS Searches", D),
    (32, "TSK_PROG_RUN", "Run Programs", D),
    (33, "TSK_ENCRYPTION_DETECTED", "Encryption Detected", A),
    (
        34,
        "TSK_EXT_MISMATCH_DETECTED",
        "Extension Mismatch Detected",
        A,
    ),
    (35, "TSK_INTERESTING_ARTIFACT_HIT", "Interesting Results", A),
    (36, "TSK_GPS_ROUTE", "GPS Route", D),
    (37, "TSK_REMOTE_DRIVE", "Remote Drive", D),
    (38, "TSK_FACE_DETECTED", "Face Detected", A),
    (39, "TSK_ACCOUNT", "Accounts", D),
    (40, "TSK_ENCRYPTION_SUSPECTED", "Encryption Suspected", A),
    (41, "TSK_OBJECT_DETECTED", "Object Detected", A),
    (42, "TSK_WIFI_NETWORK", "Wireless Networks", D),
    (43, "TSK_DEVICE_INFO", "Device Info", D),
    (44, "TSK_SIM_ATTACHED", "SIM Attached", D),
    (45, "TSK_BLUETOOTH_ADAPTER", "Bluetooth Adapter", D),
    (
        46,
        "TSK_WIFI_NETWORK_ADAPTER",
        "Wireless Network Adapters",
        D,
    ),
    (47, "TSK_VERIFICATION_FAILED", "Verification Failure", A),
    (48, "TSK_DATA_SOURCE_USAGE", "Data Source Usage", A),
    (49, "TSK_WEB_FORM_AUTOFILL", "Web Form Autofill", D),
    (50, "TSK_WEB_FORM_ADDRESS", "Web Form Addresses", D),
    (51, "TSK_DOWNLOAD_SOURCE", "Download Source", D),
    (52, "TSK_WEB_CACHE", "Web Cache", D),
    (53, "TSK_TL_EVENT", "TL Events", D),
    (54, "TSK_CLIPBOARD_CONTENT", "Clipboard Content", D),
    (55, "TSK_ASSOCIATED_OBJECT", "Associated Object", D),
    (
        56,
        "TSK_USER_CONTENT_SUSPECTED",
        "User Content Suspected",
        A,
    ),
    (57, "TSK_METADATA", "Metadata", D),
    (58, "TSK_GPS_TRACK", "GPS Track", D),
    (59, "TSK_WEB_ACCOUNT_TYPE", "Web Account Type", A),
    (60, "TSK_SCREEN_SHOTS", "Screenshots", D),
    (61, "TSK_PROG_NOTIFICATIONS", "Program Notifications", D),
    (62, "TSK_BACKUP_EVENT", "Backup Events", D),
    (63, "TSK_DELETED_PROG", "Deleted Programs", D),
    (64, "TSK_USER_DEVICE_EVENT", "User Device Events", D),
    (65, "TSK_YARA_HIT", "YARA Hit", A),
    (66, "TSK_GPS_AREA", "GPS Area", D),
    (67, "TSK_WEB_CATEGORIZATION", "Web Categorization", A),
    (68, "TSK_PREVIOUSLY_SEEN", "Previously Seen", A),
    (69, "TSK_PREVIOUSLY_UNSEEN", "Previously Unseen", A),
    (70, "TSK_PREVIOUSLY_NOTABLE", "Previously Notable", A),
    (71, "TSK_INTERESTING_ITEM", "Interesting Items", A),
    (72, "TSK_MALWARE", "Malware", A),
];

/// Artifact types Autopsy hides from the tree.
pub const HIDDEN_ARTIFACT_TYPES: &[&str] = &[
    "TSK_DATA_SOURCE_USAGE",
    "TSK_GEN_INFO",
    "TSK_DOWNLOAD_SOURCE",
    "TSK_TL_EVENT",
    "TSK_ASSOCIATED_OBJECT",
];

use ValueType::{DateTime as T, Double as F, Integer as I, Json as J, Long as L, String as S};

/// `(name, display name, value type)` for every standard attribute type.
/// Ids are assigned in this order starting at 1; append only.
pub const ATTRIBUTE_TYPES: &[(&str, &str, ValueType)] = &[
    ("TSK_URL", "URL", S),
    ("TSK_DATETIME", "Date/Time", T),
    ("TSK_NAME", "Name", S),
    ("TSK_PROG_NAME", "Program Name", S),
    ("TSK_VALUE", "Value", S),
    ("TSK_FLAG", "Flag", S),
    ("TSK_PATH", "Path", S),
    ("TSK_KEYWORD", "Keyword", S),
    ("TSK_KEYWORD_REGEXP", "Keyword Regular Expression", S),
    ("TSK_KEYWORD_PREVIEW", "Keyword Preview", S),
    ("TSK_KEYWORD_SEARCH_TYPE", "Keyword Search Type", I),
    ("TSK_SET_NAME", "Set Name", S),
    ("TSK_USER_NAME", "User Name", S),
    ("TSK_DOMAIN", "Domain", S),
    ("TSK_PASSWORD", "Password", S),
    ("TSK_NAME_PERSON", "Person Name", S),
    ("TSK_DEVICE_MODEL", "Device Model", S),
    ("TSK_MAC_ADDRESS", "MAC Address", S),
    ("TSK_EMAIL", "Email", S),
    ("TSK_EMAIL_BCC", "BCC", S),
    ("TSK_EMAIL_CC", "CC", S),
    ("TSK_EMAIL_CONTENT_HTML", "Message (HTML)", S),
    ("TSK_EMAIL_CONTENT_PLAIN", "Message (Plaintext)", S),
    ("TSK_EMAIL_CONTENT_RTF", "Message (RTF)", S),
    ("TSK_MSG_ID", "Message ID", S),
    ("TSK_MSG_REPLY_ID", "Message Reply ID", S),
    ("TSK_DATETIME_RCVD", "Date Received", T),
    ("TSK_DATETIME_SENT", "Date Sent", T),
    ("TSK_SUBJECT", "Subject", S),
    ("TSK_TITLE", "Title", S),
    ("TSK_GEO_LATITUDE", "Latitude", F),
    ("TSK_GEO_LONGITUDE", "Longitude", F),
    ("TSK_GEO_VELOCITY", "Velocity", F),
    ("TSK_GEO_ALTITUDE", "Altitude", F),
    ("TSK_GEO_BEARING", "Bearing", S),
    ("TSK_GEO_HPRECISION", "Horizontal Precision", F),
    ("TSK_GEO_VPRECISION", "Vertical Precision", F),
    ("TSK_GEO_MAPDATUM", "Map Datum", S),
    ("TSK_FILE_TYPE_SIG", "File Type (signature)", S),
    ("TSK_FILE_TYPE_EXT", "File Type (extension)", S),
    ("TSK_TAGGED_ARTIFACT", "Tagged Result", L),
    ("TSK_TAG_NAME", "Tag Name", S),
    ("TSK_COMMENT", "Comment", S),
    ("TSK_URL_DECODED", "Decoded URL", S),
    ("TSK_DATETIME_CREATED", "Date Created", T),
    ("TSK_DATETIME_MODIFIED", "Date Modified", T),
    ("TSK_PROCESSOR_ARCHITECTURE", "Processor Architecture", S),
    ("TSK_VERSION", "Version", S),
    ("TSK_USER_ID", "User ID", S),
    ("TSK_DESCRIPTION", "Description", S),
    ("TSK_MESSAGE_TYPE", "Message Type", S),
    ("TSK_PHONE_NUMBER_HOME", "Phone Number (Home)", S),
    ("TSK_PHONE_NUMBER_OFFICE", "Phone Number (Office)", S),
    ("TSK_PHONE_NUMBER_MOBILE", "Phone Number (Mobile)", S),
    ("TSK_PHONE_NUMBER_FROM", "From Phone Number", S),
    ("TSK_PHONE_NUMBER_TO", "To Phone Number", S),
    ("TSK_DIRECTION", "Direction", S),
    ("TSK_EMAIL_HOME", "Email (Home)", S),
    ("TSK_EMAIL_OFFICE", "Email (Office)", S),
    ("TSK_DATETIME_START", "Start Date/Time", T),
    ("TSK_DATETIME_END", "End Date/Time", T),
    ("TSK_CALENDAR_ENTRY_TYPE", "Calendar Entry Type", S),
    ("TSK_LOCATION", "Location", S),
    ("TSK_SHORTCUT", "Short Cut", S),
    ("TSK_DEVICE_NAME", "Device Name", S),
    ("TSK_CATEGORY", "Category", S),
    ("TSK_EMAIL_REPLYTO", "ReplyTo Address", S),
    ("TSK_SERVER_NAME", "Server Name", S),
    ("TSK_COUNT", "Count", I),
    ("TSK_MIN_COUNT", "Minimum Count", I),
    ("TSK_PATH_SOURCE", "Path Source", S),
    ("TSK_PERMISSIONS", "Permissions", S),
    ("TSK_ASSOCIATED_ARTIFACT", "Associated Artifact", L),
    ("TSK_ISDELETED", "Is Deleted", S),
    ("TSK_GEO_LATITUDE_START", "Starting Latitude", F),
    ("TSK_GEO_LATITUDE_END", "Ending Latitude", F),
    ("TSK_GEO_LONGITUDE_START", "Starting Longitude", F),
    ("TSK_GEO_LONGITUDE_END", "Ending Longitude", F),
    ("TSK_READ_STATUS", "Read", I),
    ("TSK_LOCAL_PATH", "Local Path", S),
    ("TSK_REMOTE_PATH", "Remote Path", S),
    ("TSK_TEMP_DIR", "Temporary Files Directory", S),
    ("TSK_PRODUCT_ID", "Product ID", S),
    ("TSK_OWNER", "Owner", S),
    ("TSK_ORGANIZATION", "Organization", S),
    ("TSK_CARD_NUMBER", "Card Number", S),
    ("TSK_CARD_EXPIRATION", "Card Expiration (YYMM)", S),
    ("TSK_CARD_SERVICE_CODE", "Card Service Code", S),
    ("TSK_CARD_DISCRETIONARY", "Card Discretionary", S),
    ("TSK_CARD_LRC", "Card Longitudinal Redundancy Check", S),
    (
        "TSK_KEYWORD_SEARCH_DOCUMENT_ID",
        "Keyword Search Document ID",
        S,
    ),
    ("TSK_CARD_SCHEME", "Card Scheme", S),
    ("TSK_CARD_TYPE", "Card Type", S),
    ("TSK_BRAND_NAME", "Brand Name", S),
    ("TSK_BANK_NAME", "Bank Name", S),
    ("TSK_COUNTRY", "Country", S),
    ("TSK_CITY", "City", S),
    ("TSK_ACCOUNT_TYPE", "Account Type", S),
    (
        "TSK_KEYWORD_SEARCH_TYPE_2",
        "Keyword Search Type (legacy)",
        I,
    ),
    ("TSK_PHONE_NUMBER", "Phone Number", S),
    ("TSK_EMAIL_TO", "E-Mail To", S),
    ("TSK_EMAIL_FROM", "E-Mail From", S),
    ("TSK_HEADERS", "Headers", S),
    ("TSK_THREAD_ID", "Thread ID", S),
    ("TSK_TEXT", "Text", S),
    ("TSK_ID", "ID", S),
    ("TSK_REFERRER", "Referrer URL", S),
    ("TSK_HOST", "Host", S),
    ("TSK_DATETIME_ACCESSED", "Date Accessed", T),
    ("TSK_DATETIME_DELETED", "Date Deleted", T),
    ("TSK_DATETIME_PASSWORD_RESET", "Password Reset Date", T),
    ("TSK_DATETIME_PASSWORD_FAIL", "Password Fail Date", T),
    ("TSK_LAST_ACCESSED", "Last Accessed", T),
    ("TSK_LAST_PRINTED_DATETIME", "Last Printed", T),
    ("TSK_PASSWORD_HINT", "Password Hint", S),
    ("TSK_PASSWORD_SETTINGS", "Password Settings", S),
    ("TSK_ACCOUNT_SETTINGS", "Account Settings", S),
    ("TSK_IS_ADMIN", "Is Administrator", I),
    ("TSK_GROUPS", "Groups", S),
    ("TSK_HOME_DIR", "Home Directory", S),
    ("TSK_REALM", "Realm", S),
    ("TSK_RULE", "Rule", S),
    ("TSK_PATH_ID", "Path ID", L),
    ("TSK_HASH_MD5", "MD5 Hash", S),
    ("TSK_HASH_SHA1", "SHA1 Hash", S),
    ("TSK_HASH_SHA2_256", "SHA-256 Hash", S),
    ("TSK_BYTES_SENT", "Bytes Sent", L),
    ("TSK_BYTES_RECEIVED", "Bytes Received", L),
    ("TSK_DEVICE_ID", "Device ID", S),
    ("TSK_DEVICE_MAKE", "Device Make", S),
    ("TSK_IMEI", "IMEI", S),
    ("TSK_IMSI", "IMSI", S),
    ("TSK_ICCID", "ICCID", S),
    ("TSK_SSID", "SSID", S),
    ("TSK_IP_ADDRESS", "IP Address", S),
    ("TSK_ATTACHMENTS", "Message Attachments", J),
    ("TSK_GEO_TRACKPOINTS", "Track Points", J),
    ("TSK_GEO_WAYPOINTS", "Waypoints", J),
    ("TSK_GEO_AREAPOINTS", "Area Points", J),
    ("TSK_CORRELATION_TYPE", "Correlation Type", S),
    ("TSK_CORRELATION_VALUE", "Correlation Value", S),
    ("TSK_OTHER_CASES", "Other Cases", S),
    ("TSK_TL_EVENT_TYPE", "Event Type", L),
    ("TSK_HASH_PHOTODNA", "PhotoDNA Hash", S),
    ("TSK_ENTROPY", "Entropy", F),
    ("TSK_MIME_TYPE", "MIME Type", S),
];

/// Id of a standard artifact type by name.
pub fn artifact_type_id(name: &str) -> Option<i64> {
    ARTIFACT_TYPES.iter().find(|t| t.1 == name).map(|t| t.0)
}

/// Id of a standard attribute type by name.
pub fn attribute_type_id(name: &str) -> Option<i64> {
    ATTRIBUTE_TYPES
        .iter()
        .position(|t| t.0 == name)
        .map(|i| i as i64 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn artifact_ids_are_tsk_ids_and_unique() {
        assert_eq!(
            artifact_type_id("TSK_KEYWORD_HIT"),
            Some(9),
            "confirmed by Autopsy's SQL"
        );
        let ids: HashSet<_> = ARTIFACT_TYPES.iter().map(|t| t.0).collect();
        assert_eq!(ids.len(), ARTIFACT_TYPES.len());
        let names: HashSet<_> = ARTIFACT_TYPES.iter().map(|t| t.1).collect();
        assert_eq!(names.len(), ARTIFACT_TYPES.len());
        assert!(ARTIFACT_TYPES.windows(2).all(|w| w[0].0 + 1 == w[1].0));
    }

    #[test]
    fn attribute_names_unique() {
        let names: HashSet<_> = ATTRIBUTE_TYPES.iter().map(|t| t.0).collect();
        assert_eq!(names.len(), ATTRIBUTE_TYPES.len());
        assert_eq!(attribute_type_id("TSK_URL"), Some(1));
        assert!(attribute_type_id("TSK_SET_NAME").is_some());
        assert!(attribute_type_id("NOPE").is_none());
    }

    #[test]
    fn hidden_types_exist() {
        for h in HIDDEN_ARTIFACT_TYPES {
            assert!(artifact_type_id(h).is_some(), "{h}");
        }
    }
}
