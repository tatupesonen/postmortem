//! Autopsy's report table columns per artifact type, generated from
//! `TableReportGenerator.getArtifactTableColumns` and its bundle by
//! `scripts/gen/report_columns.py`. Do not edit by hand.

use crate::table::Col;

/// `(artifact types, leading columns, attribute types left out)`.
pub(crate) const TYPE_COLUMNS: &[(&[&str], &[Col], &[&str])] = &[
    (
        &["KAAMOS_WEB_BOOKMARK"],
        &[
            Col::Attr("URL", "KAAMOS_URL"),
            Col::Attr("Title", "KAAMOS_TITLE"),
            Col::Attr("Date Created", "KAAMOS_DATETIME_CREATED"),
            Col::Attr("Program", "KAAMOS_PROG_NAME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_WEB_COOKIE"],
        &[
            Col::Attr("URL", "KAAMOS_URL"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME"),
            Col::Attr("Name", "KAAMOS_NAME"),
            Col::Attr("Value", "KAAMOS_VALUE"),
            Col::Attr("Program", "KAAMOS_PROG_NAME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_WEB_HISTORY"],
        &[
            Col::Attr("URL", "KAAMOS_URL"),
            Col::Attr("Date Accessed", "KAAMOS_DATETIME_ACCESSED"),
            Col::Attr("Referrer", "KAAMOS_REFERRER"),
            Col::Attr("Title", "KAAMOS_TITLE"),
            Col::Attr("Program", "KAAMOS_PROG_NAME"),
            Col::Attr("URL Domain", "KAAMOS_URL_DECODED"),
        ],
        &[],
    ),
    (
        &["KAAMOS_WEB_DOWNLOAD"],
        &[
            Col::Attr("Destination", "KAAMOS_PATH"),
            Col::Attr("Source URL", "KAAMOS_URL"),
            Col::Attr("Date Accessed", "KAAMOS_DATETIME_ACCESSED"),
            Col::Attr("Program", "KAAMOS_PROG_NAME"),
        ],
        &["KAAMOS_PATH_ID"],
    ),
    (
        &["KAAMOS_RECENT_OBJECT"],
        &[
            Col::Attr("Path", "KAAMOS_PATH"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME_ACCESSED"),
        ],
        &["KAAMOS_PATH_ID"],
    ),
    (
        &["KAAMOS_INSTALLED_PROG"],
        &[
            Col::Attr("Program Name", "KAAMOS_PROG_NAME"),
            Col::Attr("Install Date/Time", "KAAMOS_DATETIME"),
        ],
        &[],
    ),
    (&["KAAMOS_KEYWORD_HIT"], &[Col::Special("Preview")], &[]),
    (
        &["KAAMOS_HASHSET_HIT"],
        &[Col::SourceFile("File"), Col::Special("Size")],
        &[],
    ),
    (
        &["KAAMOS_DEVICE_ATTACHED"],
        &[
            Col::Attr("Device Make", "KAAMOS_DEVICE_MAKE"),
            Col::Attr("Device Model", "KAAMOS_DEVICE_MODEL"),
            Col::Attr("Device ID", "KAAMOS_DEVICE_ID"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_WEB_SEARCH_QUERY"],
        &[
            Col::Attr("Text", "KAAMOS_TEXT"),
            Col::Attr("Domain", "KAAMOS_DOMAIN"),
            Col::Attr("Date Accessed", "KAAMOS_DATETIME_ACCESSED"),
            Col::Attr("Program Name", "KAAMOS_PROG_NAME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_METADATA_EXIF"],
        &[
            Col::Attr("Date Taken", "KAAMOS_DATETIME_CREATED"),
            Col::Attr("Device Manufacturer", "KAAMOS_DEVICE_MAKE"),
            Col::Attr("Device Model", "KAAMOS_DEVICE_MODEL"),
            Col::Attr("Latitude", "KAAMOS_GEO_LATITUDE"),
            Col::Attr("Longitude", "KAAMOS_GEO_LONGITUDE"),
            Col::Attr("Altitude", "KAAMOS_GEO_ALTITUDE"),
        ],
        &[],
    ),
    (
        &["KAAMOS_CONTACT"],
        &[
            Col::Attr("Person Name", "KAAMOS_NAME"),
            Col::Attr("Phone Number", "KAAMOS_PHONE_NUMBER"),
            Col::Attr("Phone Number (Home)", "KAAMOS_PHONE_NUMBER_HOME"),
            Col::Attr("Phone Number (Office)", "KAAMOS_PHONE_NUMBER_OFFICE"),
            Col::Attr("Phone Number (Mobile)", "KAAMOS_PHONE_NUMBER_MOBILE"),
            Col::Attr("Email", "KAAMOS_EMAIL"),
        ],
        &[],
    ),
    (
        &["KAAMOS_MESSAGE"],
        &[
            Col::Attr("Message Type", "KAAMOS_MESSAGE_TYPE"),
            Col::Attr("Direction", "KAAMOS_DIRECTION"),
            Col::Attr("Read Status", "KAAMOS_READ_STATUS"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME"),
            Col::Attr("From Phone Number", "KAAMOS_PHONE_NUMBER_FROM"),
            Col::Attr("From Email", "KAAMOS_EMAIL_FROM"),
            Col::Attr("To Phone Number", "KAAMOS_PHONE_NUMBER_TO"),
            Col::Attr("To Email", "KAAMOS_EMAIL_TO"),
            Col::Attr("Subject", "KAAMOS_SUBJECT"),
            Col::Attr("Text", "KAAMOS_TEXT"),
        ],
        &[],
    ),
    (
        &["KAAMOS_CALLLOG"],
        &[
            Col::Attr("Person Name", "KAAMOS_NAME"),
            Col::Attr("From Phone Number", "KAAMOS_PHONE_NUMBER_FROM"),
            Col::Attr("To Phone Number", "KAAMOS_PHONE_NUMBER_TO"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME_START"),
            Col::Attr("Direction", "KAAMOS_DIRECTION"),
        ],
        &[],
    ),
    (
        &["KAAMOS_CALENDAR_ENTRY"],
        &[
            Col::Attr("Calendar Entry Type", "KAAMOS_CALENDAR_ENTRY_TYPE"),
            Col::Attr("Description", "KAAMOS_DESCRIPTION"),
            Col::Attr("Start Date/Time", "KAAMOS_DATETIME_START"),
            Col::Attr("End Date/Time", "KAAMOS_DATETIME_END"),
            Col::Attr("Location", "KAAMOS_LOCATION"),
        ],
        &[],
    ),
    (
        &["KAAMOS_SPEED_DIAL_ENTRY"],
        &[
            Col::Attr("Short Cut", "KAAMOS_SHORTCUT"),
            Col::Attr("Person Name", "KAAMOS_NAME_PERSON"),
            Col::Attr("Phone Number", "KAAMOS_PHONE_NUMBER"),
        ],
        &[],
    ),
    (
        &["KAAMOS_BLUETOOTH_PAIRING"],
        &[
            Col::Attr("Device Name", "KAAMOS_DEVICE_NAME"),
            Col::Attr("Device Address", "KAAMOS_DEVICE_ID"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_GPS_TRACKPOINT"],
        &[
            Col::Attr("Latitude", "KAAMOS_GEO_LATITUDE"),
            Col::Attr("Longitude", "KAAMOS_GEO_LONGITUDE"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_GPS_BOOKMARK"],
        &[
            Col::Attr("Latitude", "KAAMOS_GEO_LATITUDE"),
            Col::Attr("Longitude", "KAAMOS_GEO_LONGITUDE"),
            Col::Attr("Altitude", "KAAMOS_GEO_ALTITUDE"),
            Col::Attr("Name", "KAAMOS_NAME"),
            Col::Attr("Location Address", "KAAMOS_LOCATION"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_GPS_LAST_KNOWN_LOCATION"],
        &[
            Col::Attr("Latitude", "KAAMOS_GEO_LATITUDE"),
            Col::Attr("Longitude", "KAAMOS_GEO_LONGITUDE"),
            Col::Attr("Altitude", "KAAMOS_GEO_ALTITUDE"),
            Col::Attr("Name", "KAAMOS_NAME"),
            Col::Attr("Location Address", "KAAMOS_LOCATION"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_GPS_SEARCH"],
        &[
            Col::Attr("Latitude", "KAAMOS_GEO_LATITUDE"),
            Col::Attr("Longitude", "KAAMOS_GEO_LONGITUDE"),
            Col::Attr("Altitude", "KAAMOS_GEO_ALTITUDE"),
            Col::Attr("Name", "KAAMOS_NAME"),
            Col::Attr("Location Address", "KAAMOS_LOCATION"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_SERVICE_ACCOUNT"],
        &[
            Col::Attr("Category", "KAAMOS_CATEGORY"),
            Col::Attr("User ID", "KAAMOS_USER_ID"),
            Col::Attr("Password", "KAAMOS_PASSWORD"),
            Col::Attr("Person Name", "KAAMOS_NAME"),
            Col::Attr("App Name", "KAAMOS_PROG_NAME"),
            Col::Attr("URL", "KAAMOS_URL"),
            Col::Attr("App Path", "KAAMOS_PATH"),
            Col::Attr("Description", "KAAMOS_DESCRIPTION"),
            Col::Attr("ReplyTo Address", "KAAMOS_EMAIL_REPLYTO"),
            Col::Attr("Mail Server", "KAAMOS_SERVER_NAME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_ENCRYPTION_DETECTED", "KAAMOS_ENCRYPTION_SUSPECTED"],
        &[Col::Attr("Name", "KAAMOS_NAME")],
        &[],
    ),
    (
        &["KAAMOS_EXT_MISMATCH_DETECTED"],
        &[
            Col::Special("File"),
            Col::Special("Extension"),
            Col::Special("MIME Type"),
            Col::Special("Path"),
        ],
        &[],
    ),
    (
        &["KAAMOS_OS_INFO"],
        &[
            Col::Attr("Processor Architecture", "KAAMOS_PROCESSOR_ARCHITECTURE"),
            Col::Attr("Operating System Name", "KAAMOS_PROG_NAME"),
            Col::Attr("Install Date", "KAAMOS_DATETIME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_EMAIL_MSG"],
        &[
            Col::Attr("E-Mail To", "KAAMOS_EMAIL_TO"),
            Col::Attr("E-Mail From", "KAAMOS_EMAIL_FROM"),
            Col::Attr("Subject", "KAAMOS_SUBJECT"),
            Col::Attr("Date Sent", "KAAMOS_DATETIME_SENT"),
            Col::Attr("Date Received", "KAAMOS_DATETIME_RCVD"),
            Col::Attr("Path", "KAAMOS_PATH"),
            Col::Attr("E-Mail CC", "KAAMOS_EMAIL_CC"),
            Col::Attr("E-Mail BCC", "KAAMOS_EMAIL_BCC"),
            Col::Attr("Message ID", "KAAMOS_MSG_ID"),
        ],
        &[],
    ),
    (
        &["KAAMOS_INTERESTING_FILE_HIT"],
        &[
            Col::Attr("Set Name", "KAAMOS_SET_NAME"),
            Col::Attr("Rule", "KAAMOS_CATEGORY"),
            Col::Attr("Path", "KAAMOS_PATH"),
            Col::Attr("Comment", "KAAMOS_COMMENT"),
            Col::Attr("Description", "KAAMOS_DESCRIPTION"),
        ],
        &[],
    ),
    (
        &["KAAMOS_GPS_ROUTE"],
        &[
            Col::Attr("Category", "KAAMOS_CATEGORY"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME"),
            Col::Attr("Ending Latitude", "KAAMOS_GEO_LATITUDE_END"),
            Col::Attr("Ending Longitude", "KAAMOS_GEO_LONGITUDE_END"),
            Col::Attr("Starting Latitude", "KAAMOS_GEO_LATITUDE_START"),
            Col::Attr("Starting Longitude", "KAAMOS_GEO_LONGITUDE_START"),
            Col::Attr("Name", "KAAMOS_NAME"),
            Col::Attr("Location", "KAAMOS_LOCATION"),
            Col::Attr("Program", "KAAMOS_PROG_NAME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_INTERESTING_ARTIFACT_HIT"],
        &[
            Col::Attr("Set Name", "KAAMOS_SET_NAME"),
            Col::Attr("Associated Artifact", "KAAMOS_ASSOCIATED_ARTIFACT"),
            Col::Attr("Program", "KAAMOS_PROG_NAME"),
        ],
        &[],
    ),
    (
        &["KAAMOS_INTERESTING_ITEM"],
        &[
            Col::Attr("Set Name", "KAAMOS_SET_NAME"),
            Col::Attr("Associated Artifact", "KAAMOS_ASSOCIATED_ARTIFACT"),
            Col::Attr("Program", "KAAMOS_PROG_NAME"),
            Col::Attr("Rule", "KAAMOS_CATEGORY"),
            Col::Attr("Path", "KAAMOS_PATH"),
            Col::Attr("Comment", "KAAMOS_COMMENT"),
            Col::Attr("Description", "KAAMOS_DESCRIPTION"),
        ],
        &[],
    ),
    (
        &["KAAMOS_PROG_RUN"],
        &[
            Col::Attr("Program", "KAAMOS_PROG_NAME"),
            Col::Attr("Associated Artifact", "KAAMOS_ASSOCIATED_ARTIFACT"),
            Col::Attr("Date/Time", "KAAMOS_DATETIME"),
            Col::Attr("Count", "KAAMOS_COUNT"),
        ],
        &[],
    ),
    (
        &["KAAMOS_OS_ACCOUNT"],
        &[
            Col::Attr("User Name", "KAAMOS_USER_NAME"),
            Col::Attr("User ID", "KAAMOS_USER_ID"),
        ],
        &[],
    ),
    (
        &["KAAMOS_REMOTE_DRIVE"],
        &[
            Col::Attr("Local path", "KAAMOS_LOCAL_PATH"),
            Col::Attr("Remote path", "KAAMOS_REMOTE_PATH"),
        ],
        &[],
    ),
    (
        &["KAAMOS_ACCOUNT"],
        &[Col::Status],
        &[
            "KAAMOS_ACCOUNT_TYPE",
            "KAAMOS_ASSOCIATED_ARTIFACT",
            "KAAMOS_SET_NAME",
            "KAAMOS_KEYWORD_SEARCH_DOCUMENT_ID",
        ],
    ),
    (&["KAAMOS_WEB_CACHE"], &[], &["KAAMOS_PATH_ID"]),
];

/// Types whose table ends with the source file column (before Tags).
pub(crate) const SOURCE_FILE_LAST: &[&str] = &[
    "KAAMOS_WEB_BOOKMARK",
    "KAAMOS_WEB_COOKIE",
    "KAAMOS_WEB_HISTORY",
    "KAAMOS_WEB_DOWNLOAD",
    "KAAMOS_RECENT_OBJECT",
    "KAAMOS_INSTALLED_PROG",
    "KAAMOS_DEVICE_ATTACHED",
    "KAAMOS_WEB_SEARCH_QUERY",
    "KAAMOS_METADATA_EXIF",
    "KAAMOS_CONTACT",
    "KAAMOS_MESSAGE",
    "KAAMOS_CALLLOG",
    "KAAMOS_CALENDAR_ENTRY",
    "KAAMOS_SPEED_DIAL_ENTRY",
    "KAAMOS_BLUETOOTH_PAIRING",
    "KAAMOS_GPS_TRACKPOINT",
    "KAAMOS_GPS_BOOKMARK",
    "KAAMOS_GPS_LAST_KNOWN_LOCATION",
    "KAAMOS_GPS_SEARCH",
    "KAAMOS_GPS_AREA",
    "KAAMOS_SERVICE_ACCOUNT",
    "KAAMOS_ENCRYPTION_DETECTED",
    "KAAMOS_ENCRYPTION_SUSPECTED",
    "KAAMOS_OS_INFO",
];

pub(crate) const SOURCE_FILE: &str = "Source File";
pub(crate) const TAGS: &str = "Tags";
