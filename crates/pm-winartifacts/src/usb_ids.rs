//! USB vendor/product names (Autopsy `UsbDeviceIdMapper`).
//!
//! The database is the linux-usb.org `usb.ids` list shipped with Autopsy
//! (version 2022.05.20, used under its BSD-3-Clause option), cut before the
//! device-class section. It is parsed once per process.

use std::collections::HashMap;
use std::sync::OnceLock;

const DB: &str = include_str!("../data/usb.ids");

/// Vendor and product names of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbInfo {
    pub vendor: String,
    pub product: String,
}

struct Db {
    vendors: HashMap<u16, &'static str>,
    products: HashMap<(u16, u16), &'static str>,
}

fn db() -> &'static Db {
    static DB_CELL: OnceLock<Db> = OnceLock::new();
    DB_CELL.get_or_init(|| {
        let mut vendors = HashMap::new();
        let mut products = HashMap::new();
        let mut vendor: Option<u16> = None;
        for line in DB.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            if line.starts_with("\t\t") {
                continue; // interface line
            }
            let (indented, body) = match line.strip_prefix('\t') {
                Some(b) => (true, b),
                None => (false, line),
            };
            let Some((id, name)) = body.split_once("  ") else {
                continue;
            };
            let Ok(id) = u16::from_str_radix(id.trim(), 16) else {
                continue;
            };
            if indented {
                if let Some(v) = vendor {
                    products.insert((v, id), name.trim());
                }
            } else {
                vendor = Some(id);
                vendors.insert(id, name.trim());
            }
        }
        Db { vendors, products }
    })
}

/// Autopsy's `parseAndLookup`: `dev` like `VID_0781&PID_5567` (any case,
/// extra `&MI_00` parts ignored). Exact vendor+product, else the vendor
/// with product `"Product: <pid>"`, else `None`.
pub fn lookup(dev: &str) -> Option<UsbInfo> {
    let parts: Vec<&str> = dev.split(['_', '&']).collect();
    let vid = u16::from_str_radix(parts.get(1)?.get(..4)?, 16).ok()?;
    let pid_text = parts
        .get(3)
        .and_then(|p| p.get(..4))
        .unwrap_or("0000")
        .to_uppercase();
    let pid = u16::from_str_radix(&pid_text, 16).unwrap_or(0);
    let d = db();
    let vendor = *d.vendors.get(&vid)?;
    let product = d
        .products
        .get(&(vid, pid))
        .map_or_else(|| format!("Product: {pid_text}"), |p| (*p).to_owned());
    Some(UsbInfo {
        vendor: vendor.to_owned(),
        product,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        let i = lookup("VID_0781&PID_5567").unwrap();
        assert_eq!(i.vendor, "SanDisk Corp.");
        assert_eq!(i.product, "Cruzer Blade");
        let i = lookup("Vid_0781&Pid_FFFE&MI_00").unwrap();
        assert_eq!(i.product, "Product: FFFE");
        assert_eq!(lookup("VID_0781").unwrap().product, "Product: 0000");
        assert!(lookup("VID_FFFF&PID_0001").is_none());
        assert!(lookup("ROOT_HUB").is_none());
        assert!(lookup("").is_none());
    }
}
