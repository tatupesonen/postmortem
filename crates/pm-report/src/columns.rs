//! Autopsy's report table columns per artifact type, generated from
//! `TableReportGenerator.getArtifactTableColumns` and its bundle by
//! `scripts/gen/report_columns.py`. Do not edit by hand.

use crate::table::Col;

/// `(artifact types, leading columns, attribute types left out)`.
pub(crate) const TYPE_COLUMNS: &[(&[&str], &[Col], &[&str])] = &[
    (
        &["TSK_WEB_BOOKMARK"],
        &[
            Col::Attr("URL", "TSK_URL"),
            Col::Attr("Title", "TSK_TITLE"),
            Col::Attr("Date Created", "TSK_DATETIME_CREATED"),
            Col::Attr("Program", "TSK_PROG_NAME"),
        ],
        &[],
    ),
    (
        &["TSK_WEB_COOKIE"],
        &[
            Col::Attr("URL", "TSK_URL"),
            Col::Attr("Date/Time", "TSK_DATETIME"),
            Col::Attr("Name", "TSK_NAME"),
            Col::Attr("Value", "TSK_VALUE"),
            Col::Attr("Program", "TSK_PROG_NAME"),
        ],
        &[],
    ),
    (
        &["TSK_WEB_HISTORY"],
        &[
            Col::Attr("URL", "TSK_URL"),
            Col::Attr("Date Accessed", "TSK_DATETIME_ACCESSED"),
            Col::Attr("Referrer", "TSK_REFERRER"),
            Col::Attr("Title", "TSK_TITLE"),
            Col::Attr("Program", "TSK_PROG_NAME"),
            Col::Attr("URL Domain", "TSK_URL_DECODED"),
        ],
        &[],
    ),
    (
        &["TSK_WEB_DOWNLOAD"],
        &[
            Col::Attr("Destination", "TSK_PATH"),
            Col::Attr("Source URL", "TSK_URL"),
            Col::Attr("Date Accessed", "TSK_DATETIME_ACCESSED"),
            Col::Attr("Program", "TSK_PROG_NAME"),
        ],
        &["TSK_PATH_ID"],
    ),
    (
        &["TSK_RECENT_OBJECT"],
        &[
            Col::Attr("Path", "TSK_PATH"),
            Col::Attr("Date/Time", "TSK_DATETIME_ACCESSED"),
        ],
        &["TSK_PATH_ID"],
    ),
    (
        &["TSK_INSTALLED_PROG"],
        &[
            Col::Attr("Program Name", "TSK_PROG_NAME"),
            Col::Attr("Install Date/Time", "TSK_DATETIME"),
        ],
        &[],
    ),
    (&["TSK_KEYWORD_HIT"], &[Col::Special("Preview")], &[]),
    (
        &["TSK_HASHSET_HIT"],
        &[Col::SourceFile("File"), Col::Special("Size")],
        &[],
    ),
    (
        &["TSK_DEVICE_ATTACHED"],
        &[
            Col::Attr("Device Make", "TSK_DEVICE_MAKE"),
            Col::Attr("Device Model", "TSK_DEVICE_MODEL"),
            Col::Attr("Device ID", "TSK_DEVICE_ID"),
            Col::Attr("Date/Time", "TSK_DATETIME"),
        ],
        &[],
    ),
    (
        &["TSK_WEB_SEARCH_QUERY"],
        &[
            Col::Attr("Text", "TSK_TEXT"),
            Col::Attr("Domain", "TSK_DOMAIN"),
            Col::Attr("Date Accessed", "TSK_DATETIME_ACCESSED"),
            Col::Attr("Program Name", "TSK_PROG_NAME"),
        ],
        &[],
    ),
    (
        &["TSK_METADATA_EXIF"],
        &[
            Col::Attr("Date Taken", "TSK_DATETIME_CREATED"),
            Col::Attr("Device Manufacturer", "TSK_DEVICE_MAKE"),
            Col::Attr("Device Model", "TSK_DEVICE_MODEL"),
            Col::Attr("Latitude", "TSK_GEO_LATITUDE"),
            Col::Attr("Longitude", "TSK_GEO_LONGITUDE"),
            Col::Attr("Altitude", "TSK_GEO_ALTITUDE"),
        ],
        &[],
    ),
    (
        &["TSK_CONTACT"],
        &[
            Col::Attr("Person Name", "TSK_NAME"),
            Col::Attr("Phone Number", "TSK_PHONE_NUMBER"),
            Col::Attr("Phone Number (Home)", "TSK_PHONE_NUMBER_HOME"),
            Col::Attr("Phone Number (Office)", "TSK_PHONE_NUMBER_OFFICE"),
            Col::Attr("Phone Number (Mobile)", "TSK_PHONE_NUMBER_MOBILE"),
            Col::Attr("Email", "TSK_EMAIL"),
        ],
        &[],
    ),
    (
        &["TSK_MESSAGE"],
        &[
            Col::Attr("Message Type", "TSK_MESSAGE_TYPE"),
            Col::Attr("Direction", "TSK_DIRECTION"),
            Col::Attr("Read Status", "TSK_READ_STATUS"),
            Col::Attr("Date/Time", "TSK_DATETIME"),
            Col::Attr("From Phone Number", "TSK_PHONE_NUMBER_FROM"),
            Col::Attr("From Email", "TSK_EMAIL_FROM"),
            Col::Attr("To Phone Number", "TSK_PHONE_NUMBER_TO"),
            Col::Attr("To Email", "TSK_EMAIL_TO"),
            Col::Attr("Subject", "TSK_SUBJECT"),
            Col::Attr("Text", "TSK_TEXT"),
        ],
        &[],
    ),
    (
        &["TSK_CALLLOG"],
        &[
            Col::Attr("Person Name", "TSK_NAME"),
            Col::Attr("From Phone Number", "TSK_PHONE_NUMBER_FROM"),
            Col::Attr("To Phone Number", "TSK_PHONE_NUMBER_TO"),
            Col::Attr("Date/Time", "TSK_DATETIME_START"),
            Col::Attr("Direction", "TSK_DIRECTION"),
        ],
        &[],
    ),
    (
        &["TSK_CALENDAR_ENTRY"],
        &[
            Col::Attr("Calendar Entry Type", "TSK_CALENDAR_ENTRY_TYPE"),
            Col::Attr("Description", "TSK_DESCRIPTION"),
            Col::Attr("Start Date/Time", "TSK_DATETIME_START"),
            Col::Attr("End Date/Time", "TSK_DATETIME_END"),
            Col::Attr("Location", "TSK_LOCATION"),
        ],
        &[],
    ),
    (
        &["TSK_SPEED_DIAL_ENTRY"],
        &[
            Col::Attr("Short Cut", "TSK_SHORTCUT"),
            Col::Attr("Person Name", "TSK_NAME_PERSON"),
            Col::Attr("Phone Number", "TSK_PHONE_NUMBER"),
        ],
        &[],
    ),
    (
        &["TSK_BLUETOOTH_PAIRING"],
        &[
            Col::Attr("Device Name", "TSK_DEVICE_NAME"),
            Col::Attr("Device Address", "TSK_DEVICE_ID"),
            Col::Attr("Date/Time", "TSK_DATETIME"),
        ],
        &[],
    ),
    (
        &["TSK_GPS_TRACKPOINT"],
        &[
            Col::Attr("Latitude", "TSK_GEO_LATITUDE"),
            Col::Attr("Longitude", "TSK_GEO_LONGITUDE"),
            Col::Attr("Date/Time", "TSK_DATETIME"),
        ],
        &[],
    ),
    (
        &["TSK_GPS_BOOKMARK"],
        &[
            Col::Attr("Latitude", "TSK_GEO_LATITUDE"),
            Col::Attr("Longitude", "TSK_GEO_LONGITUDE"),
            Col::Attr("Altitude", "TSK_GEO_ALTITUDE"),
            Col::Attr("Name", "TSK_NAME"),
            Col::Attr("Location Address", "TSK_LOCATION"),
            Col::Attr("Date/Time", "TSK_DATETIME"),
        ],
        &[],
    ),
    (
        &["TSK_GPS_LAST_KNOWN_LOCATION"],
        &[
            Col::Attr("Latitude", "TSK_GEO_LATITUDE"),
            Col::Attr("Longitude", "TSK_GEO_LONGITUDE"),
            Col::Attr("Altitude", "TSK_GEO_ALTITUDE"),
            Col::Attr("Name", "TSK_NAME"),
            Col::Attr("Location Address", "TSK_LOCATION"),
            Col::Attr("Date/Time", "TSK_DATETIME"),
        ],
        &[],
    ),
    (
        &["TSK_GPS_SEARCH"],
        &[
            Col::Attr("Latitude", "TSK_GEO_LATITUDE"),
            Col::Attr("Longitude", "TSK_GEO_LONGITUDE"),
            Col::Attr("Altitude", "TSK_GEO_ALTITUDE"),
            Col::Attr("Name", "TSK_NAME"),
            Col::Attr("Location Address", "TSK_LOCATION"),
            Col::Attr("Date/Time", "TSK_DATETIME"),
        ],
        &[],
    ),
    (
        &["TSK_SERVICE_ACCOUNT"],
        &[
            Col::Attr("Category", "TSK_CATEGORY"),
            Col::Attr("User ID", "TSK_USER_ID"),
            Col::Attr("Password", "TSK_PASSWORD"),
            Col::Attr("Person Name", "TSK_NAME"),
            Col::Attr("App Name", "TSK_PROG_NAME"),
            Col::Attr("URL", "TSK_URL"),
            Col::Attr("App Path", "TSK_PATH"),
            Col::Attr("Description", "TSK_DESCRIPTION"),
            Col::Attr("ReplyTo Address", "TSK_EMAIL_REPLYTO"),
            Col::Attr("Mail Server", "TSK_SERVER_NAME"),
        ],
        &[],
    ),
    (
        &["TSK_ENCRYPTION_DETECTED", "TSK_ENCRYPTION_SUSPECTED"],
        &[Col::Attr("Name", "TSK_NAME")],
        &[],
    ),
    (
        &["TSK_EXT_MISMATCH_DETECTED"],
        &[
            Col::Special("File"),
            Col::Special("Extension"),
            Col::Special("MIME Type"),
            Col::Special("Path"),
        ],
        &[],
    ),
    (
        &["TSK_OS_INFO"],
        &[
            Col::Attr("Processor Architecture", "TSK_PROCESSOR_ARCHITECTURE"),
            Col::Attr("Operating System Name", "TSK_PROG_NAME"),
            Col::Attr("Install Date", "TSK_DATETIME"),
        ],
        &[],
    ),
    (
        &["TSK_EMAIL_MSG"],
        &[
            Col::Attr("E-Mail To", "TSK_EMAIL_TO"),
            Col::Attr("E-Mail From", "TSK_EMAIL_FROM"),
            Col::Attr("Subject", "TSK_SUBJECT"),
            Col::Attr("Date Sent", "TSK_DATETIME_SENT"),
            Col::Attr("Date Received", "TSK_DATETIME_RCVD"),
            Col::Attr("Path", "TSK_PATH"),
            Col::Attr("E-Mail CC", "TSK_EMAIL_CC"),
            Col::Attr("E-Mail BCC", "TSK_EMAIL_BCC"),
            Col::Attr("Message ID", "TSK_MSG_ID"),
        ],
        &[],
    ),
    (
        &["TSK_INTERESTING_FILE_HIT"],
        &[
            Col::Attr("Set Name", "TSK_SET_NAME"),
            Col::Attr("Rule", "TSK_CATEGORY"),
            Col::Attr("Path", "TSK_PATH"),
            Col::Attr("Comment", "TSK_COMMENT"),
            Col::Attr("Description", "TSK_DESCRIPTION"),
        ],
        &[],
    ),
    (
        &["TSK_GPS_ROUTE"],
        &[
            Col::Attr("Category", "TSK_CATEGORY"),
            Col::Attr("Date/Time", "TSK_DATETIME"),
            Col::Attr("Ending Latitude", "TSK_GEO_LATITUDE_END"),
            Col::Attr("Ending Longitude", "TSK_GEO_LONGITUDE_END"),
            Col::Attr("Starting Latitude", "TSK_GEO_LATITUDE_START"),
            Col::Attr("Starting Longitude", "TSK_GEO_LONGITUDE_START"),
            Col::Attr("Name", "TSK_NAME"),
            Col::Attr("Location", "TSK_LOCATION"),
            Col::Attr("Program", "TSK_PROG_NAME"),
        ],
        &[],
    ),
    (
        &["TSK_INTERESTING_ARTIFACT_HIT"],
        &[
            Col::Attr("Set Name", "TSK_SET_NAME"),
            Col::Attr("Associated Artifact", "TSK_ASSOCIATED_ARTIFACT"),
            Col::Attr("Program", "TSK_PROG_NAME"),
        ],
        &[],
    ),
    (
        &["TSK_INTERESTING_ITEM"],
        &[
            Col::Attr("Set Name", "TSK_SET_NAME"),
            Col::Attr("Associated Artifact", "TSK_ASSOCIATED_ARTIFACT"),
            Col::Attr("Program", "TSK_PROG_NAME"),
            Col::Attr("Rule", "TSK_CATEGORY"),
            Col::Attr("Path", "TSK_PATH"),
            Col::Attr("Comment", "TSK_COMMENT"),
            Col::Attr("Description", "TSK_DESCRIPTION"),
        ],
        &[],
    ),
    (
        &["TSK_PROG_RUN"],
        &[
            Col::Attr("Program", "TSK_PROG_NAME"),
            Col::Attr("Associated Artifact", "TSK_ASSOCIATED_ARTIFACT"),
            Col::Attr("Date/Time", "TSK_DATETIME"),
            Col::Attr("Count", "TSK_COUNT"),
        ],
        &[],
    ),
    (
        &["TSK_OS_ACCOUNT"],
        &[
            Col::Attr("User Name", "TSK_USER_NAME"),
            Col::Attr("User ID", "TSK_USER_ID"),
        ],
        &[],
    ),
    (
        &["TSK_REMOTE_DRIVE"],
        &[
            Col::Attr("Local path", "TSK_LOCAL_PATH"),
            Col::Attr("Remote path", "TSK_REMOTE_PATH"),
        ],
        &[],
    ),
    (
        &["TSK_ACCOUNT"],
        &[Col::Status],
        &[
            "TSK_ACCOUNT_TYPE",
            "TSK_ASSOCIATED_ARTIFACT",
            "TSK_SET_NAME",
            "TSK_KEYWORD_SEARCH_DOCUMENT_ID",
        ],
    ),
    (&["TSK_WEB_CACHE"], &[], &["TSK_PATH_ID"]),
];

/// Types whose table ends with the source file column (before Tags).
pub(crate) const SOURCE_FILE_LAST: &[&str] = &[
    "TSK_WEB_BOOKMARK",
    "TSK_WEB_COOKIE",
    "TSK_WEB_HISTORY",
    "TSK_WEB_DOWNLOAD",
    "TSK_RECENT_OBJECT",
    "TSK_INSTALLED_PROG",
    "TSK_DEVICE_ATTACHED",
    "TSK_WEB_SEARCH_QUERY",
    "TSK_METADATA_EXIF",
    "TSK_CONTACT",
    "TSK_MESSAGE",
    "TSK_CALLLOG",
    "TSK_CALENDAR_ENTRY",
    "TSK_SPEED_DIAL_ENTRY",
    "TSK_BLUETOOTH_PAIRING",
    "TSK_GPS_TRACKPOINT",
    "TSK_GPS_BOOKMARK",
    "TSK_GPS_LAST_KNOWN_LOCATION",
    "TSK_GPS_SEARCH",
    "TSK_GPS_AREA",
    "TSK_SERVICE_ACCOUNT",
    "TSK_ENCRYPTION_DETECTED",
    "TSK_ENCRYPTION_SUSPECTED",
    "TSK_OS_INFO",
];

pub(crate) const SOURCE_FILE: &str = "Source File";
pub(crate) const TAGS: &str = "Tags";
