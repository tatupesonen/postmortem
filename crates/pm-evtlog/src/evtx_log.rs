//! EVTX logs through the `evtx` crate. Each record's XML is parsed with
//! `roxmltree` into [`EventRecord`] fields; the XML is kept so event maps
//! ([`crate::maps`]) can evaluate their XPath expressions.

use evtx::{EvtxParser, ParserSettings};
use pm_core::SharedSource;
use pm_core::io::Cursor;
use roxmltree::{Document, Node};

use crate::{EventRecord, EvtLogError, LogFormat};

/// Parses an EVTX log; `f` returning `false` stops early. Corrupt
/// records are skipped (logged at debug level).
pub fn parse_evtx(
    src: &SharedSource,
    f: &mut dyn FnMut(EventRecord) -> bool,
) -> Result<(), EvtLogError> {
    let cursor = Cursor::new(std::sync::Arc::clone(src));
    let mut parser = EvtxParser::from_read_seek(cursor)
        .map_err(|e| EvtLogError::Evtx(e.to_string()))?
        .with_configuration(ParserSettings::default().num_threads(1));
    for rec in parser.records() {
        match rec {
            Ok(r) => {
                let Some(e) = from_xml(r.data, r.event_record_id, r.timestamp.as_second()) else {
                    continue;
                };
                if !f(e) {
                    break;
                }
            }
            Err(e) => tracing::debug!("skipping EVTX record: {e}"),
        }
    }
    Ok(())
}

/// Child element by local name.
pub(crate) fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
}

/// XPath-style string value: all descendant text.
pub(crate) fn string_value(n: Node<'_, '_>) -> String {
    if n.is_text() {
        return n.text().unwrap_or_default().to_owned();
    }
    n.descendants()
        .filter(Node::is_text)
        .filter_map(|d| d.text())
        .collect()
}

/// Builds a record from its XML (`None` if it has no `System/EventID`).
pub fn from_xml(xml: String, record_id: u64, time: i64) -> Option<EventRecord> {
    let (channel, provider, event_id, computer, user_sid, data) = {
        let doc = Document::parse(&xml).ok()?;
        let event = doc.root_element();
        let sys = child(event, "System")?;
        let text_of = |name: &str| child(sys, name).map(string_value).unwrap_or_default();
        let event_id = text_of("EventID").trim().parse::<u32>().ok()?;
        let provider = child(sys, "Provider")
            .and_then(|p| p.attribute("Name"))
            .unwrap_or_default()
            .to_owned();
        let user_sid = child(sys, "Security")
            .and_then(|s| s.attribute("UserID"))
            .map(str::to_owned);
        let mut data = Vec::new();
        if let Some(ed) = child(event, "EventData") {
            let mut unnamed = 0;
            for d in ed.children().filter(Node::is_element) {
                let value = string_value(d);
                if let Some(name) = d.attribute("Name") {
                    data.push((name.to_owned(), value));
                } else {
                    unnamed += 1;
                    data.push((unnamed.to_string(), value));
                }
            }
        }
        if let Some(ud) = child(event, "UserData") {
            for leaf in ud
                .descendants()
                .filter(|d| d.is_element() && !d.children().any(|c| c.is_element()))
            {
                data.push((leaf.tag_name().name().to_owned(), string_value(leaf)));
            }
        }
        (
            text_of("Channel"),
            provider,
            event_id,
            text_of("Computer"),
            user_sid,
            data,
        )
    };
    Some(EventRecord {
        format: LogFormat::Evtx,
        channel,
        provider,
        event_id,
        record_id,
        time,
        computer,
        user_sid,
        data,
        xml: Some(xml),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const LOGON: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Event xmlns="http://schemas.microsoft.com/win/2004/08/events/event">
  <System>
    <Provider Name="Microsoft-Windows-Security-Auditing" Guid="{54849625-5478-4994-A5BA-3E3B0328C30D}"></Provider>
    <EventID>4624</EventID>
    <TimeCreated SystemTime="2020-09-13T12:26:40.123Z"></TimeCreated>
    <EventRecordID>7</EventRecordID>
    <Channel>Security</Channel>
    <Computer>WS01</Computer>
    <Security UserID="S-1-5-18"></Security>
  </System>
  <EventData>
    <Data Name="SubjectUserName">WS01$</Data>
    <Data Name="SubjectDomainName">CORP</Data>
    <Data Name="TargetUserName">alice</Data>
    <Data Name="TargetDomainName">CORP</Data>
    <Data Name="TargetLogonId">0x3e7</Data>
    <Data Name="LogonType">10</Data>
    <Data Name="LogonProcessName">User32 </Data>
    <Data Name="AuthenticationPackageName">Negotiate</Data>
    <Data Name="WorkstationName">LAPTOP</Data>
    <Data Name="IpAddress">10.0.0.5</Data>
    <Data Name="ProcessName">C:\Windows\System32\svchost.exe</Data>
  </EventData>
</Event>"#;

    #[test]
    fn parses_system_named_and_user_data() {
        let r = from_xml(LOGON.to_owned(), 7, 1_600_000_000).unwrap();
        assert_eq!(r.event_id, 4624);
        assert_eq!(r.channel, "Security");
        assert_eq!(r.provider, "Microsoft-Windows-Security-Auditing");
        assert_eq!(r.computer, "WS01");
        assert_eq!(r.get("targetusername"), Some("alice"));
        assert_eq!(r.get("LogonType"), Some("10"));
        assert_eq!(r.user_sid.as_deref(), Some("S-1-5-18"));

        let xml = r#"<Event><System><EventID Qualifiers="16384">7045</EventID><Channel>System</Channel></System>
            <EventData><Data>svc</Data><Data>C:\evil.exe</Data></EventData>
            <UserData><LogFileCleared><SubjectUserName>bob</SubjectUserName></LogFileCleared></UserData></Event>"#;
        let r = from_xml(xml.to_owned(), 1, 0).unwrap();
        assert_eq!(r.event_id, 7045);
        assert_eq!(r.nth(2), Some("C:\\evil.exe"));
        assert_eq!(r.get("SubjectUserName"), Some("bob"));
        assert!(from_xml("<x/>".into(), 0, 0).is_none());
        assert!(from_xml("not xml".into(), 0, 0).is_none());
    }
}
