//! Shellbags: folders a user browsed in Explorer, from the `BagMRU` trees
//! of NTUSER.DAT (XP/2003) and UsrClass.dat (Vista+). Each numbered value
//! of a `BagMRU` key is a shell item; the subkey with the same number holds
//! its children (RegRipper `shellbags`, `shellbags_xp`).

use std::collections::HashSet;

use pm_lnk::shell_item::{ShellItem, parse_item};
use pm_registry::{Hive, Key};

use super::{key, subkeys};

/// `BagMRU` roots searched in NTUSER.DAT.
pub const NTUSER_ROOTS: &[&str] = &[
    "Software\\Microsoft\\Windows\\ShellNoRoam\\BagMRU",
    "Software\\Microsoft\\Windows\\Shell\\BagMRU",
];
/// `BagMRU` roots searched in UsrClass.dat.
pub const USRCLASS_ROOTS: &[&str] = &[
    "Local Settings\\Software\\Microsoft\\Windows\\Shell\\BagMRU",
    "Local Settings\\Software\\Microsoft\\Windows\\ShellNoRoam\\BagMRU",
];

const MAX_DEPTH: usize = 64;
const MAX_BAGS: usize = 200_000;

/// One shellbag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellBag {
    /// Full shell path (`My Computer\C:\Windows`).
    pub path: String,
    /// Registry key of the bag (`…\BagMRU\0\1`).
    pub key: String,
    /// Last write of the bag's key (Unix seconds).
    pub last_written: Option<i64>,
    /// Times stored in the shell item (file-entry items).
    pub modified: Option<i64>,
    pub created: Option<i64>,
    pub accessed: Option<i64>,
    pub mft_entry: Option<u64>,
}

/// Shellbags under the given roots.
pub fn shellbags(h: &Hive, roots: &[&str]) -> Vec<ShellBag> {
    let mut out = Vec::new();
    for r in roots {
        if let Some(k) = key(h, r) {
            let mut seen = HashSet::new();
            walk(&k, r, &[], 0, &mut seen, &mut out);
        }
    }
    out
}

fn walk(
    k: &Key<'_>,
    key_path: &str,
    parents: &[ShellItem],
    depth: usize,
    seen: &mut HashSet<u32>,
    out: &mut Vec<ShellBag>,
) {
    if depth > MAX_DEPTH || out.len() >= MAX_BAGS || !seen.insert(k.offset()) {
        return;
    }
    let children = subkeys(k);
    let mut values: Vec<(u32, Vec<u8>)> = k
        .values()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| Some((v.name().parse::<u32>().ok()?, v.data().ok()?)))
        .collect();
    values.sort_by_key(|(n, _)| *n);
    for (n, data) in values {
        let item = parse_item(&data);
        let mut chain = parents.to_vec();
        chain.push(item.clone());
        let child = children.iter().find(|c| c.name() == n.to_string());
        let child_path = format!("{key_path}\\{n}");
        out.push(ShellBag {
            path: pm_lnk::shell_item::join_path(&chain),
            key: child_path.clone(),
            last_written: child.and_then(Key::last_written_unix),
            modified: item.modified,
            created: item.created,
            accessed: item.accessed,
            mft_entry: item.mft_entry,
        });
        if let Some(c) = child {
            walk(c, &child_path, &chain, depth + 1, seen, out);
        }
    }
}
