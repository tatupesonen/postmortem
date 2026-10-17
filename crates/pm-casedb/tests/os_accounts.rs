//! OS accounts, realms, hosts and persons: ports of TSK's
//! `bindings/java/test/org/sleuthkit/datamodel/OsAccountTest.java`
//! (host address tests excepted).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use pm_casedb::os_accounts::{
    AccountUpdate, InstanceKind, OsAccountAttribute, RealmScope, RecordStatus, ScopeConfidence,
};
use pm_casedb::*;

fn new_db() -> (tempfile::TempDir, CaseDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = CaseDb::create(dir.path().join("case.db")).unwrap();
    (dir, db)
}

fn add_ds(db: &CaseDb, name: &str) -> i64 {
    db.bulk(|b| {
        b.add_data_source(&NewDataSource {
            name: name.into(),
            kind: DataSourceKind::LogicalFiles,
            paths: vec![format!("/evidence/{name}")],
            size: 0,
            sector_size: 512,
            timezone: "EST".into(),
            image_format: None,
            md5: None,
            sha1: None,
            sha256: None,
            acquisition: Vec::new(),
            added_at: 0,
        })
    })
    .unwrap()
}

fn host(db: &CaseDb, name: &str) -> i64 {
    db.host_by_name_or_new(name).unwrap().id
}

fn realm_of(db: &CaseDb, a: &os_accounts::OsAccount) -> os_accounts::Realm {
    db.os_account_realm(a.realm_id).unwrap()
}

fn eq_ic(a: Option<&str>, b: &str) -> bool {
    a.is_some_and(|a| a.eq_ignore_ascii_case(b))
}

#[test]
fn host_tests() {
    let (_d, db) = new_db();
    let h1 = db.host_by_name_or_new("host11").unwrap();
    assert!(h1.name.eq_ignore_ascii_case("host11"));
    assert!(db.hosts().unwrap().iter().any(|h| h.id == h1.id));
    assert!(!db.hosts().unwrap().iter().any(|h| h.name == "host22"));
    let h2 = db.host_by_name_or_new("host22").unwrap();
    let h2_again = db.host_by_name_or_new("HOST22").unwrap();
    assert_eq!(
        h2.id, h2_again.id,
        "a duplicate name returns the existing host"
    );
    assert!(db.host_by_name_or_new("  ").is_err());
}

#[test]
fn person_tests() {
    let (_d, db) = new_db();
    let p = db.new_person("John Doe").unwrap();
    assert_eq!(p.name, "John Doe");
    db.rename_person(p.id, "Jane Doe").unwrap();
    assert_eq!(db.persons().unwrap()[0].name, "Jane Doe");
    let h = host(&db, "laptop");
    db.set_host_person(h, Some(p.id)).unwrap();
    assert_eq!(db.host(h).unwrap().unwrap().person_id, Some(p.id));
    db.delete_person(p.id).unwrap();
    assert!(db.persons().unwrap().is_empty());
    assert_eq!(
        db.host(h).unwrap().unwrap().person_id,
        None,
        "hosts are orphaned"
    );
}

#[test]
fn delete_host_is_refused_while_in_use() {
    let (_d, db) = new_db();
    let ds = add_ds(&db, "files");
    let h = host(&db, "used");
    db.set_data_source_host(ds, h).unwrap();
    assert!(db.delete_host(h).is_err());
    let free = host(&db, "free");
    db.delete_host(free).unwrap();
    assert!(db.host(free).unwrap().is_none());
}

#[test]
fn merge_host_tests() {
    let (_d, db) = new_db();
    let host1 = host(&db, "host1forHostMergeTest");
    let host2 = host(&db, "host2forHostMergeTest");
    let ds = add_ds(&db, "pathToFiles");
    db.set_data_source_host(ds, host1).unwrap();

    let sid3 = "S-1-5-27-777777777-854245398-1060284298-7777";
    let sid4 = "S-1-5-27-788888888-854245398-1060284298-8888";
    let sid5 = "S-1-5-27-799999999-854245398-1060284298-9999";
    let sid6 = "S-1-5-27-711111111-854245398-1060284298-1111";
    let sid7 = "S-1-5-27-733333333-854245398-1060284298-3333";
    let sid8 = "S-1-5-27-744444444-854245398-1060284298-4444";
    let local = RealmScope::Local;
    let r = |sid: Option<&str>, name: Option<&str>, h: i64| {
        db.new_windows_realm(sid, name, h, local).unwrap()
    };

    let realm1 = r(None, Some("hostMergeRealm1"), host1);
    let realm2 = r(None, Some("hostMergeRealm2"), host1);
    let realm3 = r(Some(sid3), None, host1);
    let realm4 = r(Some(sid4), Some("hostMergeRealm4"), host1);
    let _realm5 = r(Some(sid3), Some("hostMergeRealm2"), host2);
    let _realm6 = r(Some(sid5), Some("hostMergeRealm4"), host2);
    let realm7 = r(None, Some("hostMergeRealm5"), host2);
    let realm8 = r(None, Some("hostMergeRealm6"), host2);
    let realm8acct = db
        .new_windows_os_account(
            None,
            Some("hostMergeUniqueRealm8Account"),
            Some("hostMergeRealm6"),
            host2,
            local,
        )
        .unwrap();
    let realm9 = r(Some(sid6), None, host2);
    let _realm10 = r(Some(sid6), Some("hostMergeRealm6"), host1);
    let realm10acct = db
        .new_windows_os_account(
            None,
            Some("hostMergeUniqueRealm10Account"),
            Some("hostMergeRealm6"),
            host1,
            local,
        )
        .unwrap();
    let realm11 = r(Some(sid8), Some("hostMergeRealm7"), host1);
    let realm12 = r(None, Some("hostMergeRealm7"), host2);
    let _realm13 = r(Some(sid7), Some("notRealm8"), host1);
    let _realm14 = r(Some(sid7), Some("hostMergeRealm8"), host2);

    db.merge_hosts(host1, host2).unwrap();

    let check = |orig: &os_accounts::Realm,
                 status: RecordStatus,
                 addr: Option<Option<&str>>,
                 name: Option<&str>,
                 scope: Option<Option<i64>>| {
        let now = db.os_account_realm(orig.id).unwrap();
        assert_eq!(now.status, status, "realm {orig:?}");
        if let Some(addr) = addr {
            assert_eq!(now.addr.as_deref(), addr, "addr of {orig:?}");
        }
        if let Some(name) = name {
            assert_eq!(now.name.as_deref(), Some(name), "name of {orig:?}");
        }
        if let Some(scope) = scope {
            assert_eq!(now.scope_host_id, scope, "scope of {orig:?}");
        }
    };
    check(
        &realm1,
        RecordStatus::Active,
        Some(None),
        Some("hostMergeRealm1"),
        Some(Some(host2)),
    );
    check(&realm2, RecordStatus::Merged, None, None, None);
    check(&realm3, RecordStatus::Merged, None, None, None);
    check(
        &realm4,
        RecordStatus::Active,
        Some(realm4.addr.as_deref()),
        Some("hostMergeRealm4"),
        Some(Some(host2)),
    );
    check(
        &realm7,
        RecordStatus::Active,
        Some(None),
        Some("hostMergeRealm5"),
        Some(Some(host2)),
    );
    check(&realm8, RecordStatus::Merged, None, None, None);
    check(
        &realm9,
        RecordStatus::Active,
        Some(realm9.addr.as_deref()),
        Some("hostMergeRealm6"),
        Some(Some(host2)),
    );
    check(
        &realm12,
        RecordStatus::Active,
        Some(realm11.addr.as_deref()),
        Some("hostMergeRealm7"),
        Some(Some(host2)),
    );

    assert!(
        db.windows_realm(None, Some("notRealm8"), host1)
            .unwrap()
            .is_none()
    );
    assert!(
        db.windows_realm(None, Some("notRealm8"), host2)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.os_account(realm8acct.id).unwrap().unwrap().realm_id,
        realm9.id
    );
    assert_eq!(
        db.os_account(realm10acct.id).unwrap().unwrap().realm_id,
        realm9.id
    );

    assert_eq!(db.host_of_data_source(ds).unwrap().unwrap().id, host2);
    assert!(
        !db.hosts()
            .unwrap()
            .iter()
            .any(|h| h.name == "host1forHostMergeTest")
    );
    assert_ne!(
        host(&db, "host1forHostMergeTest"),
        host1,
        "the old name is free again"
    );
}

#[test]
fn merge_realms_tests() {
    let (_d, db) = new_db();
    let h = host(&db, "mergeTestHost");
    let (dest_name, src_name) = ("mergeTestDestRealm", "mergeTestSourceRealm");
    let sid1 = "S-1-5-21-222222222-222222222-1060284298-2222";
    let sid2 = "S-1-5-21-555555555-555555555-1060284298-5555";
    let local = RealmScope::Local;
    let src = db
        .new_windows_realm(None, Some(src_name), h, local)
        .unwrap();
    let dest = db
        .new_windows_realm(None, Some(dest_name), h, local)
        .unwrap();
    let acct = |sid: Option<&str>, login: Option<&str>, realm: &str| {
        db.new_windows_os_account(sid, login, Some(realm), h, local)
            .unwrap()
    };
    acct(None, Some("uniqueRealm1Account"), dest_name);
    acct(None, Some("matchingNameAccount"), dest_name);
    acct(None, Some("uniqueRealm2Account"), src_name);
    let a4 = acct(None, Some("matchingNameAccount"), src_name);
    db.update_os_account(
        a4.id,
        &AccountUpdate {
            full_name: Some("FullName1".into()),
            created_at: Some(555),
            ..AccountUpdate::default()
        },
    )
    .unwrap();
    acct(Some(sid1), None, dest_name);
    acct(Some(sid1), None, src_name);
    acct(Some(sid2), None, dest_name);
    acct(None, Some("nameForCombining"), dest_name);
    acct(Some(sid2), Some("nameForCombining"), src_name);

    assert!(db.windows_realm(None, Some(src_name), h).unwrap().is_some());
    let with_sid1 = |db: &CaseDb| {
        db.os_accounts(None, 0, 1000)
            .unwrap()
            .into_iter()
            .filter(|a| a.addr.as_deref() == Some(sid1))
            .count()
    };
    // Realms are separate until merged, but both look up the same SID by
    // address: TSK returns the first account for the second insert too.
    assert_eq!(with_sid1(&db), 1);

    db.merge_os_account_realms(src.id, dest.id).unwrap();
    assert!(db.windows_realm(None, Some(src_name), h).unwrap().is_none());
    assert_eq!(with_sid1(&db), 1);
    let in_dest = db.os_accounts_of_realm(dest.id).unwrap();
    assert!(
        in_dest
            .iter()
            .any(|a| a.login_name.as_deref() == Some("uniqueRealm2Account"))
    );
    let matching = in_dest
        .iter()
        .find(|a| a.login_name.as_deref() == Some("matchingNameAccount"))
        .unwrap();
    assert_eq!(matching.created_at, Some(555));
    assert!(eq_ic(matching.full_name.as_deref(), "FullName1"));
    let combined: Vec<_> = in_dest
        .iter()
        .filter(|a| {
            a.login_name.as_deref() == Some("nameForCombining") || a.addr.as_deref() == Some(sid2)
        })
        .collect();
    assert_eq!(
        combined.len(),
        1,
        "login-only and SID-only accounts combine: {combined:?}"
    );
}

#[test]
fn update_realm_and_merge_tests() {
    let (_d, db) = new_db();
    let h = host(&db, "updateRealmAndMergeTestHost");
    let sid = "S-1-5-21-1182664808-117526782-2525957323-13395";
    let local = RealmScope::Local;
    let a1 = db
        .new_windows_os_account(Some(sid), Some("sandip"), None, h, local)
        .unwrap();
    let realm1 = realm_of(&db, &a1);
    assert!(realm1.addr.is_some());
    assert!(realm1.name.is_none());

    assert!(
        db.windows_os_account(None, Some("sandip"), Some("CORP"), h)
            .unwrap()
            .is_none()
    );
    let a2 = db
        .new_windows_os_account(None, Some("sandip"), Some("CORP"), h, local)
        .unwrap();
    assert_ne!(a1.id, a2.id);
    assert_ne!(a1.realm_id, a2.realm_id);

    let a3 = db
        .windows_os_account(Some(sid), Some("sandip"), Some("CORP"), h)
        .unwrap()
        .unwrap();
    let updated = db
        .update_windows_os_account_core(a3.id, Some(sid), Some("sandip"), Some("CORP"), h)
        .unwrap();
    let realm3 = realm_of(&db, &updated);
    assert_eq!(realm3.id, realm1.id);
    assert!(eq_ic(
        realm3.addr.as_deref(),
        "S-1-5-21-1182664808-117526782-2525957323"
    ));
    assert!(eq_ic(realm3.name.as_deref(), "CORP"));
    assert_eq!(
        db.os_account_realm(a2.realm_id).unwrap().status,
        RecordStatus::Merged
    );
    assert_eq!(
        db.os_account(a2.id).unwrap().unwrap().db_status,
        RecordStatus::Merged
    );
}

#[test]
fn os_account_realm_tests() {
    let (_d, db) = new_db();
    let host1 = host(&db, "host1");
    let realm_sid1 = "S-1-5-21-1111111111-2222222222-3333333333";
    let realm_addr1 = "S-1-5-21-1111111111-2222222222";
    let domain = db
        .new_windows_realm(Some(realm_sid1), Some("basis"), host1, RealmScope::Domain)
        .unwrap();
    assert!(eq_ic(domain.name.as_deref(), "basis"));
    assert_eq!(domain.scope_confidence, ScopeConfidence::Known);
    assert_eq!(domain.addr.as_deref(), Some(realm_addr1));

    let realm_sid2 = "S-1-5-18-2033736216-1234567890-5432109876";
    let realm_addr2 = "S-1-5-18-2033736216-1234567890";
    let host2 = host(&db, "host2");
    let local2 = db
        .new_windows_realm(Some(realm_sid2), None, host2, RealmScope::Local)
        .unwrap();
    assert!(eq_ic(local2.addr.as_deref(), realm_addr2));
    assert_eq!(local2.scope_host_id, Some(host2));
    assert!(
        db.update_os_account_realm(local2.id, None, Some("win-raman-abcd"))
            .unwrap()
    );
    let updated = db.os_account_realm(local2.id).unwrap();
    assert!(eq_ic(updated.addr.as_deref(), realm_addr2));
    assert!(eq_ic(updated.name.as_deref(), "win-raman-abcd"));

    let host3 = host(&db, "host3");
    let found = db
        .windows_realm(Some(&format!("{realm_addr1}-88888888")), None, host3)
        .unwrap()
        .expect("domain realm visible from any host");
    assert!(eq_ic(found.addr.as_deref(), realm_addr1));
    assert!(eq_ic(found.name.as_deref(), "basis"));

    let host4 = host(&db, "host4");
    assert!(
        db.windows_realm(Some(realm_sid2), None, host4)
            .unwrap()
            .is_none(),
        "local realm of another host"
    );
}

#[test]
fn basic_os_account_tests() {
    let (_d, db) = new_db();
    let uid1 = "S-1-5-21-111111111-222222222-3333333333-1001";
    let host1 = host(&db, "host1");
    let a1 = db
        .new_windows_os_account(
            Some(uid1),
            Some("jay"),
            Some("local"),
            host1,
            RealmScope::Local,
        )
        .unwrap();
    assert!(eq_ic(a1.addr.as_deref(), uid1));
    assert!(eq_ic(realm_of(&db, &a1).name.as_deref(), "local"));

    let a11 = db
        .new_windows_os_account(
            Some(uid1),
            Some("BlueJay"),
            Some("DESKTOP-9TO5"),
            host1,
            RealmScope::Domain,
        )
        .unwrap();
    assert!(eq_ic(a11.addr.as_deref(), uid1));
    assert!(eq_ic(realm_of(&db, &a11).name.as_deref(), "local"));
    assert!(
        eq_ic(a11.login_name.as_deref(), "jay"),
        "the existing account is returned"
    );

    let a21 = db
        .windows_os_account(Some("S-1-0-0"), Some("jay"), Some("local"), host1)
        .unwrap()
        .unwrap();
    assert!(eq_ic(a21.addr.as_deref(), uid1));
    assert!(eq_ic(a21.login_name.as_deref(), "jay"));

    let a1 = db
        .update_os_account(
            a1.id,
            &AccountUpdate {
                full_name: Some("Johnny Depp".into()),
                created_at: Some(1_611_858_618),
                ..AccountUpdate::default()
            },
        )
        .unwrap();
    assert_eq!(a1.created_at, Some(1_611_858_618));
    assert!(eq_ic(a1.full_name.as_deref(), "Johnny Depp"));

    let copy = db
        .new_windows_os_account(Some(uid1), None, Some("local"), host1, RealmScope::Local)
        .unwrap();
    assert_eq!(copy.id, a1.id);
    assert!(eq_ic(copy.full_name.as_deref(), "Johnny Depp"));

    let uid2 = "S-1-5-21-725345543-854245398-1060284298-1003";
    let uid3 = "S-1-5-21-725345543-854245398-1060284298-1004";
    let host2 = host(&db, "host2");
    let host3 = host(&db, "host3");
    db.new_windows_realm(Some(uid2), Some("basis"), host2, RealmScope::Domain)
        .unwrap();
    let a2 = db
        .new_windows_os_account(Some(uid2), None, Some("basis"), host2, RealmScope::Domain)
        .unwrap();
    let a3 = db
        .new_windows_os_account(Some(uid3), None, Some("basis"), host3, RealmScope::Domain)
        .unwrap();
    assert!(eq_ic(a2.addr.as_deref(), uid2));
    assert!(eq_ic(realm_of(&db, &a2).name.as_deref(), "basis"));
    assert!(eq_ic(a3.addr.as_deref(), uid3));
    assert_eq!(a2.realm_id, a3.realm_id, "one domain realm across hosts");
}

#[test]
fn windows_special_account_tests() {
    let (_d, db) = new_db();
    let unknown = RealmScope::Unknown;
    {
        let h = host(&db, "host222");
        for sid in ["S-1-5-18", "S-1-5-19", "S-1-5-20"] {
            let a = db
                .new_windows_os_account(Some(sid), None, None, h, unknown)
                .unwrap();
            assert_eq!(realm_of(&db, &a).addr.as_deref(), Some("S-1-5"));
        }
    }
    {
        let h = host(&db, "host333");
        let system = db
            .new_windows_os_account(Some("S-1-5-18"), None, None, h, unknown)
            .unwrap();
        let realm = realm_of(&db, &system);
        assert_eq!(realm.addr.as_deref(), Some("S-1-5"));
        assert_eq!(realm.scope_host_id, Some(h));
        assert_eq!(realm.name.as_deref(), Some("NT AUTHORITY"));
        assert_eq!(system.login_name.as_deref(), Some("SYSTEM"));
        assert_eq!(system.full_name.as_deref(), Some("Local System Account"));

        let s11 = db
            .windows_os_account(None, Some("SYSTEM"), Some("NT instans"), h)
            .unwrap()
            .unwrap();
        assert_eq!(s11.id, system.id);
        let s12 = db
            .windows_os_account(None, Some("SYSTÈME"), Some("AUTORITE NT"), h)
            .unwrap()
            .unwrap();
        assert_eq!(s12.id, system.id);
        let s13 = db
            .new_windows_os_account(
                Some("S-1-5-18"),
                Some("SYSTEM"),
                Some("NT instans"),
                h,
                unknown,
            )
            .unwrap();
        assert_eq!(s13.realm_id, system.realm_id);

        let local_service = db
            .new_windows_os_account(
                Some("S-1-5-19"),
                Some("SERVIZIO LOCALE"),
                Some("AUTORITE NT"),
                h,
                unknown,
            )
            .unwrap();
        assert_eq!(realm_of(&db, &local_service).scope_host_id, Some(h));
        assert_eq!(local_service.login_name.as_deref(), Some("LOCAL SERVICE"));
        let ls = db
            .windows_os_account(None, Some("LOKALER DIENST"), Some("NT INSTANS"), h)
            .unwrap()
            .unwrap();
        assert_eq!(ls.id, local_service.id);

        let network = db
            .new_windows_os_account(None, Some("NETZWERKDIENST"), Some("NT instans"), h, unknown)
            .unwrap();
        assert_eq!(
            realm_of(&db, &network).name.as_deref(),
            Some("NT AUTHORITY")
        );
        assert_eq!(network.login_name.as_deref(), Some("NETWORK SERVICE"));
        let n1 = db
            .windows_os_account(Some("S-1-5-20"), None, None, h)
            .unwrap()
            .unwrap();
        assert_eq!(n1.id, network.id);
        let n2 = db
            .windows_os_account(None, Some("SERVICE RÉSEAU"), Some("AUTORITE NT"), h)
            .unwrap()
            .unwrap();
        assert_eq!(n2.id, network.id);
    }
    {
        let h = host(&db, "host444");
        for (sid, addr) in [
            (
                "S-1-5-80-3696737894-3623014651-202832235-645492566-13622391",
                "S-1-5-80",
            ),
            (
                "S-1-5-82-4003674586-223046494-4022293810-2417516693-151509167",
                "S-1-5-82",
            ),
            ("S-1-5-90-0-2", "S-1-5-90"),
            ("S-1-5-96-0-3", "S-1-5-96"),
            ("S-1-5-99-0-3", "S-1-5-99"),
            ("S-1-5-100-0-3", "S-1-5-100"),
            ("S-1-5-111-0-3", "S-1-5-111"),
            ("S-1-5-112-0-3", "S-1-5-112-0"), // not special
            ("S-1-5-79-0-3", "S-1-5-79-0"),   // not special
        ] {
            let a = db
                .new_windows_os_account(Some(sid), None, None, h, unknown)
                .unwrap();
            assert!(eq_ic(realm_of(&db, &a).addr.as_deref(), addr), "{sid}");
        }
    }
    {
        let h = host(&db, "host555");
        let realm = Some("realmName555");
        for sid in [
            "S-1-5-32-544",
            "S-1-5-21-725345543-854245398-1060284298-512",
            "S-1-1-0",
        ] {
            assert!(
                db.new_windows_os_account(Some(sid), None, realm, h, unknown)
                    .is_err(),
                "{sid} is a group"
            );
        }
        assert!(
            db.new_windows_os_account(Some("S-1-0-0"), None, realm, h, unknown)
                .is_err(),
            "NULL SID, no login"
        );
        let a5 = db
            .new_windows_os_account(Some("S-1-0-0"), Some("login5"), realm, h, unknown)
            .unwrap();
        assert!(a5.addr.is_none(), "the NULL SID is not stored");
        assert!(eq_ic(realm_of(&db, &a5).name.as_deref(), "realmName555"));
        assert_eq!(a5.login_name.as_deref(), Some("login5"));
    }
}

#[test]
fn os_account_instance_tests() {
    let (_d, db) = new_db();
    let ds = add_ds(&db, "image");
    let uid = "S-1-5-21-111111111-222222222-3333333333-0001";
    let h1 = host(&db, "host1111");
    db.new_windows_realm(Some(uid), Some("realm1111"), h1, RealmScope::Local)
        .unwrap();
    let a = db
        .new_windows_os_account(Some(uid), None, Some("realm1111"), h1, RealmScope::Local)
        .unwrap();
    let kinds = |db: &CaseDb| {
        db.os_account_instances(a.id)
            .unwrap()
            .iter()
            .map(|i| i.kind)
            .collect::<Vec<_>>()
    };

    db.add_os_account_instance(a.id, ds, InstanceKind::Accessed)
        .unwrap();
    assert_eq!(kinds(&db), [InstanceKind::Accessed]);
    db.add_os_account_instance(a.id, ds, InstanceKind::Referenced)
        .unwrap();
    assert_eq!(
        kinds(&db),
        [InstanceKind::Accessed],
        "less significant: no change"
    );
    db.add_os_account_instance(a.id, ds, InstanceKind::Launched)
        .unwrap();
    assert_eq!(
        kinds(&db),
        [InstanceKind::Launched],
        "more significant: replaces"
    );
    db.add_os_account_instance(a.id, ds, InstanceKind::Referenced)
        .unwrap();
    db.add_os_account_instance(a.id, ds, InstanceKind::Accessed)
        .unwrap();
    assert_eq!(kinds(&db), [InstanceKind::Launched]);

    db.set_data_source_host(ds, h1).unwrap();
    assert_eq!(db.hosts_of_os_account(a.id).unwrap(), ["host1111"]);
    assert_eq!(db.os_accounts_of_host(h1).unwrap()[0].id, a.id);
    assert_eq!(db.os_accounts(Some(ds), 0, 10).unwrap()[0].id, a.id);

    let h2 = host(&db, "host2222");
    let reset = catalog::attribute_type_id("TSK_DATETIME_PASSWORD_RESET").unwrap();
    let hint = catalog::attribute_type_id("TSK_PASSWORD_HINT").unwrap();
    db.add_os_account_attributes(
        a.id,
        &[
            OsAccountAttribute {
                type_id: reset,
                value: Value::DateTime(1_611_859_999),
                host_id: None,
                source_file_id: None,
            },
            OsAccountAttribute {
                type_id: hint,
                value: Value::Text("HINT".into()),
                host_id: Some(h2),
                source_file_id: None,
            },
        ],
    )
    .unwrap();
    let attrs = db.os_account_attributes(a.id).unwrap();
    assert_eq!(attrs.len(), 2);
    assert_eq!(attrs[0].0.value, Value::DateTime(1_611_859_999));
    assert_eq!(attrs[1].0.value, Value::Text("HINT".into()));
    assert_eq!(attrs[1].0.host_id, Some(h2));
}

#[test]
fn windows_account_realm_update_tests() {
    let (_d, db) = new_db();
    let h1 = host(&db, "host4444");
    let a1 = db
        .new_windows_os_account(
            Some("S-1-5-21-111111111-222222222-4444444444-0001"),
            None,
            None,
            h1,
            RealmScope::Local,
        )
        .unwrap();
    let realm1 = realm_of(&db, &a1);
    assert!(eq_ic(
        realm1.addr.as_deref(),
        "S-1-5-21-111111111-222222222-4444444444"
    ));
    assert!(realm1.name.is_none());
    let a2 = db
        .new_windows_os_account(
            Some("S-1-5-21-111111111-222222222-4444444444-0002"),
            None,
            Some("realm4444"),
            h1,
            RealmScope::Local,
        )
        .unwrap();
    assert_eq!(a1.realm_id, a2.realm_id);
    assert!(
        eq_ic(realm_of(&db, &a2).name.as_deref(), "realm4444"),
        "the realm learns its name"
    );

    let h3 = host(&db, "host4444_3");
    let a3 = db
        .new_windows_os_account(
            None,
            Some("User4444_3"),
            Some("realm4444_3"),
            h3,
            RealmScope::Domain,
        )
        .unwrap();
    let realm3 = realm_of(&db, &a3);
    assert!(realm3.addr.is_none());
    assert!(eq_ic(realm3.name.as_deref(), "realm4444_3"));

    let h4 = host(&db, "host4444_4");
    let a4 = db
        .new_windows_os_account(
            Some("S-1-5-21-111111111-444444444-4444444444-0001"),
            Some("User4444_4"),
            Some("realm4444_3"),
            h4,
            RealmScope::Domain,
        )
        .unwrap();
    assert_eq!(a3.realm_id, a4.realm_id);
    let realm4 = realm_of(&db, &a4);
    assert!(eq_ic(
        realm4.addr.as_deref(),
        "S-1-5-21-111111111-444444444-4444444444"
    ));
    assert!(eq_ic(realm4.name.as_deref(), "realm4444_3"));
}

#[test]
fn windows_account_update_tests() {
    let (_d, db) = new_db();
    let h = host(&db, "host55555");
    let uid1 = "S-1-5-21-111111111-222222222-555555555-0001";
    let a1 = db
        .new_windows_os_account(Some(uid1), None, None, h, RealmScope::Domain)
        .unwrap();
    let up = db
        .update_windows_os_account_core(a1.id, None, Some("jbravo"), None, h)
        .unwrap();
    assert!(eq_ic(up.addr.as_deref(), uid1));
    assert!(eq_ic(up.login_name.as_deref(), "jbravo"));
    assert!(
        up.signature.eq_ignore_ascii_case(uid1),
        "signature stays the SID"
    );
    let realm_addr = "S-1-5-21-111111111-222222222-555555555";
    let realm1 = realm_of(&db, &up);
    assert!(eq_ic(realm1.addr.as_deref(), realm_addr));
    assert!(
        realm1
            .signature
            .eq_ignore_ascii_case(&format!("{realm_addr}_DOMAIN"))
    );

    let a2 = db
        .new_windows_os_account(
            None,
            Some("janeB"),
            Some("realm55555"),
            h,
            RealmScope::Domain,
        )
        .unwrap();
    assert!(a2.addr.is_none());
    assert!(a2.signature.eq_ignore_ascii_case("janeB"));
    let uid2 = "S-1-5-21-111111111-222222222-555555555-0007";
    let up2 = db
        .update_windows_os_account_core(a2.id, Some(uid2), None, Some("realm55555"), h)
        .unwrap();
    assert!(eq_ic(up2.addr.as_deref(), uid2));
    assert!(eq_ic(up2.login_name.as_deref(), "janeB"));
    assert!(
        up2.signature.eq_ignore_ascii_case(uid2),
        "signature becomes the SID"
    );
    let realm2 = realm_of(&db, &up2);
    assert!(eq_ic(realm2.addr.as_deref(), realm_addr));
    assert!(
        realm2
            .signature
            .eq_ignore_ascii_case(&format!("{realm_addr}_DOMAIN"))
    );
    assert!(
        db.update_windows_os_account_core(up2.id, Some(uid1), None, None, h)
            .is_err(),
        "SIDs never change"
    );
}

#[test]
fn windows_account_merge_tests() {
    let (_d, db) = new_db();
    let h = host(&db, "windowsAccountMergeTestHost");
    let sid = "S-1-5-21-111111111-222222222-666666666-0001";
    db.new_windows_os_account(Some(sid), None, None, h, RealmScope::Local)
        .unwrap();
    db.new_windows_os_account(None, Some("jdoe"), Some("testRealm"), h, RealmScope::Local)
        .unwrap();
    let found = db
        .windows_os_account(Some(sid), Some("jdoe"), Some("testRealm"), h)
        .unwrap()
        .unwrap();
    db.update_windows_os_account_core(found.id, Some(sid), Some("jdoe"), Some("testRealm"), h)
        .unwrap();
    let all = db.os_accounts(None, 0, 100).unwrap();
    assert_eq!(
        all.iter()
            .filter(|a| a.addr.as_deref() == Some(sid))
            .count(),
        1
    );
    assert_eq!(
        all.iter()
            .filter(|a| a.login_name.as_deref() == Some("jdoe"))
            .count(),
        1
    );
}

#[test]
fn default_host_for_images() {
    assert_eq!(default_host_name(Some("/x/disk.E01"), 3), "disk.E01 Host");
}
