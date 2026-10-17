//! Safari stores (Autopsy `ExtractSafari`): `History.db`,
//! `Bookmarks.plist`, `Downloads.plist` (binary or XML property lists).
//! Cookies are in [`crate::binarycookies`].

use std::io::Cursor;
use std::time::{SystemTime, UNIX_EPOCH};

use plist::Value;
use rusqlite::Connection;

use crate::sqlite::{each, real, text_lossy};

/// One visit (`TSK_WEB_HISTORY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub time: Option<i64>,
}

/// Visits of `History.db` (Cocoa seconds; Autopsy truncates the double).
pub fn history(conn: &Connection, mut f: impl FnMut(Visit) -> bool) -> rusqlite::Result<()> {
    each(
        conn,
        "SELECT url, title, visit_time + 978307200 as time FROM 'history_items' JOIN history_visits ON history_item = history_items.id;",
        |r| {
            f(Visit {
                url: text_lossy(r, "url"),
                title: text_lossy(r, "title"),
                time: real(r, "time").filter(|t| t.is_finite()).map(|t| t as i64),
            })
        },
    )
}

/// One bookmark (`TSK_WEB_BOOKMARK`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub url: Option<String>,
    pub title: Option<String>,
}

/// Parses a property list (binary or XML).
pub fn parse_plist(data: &[u8]) -> Option<Value> {
    Value::from_reader(Cursor::new(data)).ok()
}

/// Bookmarks of `Bookmarks.plist`: dictionaries with `Children` are
/// folders, ones with `URLString` are bookmarks titled by
/// `URIDictionary.title` (Autopsy looks for `title` on the wrong
/// dictionary first and so never finds one).
pub fn bookmarks(data: &[u8]) -> Option<Vec<Bookmark>> {
    let root = parse_plist(data)?;
    root.as_dictionary()?;
    let mut out = Vec::new();
    let mut stack = vec![(&root, 0usize)];
    while let Some((node, depth)) = stack.pop() {
        let Some(d) = node.as_dictionary() else {
            continue;
        };
        if let Some(children) = d.get("Children") {
            if let Some(a) = children.as_array()
                && depth < 64
            {
                stack.extend(a.iter().rev().map(|c| (c, depth + 1)));
            }
        } else if d.contains_key("URLString") {
            let url = d
                .get("URLString")
                .and_then(Value::as_string)
                .map(str::to_owned);
            let title = d
                .get("URIDictionary")
                .and_then(Value::as_dictionary)
                .and_then(|u| u.get("title"))
                .and_then(Value::as_string)
                .map(str::to_owned);
            if url.is_some() || title.is_some() {
                out.push(Bookmark { url, title });
            }
        }
    }
    Some(out)
}

/// One download (`TSK_WEB_DOWNLOAD`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub url: Option<String>,
    pub path: Option<String>,
    /// Unix seconds (Autopsy stores milliseconds by mistake).
    pub time: Option<i64>,
}

/// Downloads of `Downloads.plist` (`DownloadHistory`).
pub fn downloads(data: &[u8]) -> Option<Vec<Download>> {
    let root = parse_plist(data)?;
    let hist = root.as_dictionary()?.get("DownloadHistory")?.as_array()?;
    Some(
        hist.iter()
            .filter_map(Value::as_dictionary)
            .map(|d| Download {
                url: d
                    .get("DownloadEntryURL")
                    .and_then(Value::as_string)
                    .map(str::to_owned),
                path: d
                    .get("DownloadEntryPath")
                    .and_then(Value::as_string)
                    .map(str::to_owned),
                time: d
                    .get("DownloadEntryDateAddedKey")
                    .and_then(Value::as_date)
                    .map(|date| {
                        let t = SystemTime::from(date);
                        match t.duration_since(UNIX_EPOCH) {
                            Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
                            Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
                        }
                    }),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xml(body: &str) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0">{body}</plist>"#
        )
        .into_bytes()
    }

    #[test]
    fn bookmarks_tree() {
        let data = xml(r"<dict><key>Children</key><array>
              <dict><key>Title</key><string>BookmarksBar</string><key>Children</key><array>
                <dict><key>URLString</key><string>http://a.com/</string>
                  <key>URIDictionary</key><dict><key>title</key><string>A</string></dict></dict>
              </array></dict>
              <dict><key>URLString</key><string>http://b.com/</string></dict>
              <string>junk</string>
            </array></dict>");
        let b = bookmarks(&data).unwrap();
        assert_eq!(
            b,
            vec![
                Bookmark {
                    url: Some("http://a.com/".into()),
                    title: Some("A".into())
                },
                Bookmark {
                    url: Some("http://b.com/".into()),
                    title: None
                }
            ]
        );
        assert!(bookmarks(b"garbage").is_none());
    }

    #[test]
    fn downloads_plist() {
        let data = xml(r"<dict><key>DownloadHistory</key><array><dict>
              <key>DownloadEntryURL</key><string>https://dl.example.com/a.dmg</string>
              <key>DownloadEntryPath</key><string>/Users/u/Downloads/a.dmg</string>
              <key>DownloadEntryDateAddedKey</key><date>2020-09-13T12:26:40Z</date>
            </dict></array></dict>");
        let d = downloads(&data).unwrap();
        assert_eq!(d[0].time, Some(1_600_000_000));
        assert_eq!(d[0].path.as_deref(), Some("/Users/u/Downloads/a.dmg"));
        // Binary plists work too.
        let mut bin = Vec::new();
        parse_plist(&data)
            .unwrap()
            .to_writer_binary(&mut bin)
            .unwrap();
        assert_eq!(downloads(&bin).unwrap(), d);
    }
}
