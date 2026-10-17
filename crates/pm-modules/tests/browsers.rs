#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

//! End-to-end: Recent Activity's web extractors on a generated NTFS
//! volume holding Chrome (two profiles, both download schemas, WAL
//! companion, disk cache), Firefox, Internet Explorer and Safari stores
//! built at test time with the browsers' real table layouts.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::DemoCase;
use pm_browser::binarycookies::testing as bc;
use pm_browser::chrome_cache::testing as cc;
use pm_browser::index_dat::testing as idx;
use pm_ingest::MessageKind;
use pm_modules::recent_activity::RecentActivityFactory;
use pm_testkit::ntfs::{FileOpts, NtfsBuilder};

/// WebKit µs for a Unix time.
fn wk(unix: i64) -> i64 {
    (unix + 11_644_473_600) * 1_000_000
}

/// FILETIME for a Unix time.
fn ft(unix: i64) -> u64 {
    ((unix + 11_644_473_600) * 10_000_000) as u64
}

/// An SQLite database file built by `sql`.
fn sqlite(sql: &str) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.db");
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch(sql).unwrap();
    drop(c);
    std::fs::read(&p).unwrap()
}

/// A WAL-mode database: the main file holds `base`, the WAL `extra`
/// (never checkpointed). Returns `(main, wal)`.
fn sqlite_wal(base: &str, extra: &str) -> (Vec<u8>, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.db");
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch(base).unwrap();
    c.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
        .unwrap();
    c.execute_batch("PRAGMA wal_autocheckpoint=0;").unwrap();
    c.execute_batch(extra).unwrap();
    let main = std::fs::read(&p).unwrap();
    let wal = std::fs::read(dir.path().join("x.db-wal")).unwrap();
    drop(c);
    (main, wal)
}

const CHROME_HISTORY: &str = "
CREATE TABLE urls(id INTEGER PRIMARY KEY AUTOINCREMENT,url LONGVARCHAR,title LONGVARCHAR,visit_count INTEGER DEFAULT 0 NOT NULL,typed_count INTEGER DEFAULT 0 NOT NULL,last_visit_time INTEGER NOT NULL,hidden INTEGER DEFAULT 0 NOT NULL);
CREATE TABLE visits(id INTEGER PRIMARY KEY,url INTEGER NOT NULL,visit_time INTEGER NOT NULL,from_visit INTEGER,transition INTEGER DEFAULT 0 NOT NULL,segment_id INTEGER,visit_duration INTEGER DEFAULT 0 NOT NULL,incremented_omnibox_typed_score BOOLEAN DEFAULT FALSE NOT NULL);
CREATE TABLE downloads (id INTEGER PRIMARY KEY,guid VARCHAR NOT NULL,current_path LONGVARCHAR NOT NULL,target_path LONGVARCHAR NOT NULL,start_time INTEGER NOT NULL,received_bytes INTEGER NOT NULL,total_bytes INTEGER NOT NULL,state INTEGER NOT NULL,danger_type INTEGER NOT NULL,interrupt_reason INTEGER NOT NULL,hash BLOB NOT NULL,end_time INTEGER NOT NULL,opened INTEGER NOT NULL,last_access_time INTEGER NOT NULL,transient INTEGER NOT NULL,referrer VARCHAR NOT NULL,site_url VARCHAR NOT NULL,tab_url VARCHAR NOT NULL,tab_referrer_url VARCHAR NOT NULL,http_method VARCHAR NOT NULL,by_ext_id VARCHAR NOT NULL,by_ext_name VARCHAR NOT NULL,etag VARCHAR NOT NULL,last_modified VARCHAR NOT NULL,mime_type VARCHAR(255) NOT NULL,original_mime_type VARCHAR(255) NOT NULL);
CREATE TABLE downloads_url_chains (id INTEGER NOT NULL,chain_index INTEGER NOT NULL,url LONGVARCHAR NOT NULL, PRIMARY KEY (id, chain_index));
";

fn chrome_history() -> (Vec<u8>, Vec<u8>) {
    let base = format!(
        "{CHROME_HISTORY}
        INSERT INTO urls VALUES (1,'https://www.google.com/search?q=forensic+tools&oq=x','forensic tools - Google Search',1,0,{t2},0);
        INSERT INTO urls VALUES (2,'https://www.example.com/','Example',2,1,{t3},0);
        INSERT INTO urls VALUES (3,'https://forum.example.org/admin/index.php','Admin',1,0,{t3},0);
        INSERT INTO visits VALUES (1,2,{t1},0,805306368,0,0,0);
        INSERT INTO visits VALUES (2,1,{t2},1,0,0,0,0);
        INSERT INTO visits VALUES (3,2,{t3},2,0,0,0,0);
        INSERT INTO visits VALUES (6,3,{t3},0,0,0,0,0);
        INSERT INTO downloads VALUES (1,'g','C:\\Users\\bob\\Downloads\\setup.exe','C:\\Users\\bob\\Downloads\\setup.exe',{t2},10,10,1,0,0,x'',{t2},0,0,0,'','','','','','','','','','application/octet-stream','');
        INSERT INTO downloads_url_chains VALUES (1,0,'http://dl.example.net/redirect');
        INSERT INTO downloads_url_chains VALUES (1,1,'https://cdn.example.net/setup.exe');",
        t1 = wk(1_600_000_000),
        t2 = wk(1_600_000_100),
        t3 = wk(1_600_000_200),
    );
    // A visit committed but still in the WAL (Autopsy loses it).
    let extra = format!(
        "INSERT INTO urls VALUES (4,'https://wal.example.com/','In the WAL',1,0,{t},0);
         INSERT INTO visits VALUES (4,4,{t},0,0,0,0,0);
         INSERT INTO urls VALUES (5,'http://forum.example.org/forum/ucp.php?mode=login','UCP',1,0,{t},0);
         INSERT INTO visits VALUES (5,5,{t},0,0,0,0,0);",
        t = wk(1_600_000_300)
    );
    sqlite_wal(&base, &extra)
}

/// Chrome before v30: downloads keep their URL inline.
fn chrome_history_pre30() -> Vec<u8> {
    sqlite(&format!(
        "CREATE TABLE urls(id INTEGER PRIMARY KEY,url LONGVARCHAR,title LONGVARCHAR,visit_count INTEGER DEFAULT 0 NOT NULL,typed_count INTEGER DEFAULT 0 NOT NULL,last_visit_time INTEGER NOT NULL,hidden INTEGER DEFAULT 0 NOT NULL,favicon_id INTEGER DEFAULT 0 NOT NULL);
         CREATE TABLE visits(id INTEGER PRIMARY KEY,url INTEGER NOT NULL,visit_time INTEGER NOT NULL,from_visit INTEGER,transition INTEGER DEFAULT 0 NOT NULL,segment_id INTEGER,is_indexed BOOLEAN);
         CREATE TABLE downloads (id INTEGER PRIMARY KEY,full_path LONGVARCHAR NOT NULL,url LONGVARCHAR NOT NULL,start_time INTEGER NOT NULL,received_bytes INTEGER NOT NULL,total_bytes INTEGER NOT NULL,state INTEGER NOT NULL,end_time INTEGER NOT NULL,opened INTEGER NOT NULL);
         INSERT INTO urls VALUES (1,'https://work.example.com/','Work',1,0,{t},0,0);
         INSERT INTO visits VALUES (1,1,{t},0,0,0,1);
         INSERT INTO downloads VALUES (1,'C:\\Users\\bob\\Downloads\\report.pdf','https://work.example.com/report.pdf',{t},5,5,1,{t},0);",
        t = wk(1_500_000_000)
    ))
}

fn chrome_cookies() -> Vec<u8> {
    sqlite(&format!(
        "CREATE TABLE meta(key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY, value LONGVARCHAR);
         CREATE TABLE cookies(creation_utc INTEGER NOT NULL,host_key TEXT NOT NULL,top_frame_site_key TEXT NOT NULL,name TEXT NOT NULL,value TEXT NOT NULL,encrypted_value BLOB NOT NULL,path TEXT NOT NULL,expires_utc INTEGER NOT NULL,is_secure INTEGER NOT NULL,is_httponly INTEGER NOT NULL,last_access_utc INTEGER NOT NULL,has_expires INTEGER NOT NULL,is_persistent INTEGER NOT NULL,priority INTEGER NOT NULL,samesite INTEGER NOT NULL,source_scheme INTEGER NOT NULL,source_port INTEGER NOT NULL,last_update_utc INTEGER NOT NULL);
         INSERT INTO cookies VALUES ({c},'.example.com','','sid','abc','','/',{e},1,1,{a},1,1,1,0,2,443,{a});
         INSERT INTO cookies VALUES ({c},'mail.example.com','','enc','',x'763130aabb','/',{e},1,1,{a},1,1,1,0,2,443,{a});",
        c = wk(1_600_000_000),
        a = wk(1_600_000_500),
        e = wk(1_700_000_000)
    ))
}

fn chrome_logins() -> Vec<u8> {
    sqlite(&format!(
        "CREATE TABLE logins (origin_url VARCHAR NOT NULL, action_url VARCHAR, username_element VARCHAR, username_value VARCHAR, password_element VARCHAR, password_value BLOB, submit_element VARCHAR, signon_realm VARCHAR NOT NULL, date_created INTEGER NOT NULL, blacklisted_by_user INTEGER NOT NULL, scheme INTEGER NOT NULL, password_type INTEGER, times_used INTEGER, form_data BLOB, display_name VARCHAR, icon_url VARCHAR, federation_url VARCHAR, skip_zero_click INTEGER, generation_upload_status INTEGER, possible_username_pairs BLOB, id INTEGER PRIMARY KEY AUTOINCREMENT, date_last_used INTEGER NOT NULL DEFAULT 0, moving_blocked_for BLOB, date_password_modified INTEGER NOT NULL DEFAULT 0);
         INSERT INTO logins (origin_url, username_value, password_value, signon_realm, date_created, blacklisted_by_user, scheme) VALUES ('https://accounts.shop.example.com/login','bob@example.com',x'00','https://accounts.shop.example.com/',{t},0,0);",
        t = wk(1_600_001_000)
    ))
}

fn chrome_web_data() -> Vec<u8> {
    sqlite(
        "CREATE TABLE autofill (name VARCHAR, value VARCHAR, value_lower VARCHAR, date_created INTEGER DEFAULT 0, date_last_used INTEGER DEFAULT 0, count INTEGER DEFAULT 1, PRIMARY KEY (name, value));
         CREATE TABLE autofill_profiles ( guid VARCHAR PRIMARY KEY, company_name VARCHAR, street_address VARCHAR, dependent_locality VARCHAR, city VARCHAR, state VARCHAR, zipcode VARCHAR, sorting_code VARCHAR, country_code VARCHAR, date_modified INTEGER NOT NULL DEFAULT 0, origin VARCHAR DEFAULT '', language_code VARCHAR, use_count INTEGER NOT NULL DEFAULT 0, use_date INTEGER NOT NULL DEFAULT 0, validity_bitfield UNSIGNED NOT NULL DEFAULT 0);
         CREATE TABLE autofill_profile_names ( guid VARCHAR, first_name VARCHAR, middle_name VARCHAR, last_name VARCHAR, full_name VARCHAR);
         CREATE TABLE autofill_profile_emails ( guid VARCHAR, email VARCHAR);
         CREATE TABLE autofill_profile_phones ( guid VARCHAR, number VARCHAR);
         INSERT INTO autofill VALUES ('email','bob@example.com','bob@example.com',1600000000,1600000600,3);
         INSERT INTO autofill VALUES ('cc',x'0102','',1600000000,1600000600,1);
         INSERT INTO autofill_profiles VALUES ('g1','ACME','1 Main St','','Springfield','IL','62701','','US',1600000700,'','en',4,1600000800,0);
         INSERT INTO autofill_profile_names VALUES ('g1','Bob','Q','Builder','');
         INSERT INTO autofill_profile_emails VALUES ('g1','bob@example.com');
         INSERT INTO autofill_profile_phones VALUES ('g1','+1 555 0100');",
    )
}

fn chrome_favicons() -> Vec<u8> {
    sqlite(&format!(
        "CREATE TABLE icon_mapping(id INTEGER PRIMARY KEY,page_url LONGVARCHAR NOT NULL,icon_id INTEGER);
         CREATE TABLE favicon_bitmaps(id INTEGER PRIMARY KEY,icon_id INTEGER NOT NULL,last_updated INTEGER DEFAULT 0,image_data BLOB,width INTEGER DEFAULT 0,height INTEGER DEFAULT 0,last_requested INTEGER NOT NULL DEFAULT 0);
         INSERT INTO icon_mapping VALUES (1,'https://www.example.com/',7);
         INSERT INTO favicon_bitmaps VALUES (1,7,{u},x'00',16,16,{r});",
        u = wk(1_600_000_010),
        r = wk(1_600_000_020)
    ))
}

const LOCAL_STATE: &str = r#"{"profile":{"info_cache":{
    "Default":{"name":"Bob Personal","user_name":"bob@example.com","gaia_id":"1234","shortcut_name":"Bob","hosted_domain":"NO_HOSTED_DOMAIN"},
    "Profile 1":{"name":"Bob Work","user_name":"","gaia_id":"","shortcut_name":"Work"}}}}"#;

const BOOKMARKS: &str = r#"{"checksum":"x","roots":{
    "bookmark_bar":{"children":[{"date_added":"13245000000000000","name":"Rust","type":"url","url":"https://www.rust-lang.org/"},
        {"name":"Folder","type":"folder","children":[{"name":"Docs","type":"url","url":"https://docs.rs/"}]}],"name":"Bookmarks bar","type":"folder"},
    "other":{"children":[],"name":"Other bookmarks","type":"folder"}},"version":1}"#;

const SECURE_PREFS: &str = r#"{"extensions":{"settings":{
    "aapocclcgogkmnckokdopfmhonfmgoek":{"state":1,"active_permissions":{"api":["storage","tabs"]},"manifest":{"name":"Slides","version":"0.10","description":"Slides"}},
    "cflijgpldfbmdijnkeoadcjpfgokoeck":{"state":0,"manifest":{"name":"Bad","version":"1"}}}}}"#;

const FIREFOX_PLACES: &str = "
CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url LONGVARCHAR, title LONGVARCHAR, rev_host LONGVARCHAR, visit_count INTEGER DEFAULT 0, hidden INTEGER DEFAULT 0 NOT NULL, typed INTEGER DEFAULT 0 NOT NULL, frecency INTEGER DEFAULT -1 NOT NULL, last_visit_date INTEGER, guid TEXT, foreign_count INTEGER DEFAULT 0 NOT NULL, url_hash INTEGER DEFAULT 0 NOT NULL, description TEXT, preview_image_url TEXT, origin_id INTEGER);
CREATE TABLE moz_historyvisits (id INTEGER PRIMARY KEY, from_visit INTEGER, place_id INTEGER, visit_date INTEGER, visit_type INTEGER, session INTEGER);
CREATE TABLE moz_bookmarks (id INTEGER PRIMARY KEY, type INTEGER, fk INTEGER DEFAULT NULL, parent INTEGER, position INTEGER, title LONGVARCHAR, keyword_id INTEGER, folder_type TEXT, dateAdded INTEGER, lastModified INTEGER, guid TEXT, syncStatus INTEGER NOT NULL DEFAULT 0, syncChangeCounter INTEGER NOT NULL DEFAULT 1);
CREATE TABLE moz_anno_attributes (id INTEGER PRIMARY KEY, name VARCHAR(32) UNIQUE NOT NULL);
CREATE TABLE moz_annos (id INTEGER PRIMARY KEY, place_id INTEGER NOT NULL, anno_attribute_id INTEGER, content LONGVARCHAR, flags INTEGER DEFAULT 0, expiration INTEGER DEFAULT 0, type INTEGER DEFAULT 0, dateAdded INTEGER DEFAULT 0, lastModified INTEGER DEFAULT 0);
";

fn firefox_places() -> Vec<u8> {
    sqlite(&format!(
        "{FIREFOX_PLACES}
         INSERT INTO moz_places (id,url,title,hidden) VALUES (1,'https://duckduckgo.com/?q=rust+parser&t=h_','rust parser at DuckDuckGo',0);
         INSERT INTO moz_places (id,url,title,hidden) VALUES (2,'https://blog.example.net/wp-admin/options.php','Options',0);
         INSERT INTO moz_places (id,url,title,hidden) VALUES (3,'place:sort=8&maxResults=10',NULL,0);
         INSERT INTO moz_places (id,url,title,hidden) VALUES (4,'https://hidden.example.com/','Hidden',1);
         INSERT INTO moz_places (id,url,title,hidden) VALUES (5,'https://files.example.net/tool.zip','tool.zip',0);
         INSERT INTO moz_historyvisits VALUES (1,0,1,1600100000000000,1,0);
         INSERT INTO moz_historyvisits VALUES (2,1,2,1600100100000000,1,0);
         INSERT INTO moz_historyvisits VALUES (3,0,4,1600100200000000,1,0);
         INSERT INTO moz_bookmarks (id,type,fk,parent,title,dateAdded) VALUES (10,1,2,3,'Blog admin',1600100300000000);
         INSERT INTO moz_bookmarks (id,type,fk,parent,title,dateAdded) VALUES (11,1,3,3,'Most Visited',0);
         INSERT INTO moz_anno_attributes VALUES (1,'downloads/destinationFileURI');
         INSERT INTO moz_annos (place_id,anno_attribute_id,content,lastModified) VALUES (5,1,'file:///C:/Users/bob/Downloads/tool%20v2.zip',1600100400000000);"
    ))
}

fn firefox_cookies() -> Vec<u8> {
    sqlite(
        "CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, originAttributes TEXT NOT NULL DEFAULT '', name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, lastAccessed INTEGER, creationTime INTEGER, isSecure INTEGER, isHttpOnly INTEGER, inBrowserElement INTEGER DEFAULT 0, sameSite INTEGER DEFAULT 0, rawSameSite INTEGER DEFAULT 0, schemeMap INTEGER DEFAULT 0);
         INSERT INTO moz_cookies (name,value,host,path,expiry,lastAccessed,creationTime) VALUES ('_ga','GA1.2','.mail.example.org','/',1700000000,1600100500000000,1600100000000000);",
    )
}

/// Firefox < 3.x cookie table: no `creationTime`.
fn firefox_cookies_v3() -> Vec<u8> {
    sqlite(
        "CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, lastAccessed INTEGER, isSecure INTEGER, isHttpOnly INTEGER);
         INSERT INTO moz_cookies (name,value,host,path,expiry,lastAccessed) VALUES ('old','1','old.example.com','/',1,1200000000000000);",
    )
}

fn firefox_downloads() -> Vec<u8> {
    sqlite(
        "CREATE TABLE moz_downloads (id INTEGER PRIMARY KEY, name TEXT, source TEXT, target TEXT, tempPath TEXT, startTime INTEGER, endTime INTEGER, state INTEGER, referrer TEXT, entityID TEXT, currBytes INTEGER NOT NULL DEFAULT 0, maxBytes INTEGER NOT NULL DEFAULT -1, mimeType TEXT, preferredApplication TEXT, preferredAction INTEGER NOT NULL DEFAULT 0, autoResume INTEGER NOT NULL DEFAULT 0);
         INSERT INTO moz_downloads (name,source,target,startTime,endTime,state,maxBytes) VALUES ('old.exe','http://old.example.com/old.exe','file:///C:/Users/bob/Downloads/old.exe',1200000000000000,1200000001000000,1,10);",
    )
}

fn firefox_forms() -> Vec<u8> {
    sqlite(
        "CREATE TABLE moz_formhistory (id INTEGER PRIMARY KEY, fieldname TEXT NOT NULL, value TEXT NOT NULL, timesUsed INTEGER, firstUsed INTEGER, lastUsed INTEGER, guid TEXT);
         INSERT INTO moz_formhistory (fieldname,value,timesUsed,firstUsed,lastUsed) VALUES ('searchbar-history','rust parser',2,1600100000000000,1600100900000000);
         INSERT INTO moz_formhistory (fieldname,value,timesUsed,firstUsed,lastUsed) VALUES ('ts','123',1,1,1);",
    )
}

const FIREFOX_AUTOFILL: &str = r#"{"version":1,"addresses":[{"guid":"a","name":"Alice Example","email":"alice@example.org",
    "tel":"+358401234567","address-line1":"Main St 1","postal-code":"00100","country":"FI",
    "timeCreated":1600100000123,"timeLastUsed":1600100500000,"timesUsed":2}],"creditCards":[]}"#;

fn safari_history() -> Vec<u8> {
    sqlite(
        "CREATE TABLE history_items (id INTEGER PRIMARY KEY AUTOINCREMENT,url TEXT NOT NULL UNIQUE,domain_expansion TEXT NULL,visit_count INTEGER NOT NULL,daily_visit_counts BLOB NOT NULL,weekly_visit_counts BLOB NULL,autocomplete_triggers BLOB NULL,should_recompute_derived_visit_counts INTEGER NOT NULL,visit_count_score INTEGER NOT NULL);
         CREATE TABLE history_visits (id INTEGER PRIMARY KEY AUTOINCREMENT,history_item INTEGER NOT NULL REFERENCES history_items(id) ON DELETE CASCADE,visit_time REAL NOT NULL,title TEXT NULL,load_successful BOOLEAN NOT NULL DEFAULT 1,http_non_get BOOLEAN NOT NULL DEFAULT 0,synthesized BOOLEAN NOT NULL DEFAULT 0,redirect_source INTEGER NULL UNIQUE REFERENCES history_visits(id) ON DELETE CASCADE,redirect_destination INTEGER NULL UNIQUE REFERENCES history_visits(id) ON DELETE CASCADE,origin INTEGER NOT NULL DEFAULT 0,generation INTEGER NOT NULL DEFAULT 0,attributes INTEGER NOT NULL DEFAULT 0,score INTEGER NOT NULL DEFAULT 0);
         INSERT INTO history_items VALUES (1,'https://www.bing.com/search?q=mac+forensics',NULL,1,x'',NULL,NULL,0,1);
         INSERT INTO history_visits (history_item, visit_time, title) VALUES (1, 621792800.5, 'mac forensics - Bing');",
    )
}

fn plist_bytes(v: &plist::Value) -> Vec<u8> {
    let mut out = Vec::new();
    v.to_writer_binary(&mut out).unwrap();
    out
}

fn safari_bookmarks() -> Vec<u8> {
    use plist::{Dictionary, Value};
    let mut uri = Dictionary::new();
    uri.insert("title".into(), Value::String("Apple".into()));
    let mut leaf = Dictionary::new();
    leaf.insert(
        "URLString".into(),
        Value::String("https://www.apple.com/".into()),
    );
    leaf.insert("URIDictionary".into(), Value::Dictionary(uri));
    let mut bar = Dictionary::new();
    bar.insert("Title".into(), Value::String("BookmarksBar".into()));
    bar.insert(
        "Children".into(),
        Value::Array(vec![Value::Dictionary(leaf)]),
    );
    let mut root = Dictionary::new();
    root.insert(
        "Children".into(),
        Value::Array(vec![Value::Dictionary(bar)]),
    );
    plist_bytes(&Value::Dictionary(root))
}

fn safari_downloads() -> Vec<u8> {
    use plist::{Dictionary, Value};
    let mut d = Dictionary::new();
    d.insert(
        "DownloadEntryURL".into(),
        Value::String("https://dl.example.org/app.dmg".into()),
    );
    d.insert(
        "DownloadEntryPath".into(),
        Value::String("/Users/alice/Downloads/app.dmg".into()),
    );
    d.insert(
        "DownloadEntryDateAddedKey".into(),
        Value::Date((std::time::UNIX_EPOCH + std::time::Duration::new(1_600_200_000, 0)).into()),
    );
    let mut root = Dictionary::new();
    root.insert(
        "DownloadHistory".into(),
        Value::Array(vec![Value::Dictionary(d)]),
    );
    plist_bytes(&Value::Dictionary(root))
}

fn ie_history() -> Vec<u8> {
    let mut cache = idx::Spec::url(
        "http://www.google.com/intl/en/images/logo.gif",
        ft(1_093_500_000),
    );
    cache.referenced = true;
    let leak = idx::Spec {
        kind: *b"LEAK",
        ..idx::Spec::url("http://leak.example.com/", ft(1_093_500_000))
    };
    idx::build(
        &[],
        &[
            idx::Spec::url(
                "Visited: Mr. Evil@http://www.google.com/search?hl=en&q=hacking+tools",
                ft(1_093_532_400),
            ),
            idx::Spec::url(
                ":2004082520040826: Mr. Evil@http://www.yahoo.com/",
                ft(1_093_540_000),
            ),
            cache,
            leak,
        ],
        8,
    )
}

const CHROME: &str = "/Users/bob/AppData/Local/Google/Chrome/User Data";
const FIREFOX: &str = "/Users/bob/AppData/Roaming/Mozilla/Firefox/Profiles/abcd.default";
const IE_USER: &str = "/Documents and Settings/Mr. Evil";
const SAFARI: &str = "/Users/alice/Library/Safari";

fn image() -> Vec<u8> {
    let (history, history_wal) = chrome_history();
    let cache = cc::build(
        &[
            cc::Spec {
                key: "https://www.example.com/logo.png".into(),
                created: wk(1_600_000_050),
                headers: cc::headers(&["HTTP/1.1 200 OK", "Content-Type: image/png"]),
                payload: b"\x89PNG\r\n\x1a\nfake".to_vec(),
                external: false,
                bucket: 1,
            },
            cc::Spec {
                key: "https://cdn.example.net/app.js".into(),
                created: wk(1_600_000_060),
                headers: cc::headers(&["HTTP/1.1 200", "content-encoding: br"]),
                payload: vec![7u8; 20_000],
                external: true,
                bucket: 1,
            },
        ],
        16,
    );
    let mut b = NtfsBuilder::new(48);
    let dirs = [
        "/Users",
        "/Users/bob",
        "/Users/bob/Downloads",
        "/Users/bob/AppData",
        "/Users/bob/AppData/Local",
        "/Users/bob/AppData/Local/Google",
        "/Users/bob/AppData/Local/Google/Chrome",
        CHROME,
        "/Users/bob/AppData/Local/Google/Chrome/User Data/Default",
        "/Users/bob/AppData/Local/Google/Chrome/User Data/Default/Cache",
        "/Users/bob/AppData/Local/Google/Chrome/User Data/Profile 1",
        "/Users/bob/AppData/Roaming",
        "/Users/bob/AppData/Roaming/Mozilla",
        "/Users/bob/AppData/Roaming/Mozilla/Firefox",
        "/Users/bob/AppData/Roaming/Mozilla/Firefox/Profiles",
        FIREFOX,
        "/Users/bob/AppData/Roaming/Mozilla/Firefox/Profiles/old.default",
        "/Documents and Settings",
        IE_USER,
        "/Documents and Settings/Mr. Evil/Favorites",
        "/Documents and Settings/Mr. Evil/Cookies",
        "/Documents and Settings/Mr. Evil/Local Settings",
        "/Documents and Settings/Mr. Evil/Local Settings/History",
        "/Documents and Settings/Mr. Evil/Local Settings/History/History.IE5",
        "/Users/alice",
        "/Users/alice/Downloads",
        "/Users/alice/Library",
        SAFARI,
        "/Users/alice/Library/Cookies",
    ];
    for d in dirs {
        b.dir(d);
    }
    let def = format!("{CHROME}/Default");
    b.file("/Users/bob/Downloads/setup.exe", b"MZ setup")
        .file("/Users/bob/Downloads/tool v2.zip", b"PK zip")
        .file(&format!("{CHROME}/Local State"), LOCAL_STATE.as_bytes())
        .file(&format!("{def}/History"), &history)
        .file(&format!("{def}/History-wal"), &history_wal)
        .file(&format!("{def}/Cookies"), &chrome_cookies())
        .file(&format!("{def}/Login Data"), &chrome_logins())
        .file(&format!("{def}/Web Data"), &chrome_web_data())
        .file(&format!("{def}/Favicons"), &chrome_favicons())
        .file(&format!("{def}/Bookmarks"), BOOKMARKS.as_bytes())
        .file(
            &format!("{def}/Secure Preferences"),
            SECURE_PREFS.as_bytes(),
        )
        .file(
            &format!("{CHROME}/Profile 1/History"),
            &chrome_history_pre30(),
        );
    for (name, data) in &cache {
        b.file(&format!("{def}/Cache/{name}"), data);
    }
    b.file(&format!("{FIREFOX}/places.sqlite"), &firefox_places())
        .file(&format!("{FIREFOX}/cookies.sqlite"), &firefox_cookies())
        .file(&format!("{FIREFOX}/formhistory.sqlite"), &firefox_forms())
        .file(
            &format!("{FIREFOX}/autofill-profiles.json"),
            FIREFOX_AUTOFILL.as_bytes(),
        )
        .file(
            "/Users/bob/AppData/Roaming/Mozilla/Firefox/Profiles/old.default/downloads.sqlite",
            &firefox_downloads(),
        )
        .file(
            "/Users/bob/AppData/Roaming/Mozilla/Firefox/Profiles/old.default/cookies.sqlite",
            &firefox_cookies_v3(),
        )
        .file(
            "/Documents and Settings/Mr. Evil/Local Settings/History/History.IE5/index.dat",
            &ie_history(),
        )
        .file_with(
            "/Documents and Settings/Mr. Evil/Favorites/Google.url",
            b"[DEFAULT]\r\nBASEURL=http://www.google.com/\r\n[InternetShortcut]\r\nURL=http://www.google.com/\r\n",
            FileOpts {
                times: Some([ft(1_090_000_000); 4]),
                ..FileOpts::default()
            },
        )
        .file(
            "/Documents and Settings/Mr. Evil/Cookies/mr. evil@google[1].txt",
            b"PREF\nID=abc:TM=1093532400\ngoogle.com/\n1536\n2618878336\n32111634\n2585396736\n29653749\n*\n",
        )
        .file(&format!("{SAFARI}/History.db"), &safari_history())
        .file(&format!("{SAFARI}/Bookmarks.plist"), &safari_bookmarks())
        .file(&format!("{SAFARI}/Downloads.plist"), &safari_downloads())
        .file("/Users/alice/Downloads/app.dmg", b"dmg")
        .file(
            "/Users/alice/Library/Cookies/Cookies.binarycookies",
            &bc::build(&[vec![(
                ".icq.com",
                "sess",
                "/",
                "v1",
                700_000_000.0,
                600_000_000.0,
            )]]),
        );
    b.build()
}

type Dump = BTreeMap<String, Vec<BTreeMap<String, String>>>;

fn dump(c: &DemoCase) -> Dump {
    let db = c.case.db();
    let mut out: Dump = BTreeMap::new();
    for (id, name, _, _) in db.artifact_type_names().unwrap() {
        for a in db.artifacts_of_type(id, Some(c.ds_id), 0, 100_000).unwrap() {
            let attrs = db
                .attributes(a.id)
                .unwrap()
                .into_iter()
                .map(|x| (x.name, x.value.display()))
                .collect();
            out.entry(name.clone()).or_default().push(attrs);
        }
    }
    out
}

fn find<'a>(d: &'a Dump, ty: &str, key: &str, value: &str) -> &'a BTreeMap<String, String> {
    d.get(ty)
        .and_then(|v| {
            v.iter()
                .find(|a| a.get(key).map(String::as_str) == Some(value))
        })
        .unwrap_or_else(|| panic!("no {ty} with {key}={value}: {:#?}", d.get(ty)))
}

fn count(d: &Dump, ty: &str) -> usize {
    d.get(ty).map_or(0, Vec::len)
}

#[test]
fn browsers_end_to_end() {
    let c = DemoCase::with_image(&image());
    let (_, inbox) = c.run_one(Arc::new(RecentActivityFactory), serde_json::Value::Null);
    let d = dump(&c);

    // ---- Chromium
    let ls = find(&d, "TSK_LOCAL_STATE", "TSK_PATH", "Default");
    assert_eq!(ls["TSK_USER_ID"], "1234");
    assert_eq!(ls["TSK_NAME"], "Bob Personal");
    assert_eq!(ls["TSK_USER_NAME"], "bob@example.com");
    assert_eq!(ls["TSK_PROG_NAME"], "Google Chrome");
    assert_eq!(count(&d, "TSK_LOCAL_STATE"), 2);

    let ext = find(
        &d,
        "TSK_CHROME_EXTENSIONS",
        "TSK_ID",
        "aapocclcgogkmnckokdopfmhonfmgoek",
    );
    assert_eq!(ext["TSK_FLAG"], "Enabled");
    assert_eq!(ext["TSK_PERMISSIONS"], "storage, tabs");
    assert_eq!(ext["TSK_USER_NAME"], "Bob Personal");
    assert!(!ext.contains_key("TSK_COMMENT"));
    let bad = find(
        &d,
        "TSK_CHROME_EXTENSIONS",
        "TSK_ID",
        "cflijgpldfbmdijnkeoadcjpfgokoeck",
    );
    assert!(bad["TSK_COMMENT"].starts_with("Malicious Extension Found - https://"));

    let v = find(
        &d,
        "TSK_WEB_HISTORY",
        "TSK_TITLE",
        "forensic tools - Google Search",
    );
    assert_eq!(v["TSK_DATETIME_ACCESSED"], "1600000100");
    assert_eq!(v["TSK_REFERRER"], "https://www.example.com/");
    assert_eq!(v["TSK_DOMAIN"], "google.com");
    assert_eq!(v["TSK_PROG_NAME"], "Google Chrome");
    assert_eq!(v["TSK_USER_NAME"], "Bob Personal");
    // The WAL's visits are read.
    let wal = find(&d, "TSK_WEB_HISTORY", "TSK_URL", "https://wal.example.com/");
    assert_eq!(wal["TSK_DATETIME_ACCESSED"], "1600000300");
    let work = find(
        &d,
        "TSK_WEB_HISTORY",
        "TSK_URL",
        "https://work.example.com/",
    );
    assert_eq!(work["TSK_USER_NAME"], "Bob Work");

    let bm = find(
        &d,
        "TSK_WEB_BOOKMARK",
        "TSK_URL",
        "https://www.rust-lang.org/",
    );
    assert_eq!(bm["TSK_COMMENT"], "bookmark_bar");
    assert_eq!(bm["TSK_DATETIME_CREATED"], "1600526400");
    // Folders are walked (Autopsy reads only the first level).
    find(&d, "TSK_WEB_BOOKMARK", "TSK_URL", "https://docs.rs/");

    let ck = find(&d, "TSK_WEB_COOKIE", "TSK_NAME", "sid");
    assert_eq!(ck["TSK_URL"], ".example.com");
    assert_eq!(ck["TSK_DOMAIN"], "example.com");
    assert_eq!(ck["TSK_DATETIME_ACCESSED"], "1600000500");
    assert_eq!(ck["TSK_USER_NAME"], "Bob Personal");

    let login = find(&d, "TSK_SERVICE_ACCOUNT", "TSK_USER_ID", "bob@example.com");
    assert_eq!(login["TSK_URL"], "https://accounts.shop.example.com/login");
    assert_eq!(login["TSK_REALM"], "https://accounts.shop.example.com/");
    assert_eq!(login["TSK_DOMAIN"], "example.com");
    assert_eq!(login["TSK_DATETIME_CREATED"], "1600001000");

    let af = find(&d, "TSK_WEB_FORM_AUTOFILL", "TSK_NAME", "email");
    assert_eq!(af["TSK_VALUE"], "bob@example.com");
    assert_eq!(af["TSK_COUNT"], "3");
    assert_eq!(af["TSK_DATETIME_CREATED"], "1600000000");
    assert_eq!(af["TSK_DATETIME_ACCESSED"], "1600000600");
    let enc = find(&d, "TSK_WEB_FORM_AUTOFILL", "TSK_NAME", "cc");
    assert_eq!(enc["TSK_COMMENT"], "The data was encrypted.");
    let ed = find(
        &d,
        "TSK_ENCRYPTION_DETECTED",
        "TSK_COMMENT",
        "Google Chrome Autofill Database Encryption Detected",
    );
    assert_eq!(ed.len(), 1);
    let addr = find(&d, "TSK_WEB_FORM_ADDRESS", "TSK_EMAIL", "bob@example.com");
    assert_eq!(addr["TSK_NAME"], "Bob Q Builder");
    assert_eq!(addr["TSK_PHONE_NUMBER"], "+1 555 0100");
    assert_eq!(
        addr["TSK_LOCATION"],
        "1 Main St, Springfield, IL, 62701, US"
    );
    assert_eq!(addr["TSK_DATETIME_ACCESSED"], "1600000800");
    assert_eq!(addr["TSK_COUNT"], "4");
    assert!(
        c.case
            .db()
            .account("EMAIL", "bob@example.com")
            .unwrap()
            .is_some()
    );

    let setup_id = c.file("/Users/bob/Downloads/setup.exe").id.to_string();
    let dl = find(
        &d,
        "TSK_WEB_DOWNLOAD",
        "TSK_URL",
        "https://cdn.example.net/setup.exe",
    );
    assert_eq!(dl["TSK_PATH"], "C:\\Users\\bob\\Downloads\\setup.exe");
    assert_eq!(dl["TSK_PATH_ID"], setup_id);
    assert_eq!(dl["TSK_DATETIME_ACCESSED"], "1600000100");
    // v30+: one artifact per URL-chain entry.
    find(
        &d,
        "TSK_WEB_DOWNLOAD",
        "TSK_URL",
        "http://dl.example.net/redirect",
    );
    let old = find(
        &d,
        "TSK_WEB_DOWNLOAD",
        "TSK_URL",
        "https://work.example.com/report.pdf",
    );
    assert!(!old.contains_key("TSK_PATH_ID"), "file not on the image");
    assert_eq!(old["TSK_USER_NAME"], "Bob Work");

    let fav = find(&d, "TSK_FAVICON", "TSK_URL", "https://www.example.com/");
    assert_eq!(fav["TSK_DATETIME_MODIFIED"], "1600000010");
    assert_eq!(fav["TSK_DATETIME_ACCESSED"], "1600000020");

    // Chrome cache: one in-block payload (derived file), one f_ file.
    assert_eq!(
        count(&d, "TSK_WEB_CACHE"),
        2,
        "{:#?}",
        d.get("TSK_WEB_CACHE")
    );
    let logo = find(
        &d,
        "TSK_WEB_CACHE",
        "TSK_URL",
        "https://www.example.com/logo.png",
    );
    assert!(
        logo["TSK_PATH"].ends_with("/Cache/data_2/data_2__b0020001"),
        "{logo:?}"
    );
    assert_eq!(logo["TSK_DATETIME_CREATED"], "1600000050");
    assert!(logo["TSK_HEADERS"].contains("content-type :  image/png"));
    let derived = c
        .case
        .db()
        .file(logo["TSK_PATH_ID"].parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(derived.name, "data_2__b0020001");
    assert_eq!(
        common::read_all(&*c.case.content(&derived).unwrap()),
        b"\x89PNG\r\n\x1a\nfake"
    );
    let js = find(
        &d,
        "TSK_WEB_CACHE",
        "TSK_URL",
        "https://cdn.example.net/app.js",
    );
    assert!(js["TSK_PATH"].ends_with("/Cache/f_000001"), "{js:?}");
    let f1 = c
        .case
        .db()
        .file(js["TSK_PATH_ID"].parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(f1.mime.as_deref(), Some("application/x-brotli"));

    // ---- Firefox
    let fv = find(
        &d,
        "TSK_WEB_HISTORY",
        "TSK_URL",
        "https://blog.example.net/wp-admin/options.php",
    );
    assert_eq!(fv["TSK_PROG_NAME"], "FireFox Analyzer");
    assert_eq!(fv["TSK_DATETIME_ACCESSED"], "1600100100");
    assert_eq!(
        fv["TSK_REFERRER"],
        "https://duckduckgo.com/?q=rust+parser&t=h_"
    );
    assert!(!fv.contains_key("TSK_USER_NAME"));
    assert!(
        d["TSK_WEB_HISTORY"]
            .iter()
            .all(|a| a["TSK_URL"] != "https://hidden.example.com/")
    );
    let place = find(&d, "TSK_WEB_BOOKMARK", "TSK_TITLE", "Most Visited");
    assert!(!place.contains_key("TSK_DOMAIN"));
    assert!(!place.contains_key("TSK_DATETIME_CREATED"));
    let fbm = find(&d, "TSK_WEB_BOOKMARK", "TSK_TITLE", "Blog admin");
    assert_eq!(fbm["TSK_DATETIME_CREATED"], "1600100300");
    assert_eq!(fbm["TSK_DOMAIN"], "example.net");
    let fdl = find(
        &d,
        "TSK_WEB_DOWNLOAD",
        "TSK_URL",
        "https://files.example.net/tool.zip",
    );
    assert_eq!(fdl["TSK_PATH"], "C:/Users/bob/Downloads/tool v2.zip");
    assert_eq!(
        fdl["TSK_PATH_ID"],
        c.file("/Users/bob/Downloads/tool v2.zip").id.to_string()
    );
    let fdl_old = find(
        &d,
        "TSK_WEB_DOWNLOAD",
        "TSK_URL",
        "http://old.example.com/old.exe",
    );
    assert_eq!(fdl_old["TSK_DATETIME_ACCESSED"], "1200000000");
    let fck = find(&d, "TSK_WEB_COOKIE", "TSK_NAME", "_ga");
    assert_eq!(fck["TSK_DOMAIN"], "example.org");
    assert_eq!(fck["TSK_DATETIME_CREATED"], "1600100000");
    let v3 = find(&d, "TSK_WEB_COOKIE", "TSK_NAME", "old");
    assert!(!v3.contains_key("TSK_DATETIME_CREATED"));
    let form = find(&d, "TSK_WEB_FORM_AUTOFILL", "TSK_NAME", "searchbar-history");
    assert_eq!(form["TSK_COUNT"], "2");
    assert_eq!(form["TSK_DATETIME_ACCESSED"], "1600100900");
    assert!(
        d["TSK_WEB_FORM_AUTOFILL"]
            .iter()
            .all(|a| a["TSK_NAME"] != "ts")
    );
    let faddr = find(&d, "TSK_WEB_FORM_ADDRESS", "TSK_NAME", "Alice Example");
    assert_eq!(faddr["TSK_LOCATION"], "Main St 1, 00100, FI");
    assert_eq!(faddr["TSK_DATETIME_CREATED"], "1600100000");
    assert_eq!(faddr["TSK_COUNT"], "2");

    // ---- Internet Explorer
    let ie = find(
        &d,
        "TSK_WEB_HISTORY",
        "TSK_URL",
        "http://www.google.com/search?hl=en&q=hacking+tools",
    );
    assert_eq!(ie["TSK_USER_NAME"], "Mr. Evil");
    assert_eq!(ie["TSK_DATETIME_ACCESSED"], "1093532400");
    assert_eq!(ie["TSK_PROG_NAME"], "Internet Explorer Analyzer");
    let yahoo = find(&d, "TSK_WEB_HISTORY", "TSK_URL", "http://www.yahoo.com/");
    assert_eq!(yahoo["TSK_USER_NAME"], "Mr. Evil");
    find(
        &d,
        "TSK_WEB_HISTORY",
        "TSK_URL",
        "http://www.google.com/intl/en/images/logo.gif",
    );
    assert!(
        d["TSK_WEB_HISTORY"]
            .iter()
            .all(|a| a["TSK_URL"] != "http://leak.example.com/")
    );
    let ief = find(&d, "TSK_WEB_BOOKMARK", "TSK_TITLE", "Google.url");
    assert_eq!(ief["TSK_URL"], "http://www.google.com/");
    assert_eq!(ief["TSK_DATETIME_CREATED"], "1090000000");
    let iec = find(&d, "TSK_WEB_COOKIE", "TSK_NAME", "PREF");
    assert_eq!(iec["TSK_URL"], "google.com/");
    assert_eq!(iec["TSK_DOMAIN"], "google.com");

    // ---- Safari
    let sv = find(&d, "TSK_WEB_HISTORY", "TSK_PROG_NAME", "Safari Analyzer");
    assert_eq!(sv["TSK_URL"], "https://www.bing.com/search?q=mac+forensics");
    assert_eq!(sv["TSK_DATETIME_ACCESSED"], "1600100000");
    assert_eq!(sv["TSK_TITLE"], "mac forensics - Bing");
    let sb = find(&d, "TSK_WEB_BOOKMARK", "TSK_URL", "https://www.apple.com/");
    assert_eq!(sb["TSK_TITLE"], "Apple");
    let sd = find(
        &d,
        "TSK_WEB_DOWNLOAD",
        "TSK_URL",
        "https://dl.example.org/app.dmg",
    );
    assert_eq!(sd["TSK_DATETIME_ACCESSED"], "1600200000");
    assert_eq!(
        sd["TSK_PATH_ID"],
        c.file("/Users/alice/Downloads/app.dmg").id.to_string()
    );
    let sc = find(&d, "TSK_WEB_COOKIE", "TSK_NAME", "sess");
    assert_eq!(sc["TSK_DATETIME_CREATED"], "1578307200");
    assert_eq!(sc["TSK_DATETIME_END"], "1678307200");
    assert_eq!(sc["TSK_DOMAIN"], "icq.com");

    // ---- Analyses
    let q = find(&d, "TSK_WEB_SEARCH_QUERY", "TSK_TEXT", "forensic tools");
    assert_eq!(q["TSK_PROG_NAME"], "Google Chrome");
    assert_eq!(q["TSK_DOMAIN"], "google.com");
    assert_eq!(q["TSK_DATETIME_ACCESSED"], "1600000100");
    find(&d, "TSK_WEB_SEARCH_QUERY", "TSK_TEXT", "rust parser");
    find(&d, "TSK_WEB_SEARCH_QUERY", "TSK_TEXT", "hacking tools");
    find(&d, "TSK_WEB_SEARCH_QUERY", "TSK_TEXT", "mac forensics");

    let admin = find(
        &d,
        "TSK_WEB_ACCOUNT_TYPE",
        "TSK_TEXT",
        "Administrator role (myBB platform)",
    );
    assert_eq!(admin["TSK_DOMAIN"], "example.org");
    assert_eq!(
        admin["TSK_URL"],
        "https://forum.example.org/admin/index.php"
    );
    let wp = find(&d, "TSK_WEB_ACCOUNT_TYPE", "TSK_DOMAIN", "example.net");
    assert_eq!(wp["TSK_TEXT"], "Administrator role (WordPress platform)");
    find(
        &d,
        "TSK_WEB_ACCOUNT_TYPE",
        "TSK_TEXT",
        "User role (phpBB platform)",
    );
    let svc = find(
        &d,
        "TSK_WEB_ACCOUNT_TYPE",
        "TSK_TEXT",
        "User role (example.com)",
    );
    assert_eq!(svc["TSK_URL"], "https://accounts.shop.example.com/login");

    // The search query artifact (no URL; host = its domain) sorts first.
    let se = find(&d, "TSK_WEB_CATEGORIZATION", "TSK_DOMAIN", "google.com");
    assert_eq!(se["TSK_NAME"], "Search Engine");
    assert_eq!(se["TSK_HOST"], "google.com");
    let icq = find(&d, "TSK_WEB_CATEGORIZATION", "TSK_NAME", "Messaging");
    assert_eq!(icq["TSK_DOMAIN"], "icq.com");

    // ---- Inbox
    let msgs = inbox.since(0);
    let subjects: Vec<&str> = msgs.iter().map(|m| m.subject.as_str()).collect();
    assert!(
        subjects.iter().any(|s| s.starts_with("Started ")),
        "{subjects:?}"
    );
    let fin = msgs
        .iter()
        .find(|m| m.subject.starts_with("Finished "))
        .unwrap();
    assert!(
        fin.subject.ends_with(" - No errors reported"),
        "{}: {}",
        fin.subject,
        fin.details
    );
    assert_eq!(fin.kind, MessageKind::Info);
    let res = msgs
        .iter()
        .find(|m| m.subject.ends_with(" - Browser Results"))
        .unwrap();
    for line in [
        "Chromium Analyzer: Found.",
        "FireFox Analyzer: Found.",
        "Internet Explorer Analyzer: Found.",
        "Microsoft Edge Analyzer: Not Found.",
        "Safari Analyzer: Found.",
    ] {
        assert!(res.details.contains(line), "{}", res.details);
    }

    // ---- Re-running adds nothing.
    let before: BTreeMap<String, usize> = d.iter().map(|(k, v)| (k.clone(), v.len())).collect();
    c.run_one(Arc::new(RecentActivityFactory), serde_json::Value::Null);
    let after: BTreeMap<String, usize> =
        dump(&c).iter().map(|(k, v)| (k.clone(), v.len())).collect();
    assert_eq!(before, after);
}

#[test]
fn custom_web_categories_win() {
    let c = DemoCase::with_image(&image());
    c.run_one(
        Arc::new(RecentActivityFactory),
        serde_json::json!({"custom_web_categories": [{"suffix": "google.com", "category": "Work tools"}]}),
    );
    let d = dump(&c);
    let g = find(&d, "TSK_WEB_CATEGORIZATION", "TSK_DOMAIN", "google.com");
    assert_eq!(g["TSK_NAME"], "Work tools");
}

#[test]
fn hostile_stores_do_not_fail_ingest() {
    let mut b = NtfsBuilder::new(8);
    for d in [
        "/Users",
        "/Users/x",
        "/Users/x/Google",
        "/Users/x/Google/Chrome",
        "/Users/x/Google/Chrome/User Data",
        "/Users/x/Google/Chrome/User Data/Default",
        "/Users/x/Firefox",
        "/Users/x/Safari",
        "/Users/x/Cookies",
    ] {
        b.dir(d);
    }
    let garbage = vec![0x5Au8; 9000];
    let mut not_sqlite = b"SQLite format 3\0".to_vec();
    not_sqlite.extend(vec![0xFF; 4096]);
    b.file("/Users/x/Google/Chrome/User Data/Local State", b"{not json")
        .file(
            "/Users/x/Google/Chrome/User Data/Default/History",
            &not_sqlite,
        )
        .file("/Users/x/Google/Chrome/User Data/Default/Cookies", &garbage)
        .file(
            "/Users/x/Google/Chrome/User Data/Default/Bookmarks",
            b"[1,2",
        )
        .file("/Users/x/Firefox/places.sqlite", &garbage)
        .file(
            "/Users/x/Firefox/autofill-profiles.json",
            b"{\"addresses\":7}",
        )
        .file("/Users/x/Safari/Bookmarks.plist", &garbage)
        .file(
            "/Users/x/Safari/History.db",
            &sqlite("CREATE TABLE other(x);"),
        )
        .file(
            "/Users/x/Cookies/Cookies.binarycookies",
            b"cook\xff\xff\xff\xff",
        )
        .file(
            "/Users/x/index.dat",
            b"Client UrlCache MMF Ver 5.2\0\xff\xff",
        );
    let c = DemoCase::with_image(&b.build());
    let (_, inbox) = c.run_one(Arc::new(RecentActivityFactory), serde_json::Value::Null);
    let fin = inbox
        .since(0)
        .into_iter()
        .find(|m| m.subject.starts_with("Finished "))
        .unwrap();
    // Unreadable stores are reported, not fatal.
    assert_eq!(fin.kind, MessageKind::Error, "{}", fin.details);
    assert!(
        fin.details
            .contains("Chromium Analyzer: Error while trying to analyze file:"),
        "{}",
        fin.details
    );
}
