//! Autopsy's standard interesting files sets (`thirdparty/
//! InterestingFileSetRules`, Apache-2.0, Basis Technology), embedded
//! verbatim, and `StandardInterestingFilesSetsLoader`'s merge with the
//! user's sets.

use crate::{FilesSet, xml};

/// `(file name, XML)` of each standard set file.
pub const STANDARD_SET_FILES: [(&str, &str); 5] = [
    (
        "Cloud Storage.xml",
        include_str!("../data/Cloud Storage.xml"),
    ),
    (
        "Cryptocurrency Wallets.xml",
        include_str!("../data/Cryptocurrency Wallets.xml"),
    ),
    (
        "Encryption Programs.xml",
        include_str!("../data/Encryption Programs.xml"),
    ),
    (
        "Privacy Programs.xml",
        include_str!("../data/Privacy Programs.xml"),
    ),
    (
        "Remote Monitoring and Management Programs.xml",
        include_str!("../data/Remote Monitoring and Management Programs.xml"),
    ),
];

/// The standard sets (`readStandardFileXML`), marked standard; a newer
/// version of a set in a later file replaces an older one.
pub fn standard_sets() -> Vec<FilesSet> {
    let mut out: Vec<FilesSet> = Vec::new();
    for (file, text) in STANDARD_SET_FILES {
        match xml::read(text) {
            Ok(sets) => {
                let sets = sets
                    .into_iter()
                    .map(|s| FilesSet {
                        standard_set: true,
                        ..s
                    })
                    .collect();
                merge(sets, &mut out, false);
            }
            Err(e) => tracing::warn!("standard interesting files set {file}: {e}"),
        }
    }
    out
}

/// `copyOnNewer(src, dest, appendCustom)`: a set from `src` replaces the
/// one of the same name in `dest` when its version is newer. With
/// `append_custom`, a standard set never overwrites a user's set of the
/// same name: the user's set is kept as "<name> (Custom)".
pub fn merge(src: Vec<FilesSet>, dest: &mut Vec<FilesSet>, append_custom: bool) {
    for s in src {
        let Some(i) = dest.iter().position(|d| d.name == s.name) else {
            dest.push(s);
            continue;
        };
        let Some(d) = dest.get_mut(i) else { continue };
        if append_custom && s.standard_set != d.standard_set {
            if s.standard_set {
                let custom = dest.remove(i);
                add_custom(dest, custom);
                dest.push(s);
            } else {
                add_custom(dest, s);
            }
            continue;
        }
        if d.version < s.version {
            *d = s;
        }
    }
}

fn add_custom(dest: &mut Vec<FilesSet>, set: FilesSet) {
    if set.standard_set {
        return;
    }
    let mut set = set;
    loop {
        set = FilesSet {
            name: format!("{} (Custom)", set.name),
            standard_set: false,
            ..set
        };
        if !dest.iter().any(|d| d.name == set.name) {
            break;
        }
    }
    dest.push(set);
}

/// The sets Autopsy offers after start-up: the user's sets with the
/// standard ones merged in (`StandardInterestingFilesSetsLoader.run`).
pub fn with_standard_sets(user: Vec<FilesSet>) -> Vec<FilesSet> {
    let mut dest = user;
    merge(standard_sets(), &mut dest, true);
    dest
}
