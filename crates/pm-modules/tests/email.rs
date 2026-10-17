//! Email Parser end to end (built on pm-plugin-api): a Thunderbird MBOX,
//! an EML and a vCard on a generated NTFS volume.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod common;

use std::sync::Arc;

use common::DemoCase;
use pm_ingest::{JobSettings, ModuleSetting};
use pm_testkit::ntfs::NtfsBuilder;

const MBOX: &str = "From alice@example.com Thu Mar  4 09:00:00 2021\r
From: Alice <alice@example.com>\r
To: bob@example.com\r
Subject: Plans\r
Date: Thu, 4 Mar 2021 09:00:00 +0000\r
Message-ID: <m1@example.com>\r
Content-Type: multipart/mixed; boundary=\"b\"\r
\r
--b\r
Content-Type: text/plain\r
\r
See attached.\r
--b\r
Content-Type: text/plain; name=\"plan.txt\"\r
Content-Disposition: attachment; filename=\"plan.txt\"\r
\r
the plan\r
--b--\r
\r
From bob@example.com Thu Mar  4 10:00:00 2021\r
From: Bob <bob@example.com>\r
To: Alice <alice@example.com>\r
Cc: carol@example.org\r
Subject: Re: Plans\r
Date: Thu, 4 Mar 2021 10:00:00 +0000\r
Message-ID: <m2@example.com>\r
In-Reply-To: <m1@example.com>\r
References: <m1@example.com>\r
\r
Sounds good.\r
\r
From dave@example.net Fri Mar  5 08:00:00 2021\r
From: dave@example.net\r
To: bob@example.com\r
Subject: Lunch?\r
Date: Fri, 5 Mar 2021 08:00:00 +0000\r
Message-ID: <m3@example.net>\r
\r
Noon?\r
";

const EML: &str = "From: Eve <eve@example.com>\r\nTo: bob@example.com\r\nSubject: Hello\r\nDate: Sat, 6 Mar 2021 12:00:00 +0000\r\nMessage-ID: <e1@example.com>\r\n\r\nHi Bob\r\n";

const VCF: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Carol Example\r\nTEL;TYPE=CELL:+358401234567\r\nEMAIL:carol@example.org\r\nORG:Acme\r\nEND:VCARD\r\n";

fn image() -> Vec<u8> {
    let mut b = NtfsBuilder::new(16);
    let inbox = "/Users/bob/AppData/Roaming/Thunderbird/Profiles/p.default/Mail/Local Folders";
    let mut path = String::new();
    for part in inbox.trim_start_matches('/').split('/') {
        path.push('/');
        path.push_str(part);
        b.dir(&path);
    }
    b.dir("/Users/bob/Documents");
    b.file(&format!("{inbox}/Inbox"), MBOX.as_bytes());
    b.file("/Users/bob/Documents/hello.eml", EML.as_bytes());
    b.file("/Users/bob/Documents/carol.vcf", VCF.as_bytes());
    b.build()
}

fn run(case: &DemoCase) {
    let modules = pm_modules::core_modules();
    let settings = JobSettings {
        modules: modules
            .iter()
            .map(|m| ModuleSetting {
                id: m.info().id.to_owned(),
                enabled: matches!(m.info().id, "file_type" | "email_parser"),
                settings: m.default_settings(),
            })
            .collect(),
        ..JobSettings::default()
    };
    let (summary, _) = case.run(&settings, modules);
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
}

#[test]
fn mailboxes_messages_threads_attachments_accounts_and_contacts() {
    let case = DemoCase::with_image(&image());
    run(&case);
    let mail = case.artifacts("KAAMOS_EMAIL_MSG");
    assert_eq!(mail.len(), 4, "3 in the MBOX, 1 EML");
    let by_subject = |s: &str| {
        mail.iter()
            .find(|(_, a)| a.get("KAAMOS_SUBJECT").map(String::as_str) == Some(s))
            .unwrap()
    };
    let (m1, a1) = by_subject("Plans");
    let (_, a2) = by_subject("Re: Plans");
    let (_, a3) = by_subject("Lunch?");
    assert_eq!(a1["KAAMOS_EMAIL_FROM"], "Alice <alice@example.com>; ");
    assert_eq!(a1["KAAMOS_PATH"], "/Local Folders/Inbox");
    assert_eq!(a1["EMAIL_HAS_ATTACHMENT"], "Yes");
    assert_eq!(
        a1["KAAMOS_THREAD_ID"], a2["KAAMOS_THREAD_ID"],
        "reply threads with its parent"
    );
    assert_ne!(a1["KAAMOS_THREAD_ID"], a3["KAAMOS_THREAD_ID"]);
    assert_eq!(a2["KAAMOS_EMAIL_CC"], "carol@example.org; ");
    assert!(
        a1["KAAMOS_ATTACHMENTS"].contains("\"objectID\":"),
        "{}",
        a1["KAAMOS_ATTACHMENTS"]
    );

    // The attachment is a derived file of the mailbox.
    let inbox = case
        .files()
        .into_iter()
        .find(|f| f.name == "Inbox")
        .unwrap();
    let plan = case
        .files()
        .into_iter()
        .find(|f| f.name == "plan.txt")
        .expect("attachment");
    assert_eq!(plan.parent_id, Some(inbox.id));
    assert_eq!(
        common::read_all(&*case.case.content(&plan).unwrap()),
        b"the plan"
    );

    // Accounts and the MESSAGE relationship through the message.
    let db = case.case.db();
    let accounts: Vec<String> = db
        .accounts_of_artifact(m1.id)
        .unwrap()
        .into_iter()
        .map(|a| a.identifier)
        .collect();
    assert!(
        accounts.contains(&"alice@example.com".to_owned())
            && accounts.contains(&"bob@example.com".to_owned()),
        "{accounts:?}"
    );

    // The vCard: a contact with a typed phone number.
    let contacts = case.artifacts("KAAMOS_CONTACT");
    assert_eq!(contacts.len(), 1);
    let c = &contacts[0].1;
    assert_eq!(c["KAAMOS_NAME"], "Carol Example");
    assert_eq!(c["KAAMOS_PHONE_NUMBER_CELL"], "+358401234567");
    assert_eq!(c["KAAMOS_ORGANIZATION"], "Acme");

    // Re-running adds nothing.
    let files = case.files().len();
    run(&case);
    assert_eq!(case.artifacts("KAAMOS_EMAIL_MSG").len(), 4);
    assert_eq!(case.artifacts("KAAMOS_CONTACT").len(), 1);
    assert_eq!(case.files().len(), files);
    let _ = Arc::clone(&case.case);
}
