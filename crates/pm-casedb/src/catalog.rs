//! Standard artifact and attribute types (Autopsy/Autopsy blackboard catalogue).
//!
//! Artifact type ids equal Autopsy's numeric ids so Autopsy cases can be
//! imported without remapping. Attribute ids are postmortem's own (Autopsy's are
//! not reliably known); names are Autopsy's, which is what an importer keys on.

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
    (1, "KAAMOS_GEN_INFO", "General Info", D),
    (2, "KAAMOS_WEB_BOOKMARK", "Web Bookmarks", D),
    (3, "KAAMOS_WEB_COOKIE", "Web Cookies", D),
    (4, "KAAMOS_WEB_HISTORY", "Web History", D),
    (5, "KAAMOS_WEB_DOWNLOAD", "Web Downloads", D),
    (6, "KAAMOS_RECENT_OBJECT", "Recent Documents", D),
    (7, "KAAMOS_GPS_TRACKPOINT", "GPS Trackpoints", D),
    (8, "KAAMOS_INSTALLED_PROG", "Installed Programs", D),
    (9, "KAAMOS_KEYWORD_HIT", "Keyword Hits", A),
    (10, "KAAMOS_HASHSET_HIT", "Hashset Hits", A),
    (11, "KAAMOS_DEVICE_ATTACHED", "USB Device Attached", D),
    (12, "KAAMOS_INTERESTING_FILE_HIT", "Interesting Files", A),
    (13, "KAAMOS_EMAIL_MSG", "E-Mail Messages", D),
    (14, "KAAMOS_EXTRACTED_TEXT", "Extracted Text", D),
    (15, "KAAMOS_WEB_SEARCH_QUERY", "Web Search", D),
    (16, "KAAMOS_METADATA_EXIF", "EXIF Metadata", A),
    (17, "KAAMOS_TAG_FILE", "Tagged Files", A),
    (18, "KAAMOS_TAG_ARTIFACT", "Tagged Results", A),
    (19, "KAAMOS_OS_INFO", "Operating System Information", D),
    (20, "KAAMOS_OS_ACCOUNT", "Operating System User Account", D),
    (21, "KAAMOS_SERVICE_ACCOUNT", "Web Accounts", D),
    (22, "KAAMOS_TOOL_OUTPUT", "Raw Tool Output", D),
    (23, "KAAMOS_CONTACT", "Contacts", D),
    (24, "KAAMOS_MESSAGE", "Messages", D),
    (25, "KAAMOS_CALLLOG", "Call Logs", D),
    (26, "KAAMOS_CALENDAR_ENTRY", "Calendar Entries", D),
    (27, "KAAMOS_SPEED_DIAL_ENTRY", "Speed Dial Entries", D),
    (28, "KAAMOS_BLUETOOTH_PAIRING", "Bluetooth Pairings", D),
    (29, "KAAMOS_GPS_BOOKMARK", "GPS Bookmarks", D),
    (
        30,
        "KAAMOS_GPS_LAST_KNOWN_LOCATION",
        "GPS Last Known Location",
        D,
    ),
    (31, "KAAMOS_GPS_SEARCH", "GPS Searches", D),
    (32, "KAAMOS_PROG_RUN", "Run Programs", D),
    (33, "KAAMOS_ENCRYPTION_DETECTED", "Encryption Detected", A),
    (
        34,
        "KAAMOS_EXT_MISMATCH_DETECTED",
        "Extension Mismatch Detected",
        A,
    ),
    (
        35,
        "KAAMOS_INTERESTING_ARTIFACT_HIT",
        "Interesting Results",
        A,
    ),
    (36, "KAAMOS_GPS_ROUTE", "GPS Route", D),
    (37, "KAAMOS_REMOTE_DRIVE", "Remote Drive", D),
    (38, "KAAMOS_FACE_DETECTED", "Face Detected", A),
    (39, "KAAMOS_ACCOUNT", "Accounts", D),
    (40, "KAAMOS_ENCRYPTION_SUSPECTED", "Encryption Suspected", A),
    (41, "KAAMOS_OBJECT_DETECTED", "Object Detected", A),
    (42, "KAAMOS_WIFI_NETWORK", "Wireless Networks", D),
    (43, "KAAMOS_DEVICE_INFO", "Device Info", D),
    (44, "KAAMOS_SIM_ATTACHED", "SIM Attached", D),
    (45, "KAAMOS_BLUETOOTH_ADAPTER", "Bluetooth Adapter", D),
    (
        46,
        "KAAMOS_WIFI_NETWORK_ADAPTER",
        "Wireless Network Adapters",
        D,
    ),
    (47, "KAAMOS_VERIFICATION_FAILED", "Verification Failure", A),
    (48, "KAAMOS_DATA_SOURCE_USAGE", "Data Source Usage", A),
    (49, "KAAMOS_WEB_FORM_AUTOFILL", "Web Form Autofill", D),
    (50, "KAAMOS_WEB_FORM_ADDRESS", "Web Form Addresses", D),
    (51, "KAAMOS_DOWNLOAD_SOURCE", "Download Source", D),
    (52, "KAAMOS_WEB_CACHE", "Web Cache", D),
    (53, "KAAMOS_TL_EVENT", "TL Events", D),
    (54, "KAAMOS_CLIPBOARD_CONTENT", "Clipboard Content", D),
    (55, "KAAMOS_ASSOCIATED_OBJECT", "Associated Object", D),
    (
        56,
        "KAAMOS_USER_CONTENT_SUSPECTED",
        "User Content Suspected",
        A,
    ),
    (57, "KAAMOS_METADATA", "Metadata", D),
    (58, "KAAMOS_GPS_TRACK", "GPS Track", D),
    (59, "KAAMOS_WEB_ACCOUNT_TYPE", "Web Account Type", A),
    (60, "KAAMOS_SCREEN_SHOTS", "Screenshots", D),
    (61, "KAAMOS_PROG_NOTIFICATIONS", "Program Notifications", D),
    (62, "KAAMOS_BACKUP_EVENT", "Backup Events", D),
    (63, "KAAMOS_DELETED_PROG", "Deleted Programs", D),
    (64, "KAAMOS_USER_DEVICE_EVENT", "User Device Events", D),
    (65, "KAAMOS_YARA_HIT", "YARA Hit", A),
    (66, "KAAMOS_GPS_AREA", "GPS Area", D),
    (67, "KAAMOS_WEB_CATEGORIZATION", "Web Categorization", A),
    (68, "KAAMOS_PREVIOUSLY_SEEN", "Previously Seen", A),
    (69, "KAAMOS_PREVIOUSLY_UNSEEN", "Previously Unseen", A),
    (70, "KAAMOS_PREVIOUSLY_NOTABLE", "Previously Notable", A),
    (71, "KAAMOS_INTERESTING_ITEM", "Interesting Items", A),
    (72, "KAAMOS_MALWARE", "Malware", A),
];

/// Artifact types Autopsy hides from the tree.
pub const HIDDEN_ARTIFACT_TYPES: &[&str] = &[
    "KAAMOS_DATA_SOURCE_USAGE",
    "KAAMOS_GEN_INFO",
    "KAAMOS_DOWNLOAD_SOURCE",
    "KAAMOS_TL_EVENT",
    "KAAMOS_ASSOCIATED_OBJECT",
];

use ValueType::{DateTime as T, Double as F, Integer as I, Json as J, Long as L, String as S};

/// `(name, display name, value type)` for every standard attribute type.
/// Ids are assigned in this order starting at 1; append only.
pub const ATTRIBUTE_TYPES: &[(&str, &str, ValueType)] = &[
    ("KAAMOS_URL", "URL", S),
    ("KAAMOS_DATETIME", "Date/Time", T),
    ("KAAMOS_NAME", "Name", S),
    ("KAAMOS_PROG_NAME", "Program Name", S),
    ("KAAMOS_VALUE", "Value", S),
    ("KAAMOS_FLAG", "Flag", S),
    ("KAAMOS_PATH", "Path", S),
    ("KAAMOS_KEYWORD", "Keyword", S),
    ("KAAMOS_KEYWORD_REGEXP", "Keyword Regular Expression", S),
    ("KAAMOS_KEYWORD_PREVIEW", "Keyword Preview", S),
    ("KAAMOS_KEYWORD_SEARCH_TYPE", "Keyword Search Type", I),
    ("KAAMOS_SET_NAME", "Set Name", S),
    ("KAAMOS_USER_NAME", "User Name", S),
    ("KAAMOS_DOMAIN", "Domain", S),
    ("KAAMOS_PASSWORD", "Password", S),
    ("KAAMOS_NAME_PERSON", "Person Name", S),
    ("KAAMOS_DEVICE_MODEL", "Device Model", S),
    ("KAAMOS_MAC_ADDRESS", "MAC Address", S),
    ("KAAMOS_EMAIL", "Email", S),
    ("KAAMOS_EMAIL_BCC", "BCC", S),
    ("KAAMOS_EMAIL_CC", "CC", S),
    ("KAAMOS_EMAIL_CONTENT_HTML", "Message (HTML)", S),
    ("KAAMOS_EMAIL_CONTENT_PLAIN", "Message (Plaintext)", S),
    ("KAAMOS_EMAIL_CONTENT_RTF", "Message (RTF)", S),
    ("KAAMOS_MSG_ID", "Message ID", S),
    ("KAAMOS_MSG_REPLY_ID", "Message Reply ID", S),
    ("KAAMOS_DATETIME_RCVD", "Date Received", T),
    ("KAAMOS_DATETIME_SENT", "Date Sent", T),
    ("KAAMOS_SUBJECT", "Subject", S),
    ("KAAMOS_TITLE", "Title", S),
    ("KAAMOS_GEO_LATITUDE", "Latitude", F),
    ("KAAMOS_GEO_LONGITUDE", "Longitude", F),
    ("KAAMOS_GEO_VELOCITY", "Velocity", F),
    ("KAAMOS_GEO_ALTITUDE", "Altitude", F),
    ("KAAMOS_GEO_BEARING", "Bearing", S),
    ("KAAMOS_GEO_HPRECISION", "Horizontal Precision", F),
    ("KAAMOS_GEO_VPRECISION", "Vertical Precision", F),
    ("KAAMOS_GEO_MAPDATUM", "Map Datum", S),
    ("KAAMOS_FILE_TYPE_SIG", "File Type (signature)", S),
    ("KAAMOS_FILE_TYPE_EXT", "File Type (extension)", S),
    ("KAAMOS_TAGGED_ARTIFACT", "Tagged Result", L),
    ("KAAMOS_TAG_NAME", "Tag Name", S),
    ("KAAMOS_COMMENT", "Comment", S),
    ("KAAMOS_URL_DECODED", "Decoded URL", S),
    ("KAAMOS_DATETIME_CREATED", "Date Created", T),
    ("KAAMOS_DATETIME_MODIFIED", "Date Modified", T),
    ("KAAMOS_PROCESSOR_ARCHITECTURE", "Processor Architecture", S),
    ("KAAMOS_VERSION", "Version", S),
    ("KAAMOS_USER_ID", "User ID", S),
    ("KAAMOS_DESCRIPTION", "Description", S),
    ("KAAMOS_MESSAGE_TYPE", "Message Type", S),
    ("KAAMOS_PHONE_NUMBER_HOME", "Phone Number (Home)", S),
    ("KAAMOS_PHONE_NUMBER_OFFICE", "Phone Number (Office)", S),
    ("KAAMOS_PHONE_NUMBER_MOBILE", "Phone Number (Mobile)", S),
    ("KAAMOS_PHONE_NUMBER_FROM", "From Phone Number", S),
    ("KAAMOS_PHONE_NUMBER_TO", "To Phone Number", S),
    ("KAAMOS_DIRECTION", "Direction", S),
    ("KAAMOS_EMAIL_HOME", "Email (Home)", S),
    ("KAAMOS_EMAIL_OFFICE", "Email (Office)", S),
    ("KAAMOS_DATETIME_START", "Start Date/Time", T),
    ("KAAMOS_DATETIME_END", "End Date/Time", T),
    ("KAAMOS_CALENDAR_ENTRY_TYPE", "Calendar Entry Type", S),
    ("KAAMOS_LOCATION", "Location", S),
    ("KAAMOS_SHORTCUT", "Short Cut", S),
    ("KAAMOS_DEVICE_NAME", "Device Name", S),
    ("KAAMOS_CATEGORY", "Category", S),
    ("KAAMOS_EMAIL_REPLYTO", "ReplyTo Address", S),
    ("KAAMOS_SERVER_NAME", "Server Name", S),
    ("KAAMOS_COUNT", "Count", I),
    ("KAAMOS_MIN_COUNT", "Minimum Count", I),
    ("KAAMOS_PATH_SOURCE", "Path Source", S),
    ("KAAMOS_PERMISSIONS", "Permissions", S),
    ("KAAMOS_ASSOCIATED_ARTIFACT", "Associated Artifact", L),
    ("KAAMOS_ISDELETED", "Is Deleted", S),
    ("KAAMOS_GEO_LATITUDE_START", "Starting Latitude", F),
    ("KAAMOS_GEO_LATITUDE_END", "Ending Latitude", F),
    ("KAAMOS_GEO_LONGITUDE_START", "Starting Longitude", F),
    ("KAAMOS_GEO_LONGITUDE_END", "Ending Longitude", F),
    ("KAAMOS_READ_STATUS", "Read", I),
    ("KAAMOS_LOCAL_PATH", "Local Path", S),
    ("KAAMOS_REMOTE_PATH", "Remote Path", S),
    ("KAAMOS_TEMP_DIR", "Temporary Files Directory", S),
    ("KAAMOS_PRODUCT_ID", "Product ID", S),
    ("KAAMOS_OWNER", "Owner", S),
    ("KAAMOS_ORGANIZATION", "Organization", S),
    ("KAAMOS_CARD_NUMBER", "Card Number", S),
    ("KAAMOS_CARD_EXPIRATION", "Card Expiration (YYMM)", S),
    ("KAAMOS_CARD_SERVICE_CODE", "Card Service Code", S),
    ("KAAMOS_CARD_DISCRETIONARY", "Card Discretionary", S),
    ("KAAMOS_CARD_LRC", "Card Longitudinal Redundancy Check", S),
    (
        "KAAMOS_KEYWORD_SEARCH_DOCUMENT_ID",
        "Keyword Search Document ID",
        S,
    ),
    ("KAAMOS_CARD_SCHEME", "Card Scheme", S),
    ("KAAMOS_CARD_TYPE", "Card Type", S),
    ("KAAMOS_BRAND_NAME", "Brand Name", S),
    ("KAAMOS_BANK_NAME", "Bank Name", S),
    ("KAAMOS_COUNTRY", "Country", S),
    ("KAAMOS_CITY", "City", S),
    ("KAAMOS_ACCOUNT_TYPE", "Account Type", S),
    (
        "KAAMOS_KEYWORD_SEARCH_TYPE_2",
        "Keyword Search Type (legacy)",
        I,
    ),
    ("KAAMOS_PHONE_NUMBER", "Phone Number", S),
    ("KAAMOS_EMAIL_TO", "E-Mail To", S),
    ("KAAMOS_EMAIL_FROM", "E-Mail From", S),
    ("KAAMOS_HEADERS", "Headers", S),
    ("KAAMOS_THREAD_ID", "Thread ID", S),
    ("KAAMOS_TEXT", "Text", S),
    ("KAAMOS_ID", "ID", S),
    ("KAAMOS_REFERRER", "Referrer URL", S),
    ("KAAMOS_HOST", "Host", S),
    ("KAAMOS_DATETIME_ACCESSED", "Date Accessed", T),
    ("KAAMOS_DATETIME_DELETED", "Date Deleted", T),
    ("KAAMOS_DATETIME_PASSWORD_RESET", "Password Reset Date", T),
    ("KAAMOS_DATETIME_PASSWORD_FAIL", "Password Fail Date", T),
    ("KAAMOS_LAST_ACCESSED", "Last Accessed", T),
    ("KAAMOS_LAST_PRINTED_DATETIME", "Last Printed", T),
    ("KAAMOS_PASSWORD_HINT", "Password Hint", S),
    ("KAAMOS_PASSWORD_SETTINGS", "Password Settings", S),
    ("KAAMOS_ACCOUNT_SETTINGS", "Account Settings", S),
    ("KAAMOS_IS_ADMIN", "Is Administrator", I),
    ("KAAMOS_GROUPS", "Groups", S),
    ("KAAMOS_HOME_DIR", "Home Directory", S),
    ("KAAMOS_REALM", "Realm", S),
    ("KAAMOS_RULE", "Rule", S),
    ("KAAMOS_PATH_ID", "Path ID", L),
    ("KAAMOS_HASH_MD5", "MD5 Hash", S),
    ("KAAMOS_HASH_SHA1", "SHA1 Hash", S),
    ("KAAMOS_HASH_SHA2_256", "SHA-256 Hash", S),
    ("KAAMOS_BYTES_SENT", "Bytes Sent", L),
    ("KAAMOS_BYTES_RECEIVED", "Bytes Received", L),
    ("KAAMOS_DEVICE_ID", "Device ID", S),
    ("KAAMOS_DEVICE_MAKE", "Device Make", S),
    ("KAAMOS_IMEI", "IMEI", S),
    ("KAAMOS_IMSI", "IMSI", S),
    ("KAAMOS_ICCID", "ICCID", S),
    ("KAAMOS_SSID", "SSID", S),
    ("KAAMOS_IP_ADDRESS", "IP Address", S),
    ("KAAMOS_ATTACHMENTS", "Message Attachments", J),
    ("KAAMOS_GEO_TRACKPOINTS", "Track Points", J),
    ("KAAMOS_GEO_WAYPOINTS", "Waypoints", J),
    ("KAAMOS_GEO_AREAPOINTS", "Area Points", J),
    ("KAAMOS_CORRELATION_TYPE", "Correlation Type", S),
    ("KAAMOS_CORRELATION_VALUE", "Correlation Value", S),
    ("KAAMOS_OTHER_CASES", "Other Cases", S),
    ("KAAMOS_TL_EVENT_TYPE", "Event Type", L),
    ("KAAMOS_HASH_PHOTODNA", "PhotoDNA Hash", S),
    ("KAAMOS_ENTROPY", "Entropy", F),
    ("KAAMOS_MIME_TYPE", "MIME Type", S),
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
    fn artifact_ids_are_stable_and_unique() {
        assert_eq!(
            artifact_type_id("KAAMOS_KEYWORD_HIT"),
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
        assert_eq!(attribute_type_id("KAAMOS_URL"), Some(1));
        assert!(attribute_type_id("KAAMOS_SET_NAME").is_some());
        assert!(attribute_type_id("NOPE").is_none());
    }

    #[test]
    fn hidden_types_exist() {
        for h in HIDDEN_ARTIFACT_TYPES {
            assert!(artifact_type_id(h).is_some(), "{h}");
        }
    }
}
