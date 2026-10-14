//! Partition type identifiers and their names.

use uuid::{Uuid, uuid};

/// Whether an MBR type marks an extended (EBR chain) container.
pub(crate) fn is_extended(t: u8) -> bool {
    matches!(t, 0x05 | 0x0F | 0x85)
}

/// Name of a DOS partition type (as shown by TSK/Autopsy).
pub fn mbr_type_name(t: u8) -> &'static str {
    match t {
        0x00 => "Empty",
        0x01 => "DOS FAT12",
        0x02 => "XENIX root",
        0x03 => "XENIX /usr",
        0x04 | 0x06 => "DOS FAT16",
        0x05 => "DOS Extended",
        0x07 => "NTFS / exFAT",
        0x08 => "AIX Boot",
        0x09 => "AIX Data",
        0x0A => "OS/2 Boot Manager",
        0x0B => "Win95 FAT32",
        0x0C => "Win95 FAT32 (0x0C)",
        0x0E => "DOS FAT16 (LBA)",
        0x0F => "Win95 Extended",
        0x11 => "DOS FAT12 (hidden)",
        0x12 => "Hibernation / OEM diagnostics",
        0x14 | 0x16 => "DOS FAT16 (hidden)",
        0x17 => "Hidden IFS/HPFS/NTFS",
        0x1B => "Win95 FAT32 (hidden)",
        0x1C => "Win95 FAT32 LBA (hidden)",
        0x1E => "DOS FAT16 LBA (hidden)",
        0x27 => "Windows Recovery Environment",
        0x39 => "Plan 9",
        0x3C => "Partition Magic Recovery",
        0x42 => "Windows Dynamic (LDM)",
        0x4D..=0x4F => "QNX",
        0x63 => "Unix System V",
        0x64 => "Novell NetWare 286",
        0x65 => "Novell NetWare 386",
        0x80 => "Minix (old)",
        0x81 => "Minix",
        0x82 => "Linux Swap / Solaris x86",
        0x83 => "Linux",
        0x84 => "Hibernation",
        0x85 => "Linux Extended",
        0x86 | 0x87 => "NTFS Volume Set",
        0x8E => "Linux LVM",
        0x93 => "Amoeba",
        0x9F => "BSD/OS",
        0xA0 => "Laptop Hibernation",
        0xA5 => "FreeBSD",
        0xA6 => "OpenBSD",
        0xA8 => "Mac OS X UFS",
        0xA9 => "NetBSD",
        0xAB => "Mac OS X Boot",
        0xAF => "Mac OS X HFS",
        0xB7 => "BSDI",
        0xB8 => "BSDI Swap",
        0xBE => "Solaris 8 Boot",
        0xBF => "Solaris x86",
        0xC1 | 0xC4 | 0xC6 => "DR-DOS secured FAT",
        0xDE => "Dell Utility",
        0xEB => "BeOS",
        0xEE => "GPT Safety Partition",
        0xEF => "EFI File System",
        0xFB => "VMware File System",
        0xFC => "VMware Swap",
        0xFD => "Linux RAID",
        0xFE => "LANstep",
        0xFF => "Xenix Bad Block Table",
        _ => "Unknown Type",
    }
}

const GPT_TYPES: &[(Uuid, &str)] = &[
    (
        uuid!("C12A7328-F81F-11D2-BA4B-00A0C93EC93B"),
        "EFI System Partition",
    ),
    (
        uuid!("21686148-6449-6E6F-744E-656564454649"),
        "BIOS Boot Partition",
    ),
    (
        uuid!("024DEE41-33E7-11D3-9D69-0008C781F39F"),
        "MBR Partition Scheme",
    ),
    (
        uuid!("E3C9E316-0B5C-4DB8-817D-F92DF00215AE"),
        "Microsoft Reserved Partition",
    ),
    (
        uuid!("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"),
        "Basic Data Partition",
    ),
    (
        uuid!("5808C8AA-7E8F-42E0-85D2-E1E90434CFB3"),
        "Logical Disk Manager Metadata",
    ),
    (
        uuid!("AF9B60A0-1431-4F62-BC68-3311714A69AD"),
        "Logical Disk Manager Data",
    ),
    (
        uuid!("DE94BBA4-06D1-4D40-A16A-BFD50179D6AC"),
        "Windows Recovery Environment",
    ),
    (
        uuid!("E75CAF8F-F680-4CEE-AFA3-B001E56EFC2D"),
        "Storage Spaces",
    ),
    (
        uuid!("0FC63DAF-8483-4772-8E79-3D69D8477DE4"),
        "Linux Filesystem Data",
    ),
    (
        uuid!("4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709"),
        "Linux Root (x86-64)",
    ),
    (
        uuid!("B921B045-1DF0-41C3-AF44-4C6F280D3FAE"),
        "Linux Root (ARM64)",
    ),
    (uuid!("933AC7E1-2EB4-4F13-B844-0E14E2AEF915"), "Linux /home"),
    (uuid!("0657FD6D-A4AB-43C4-84E5-0933C84B4F4F"), "Linux Swap"),
    (uuid!("E6D6D379-F507-44C2-A23C-238F2A3DF928"), "Linux LVM"),
    (uuid!("A19D880F-05FC-4D3B-A006-743F0F84911E"), "Linux RAID"),
    (uuid!("CA7D7CCB-63ED-4C53-861C-1742536059CC"), "Linux LUKS"),
    (
        uuid!("BC13C2FF-59E6-4262-A352-B275FD6F7172"),
        "Linux Extended Boot",
    ),
    (uuid!("48465300-0000-11AA-AA11-00306543ECAC"), "Apple HFS+"),
    (uuid!("7C3457EF-0000-11AA-AA11-00306543ECAC"), "Apple APFS"),
    (uuid!("55465300-0000-11AA-AA11-00306543ECAC"), "Apple UFS"),
    (uuid!("426F6F74-0000-11AA-AA11-00306543ECAC"), "Apple Boot"),
    (uuid!("52414944-0000-11AA-AA11-00306543ECAC"), "Apple RAID"),
    (uuid!("4C616265-6C00-11AA-AA11-00306543ECAC"), "Apple Label"),
    (
        uuid!("53746F72-6167-11AA-AA11-00306543ECAC"),
        "Apple Core Storage",
    ),
    (
        uuid!("516E7CB4-6ECF-11D6-8FF8-00022D09712B"),
        "FreeBSD Data",
    ),
    (uuid!("516E7CB6-6ECF-11D6-8FF8-00022D09712B"), "FreeBSD UFS"),
    (uuid!("516E7CBA-6ECF-11D6-8FF8-00022D09712B"), "FreeBSD ZFS"),
    (
        uuid!("6A898CC3-1DD2-11B2-99A6-080020736631"),
        "Solaris /usr or Apple ZFS",
    ),
    (
        uuid!("FE3A2A5D-4F32-41A7-B725-ACCC3285A309"),
        "ChromeOS Kernel",
    ),
    (
        uuid!("3CB8E202-3B7E-47DD-8A3C-7FF2A13CFCEC"),
        "ChromeOS Root",
    ),
    (uuid!("AA31E02A-400F-11DB-9590-000C2911D1B8"), "VMware VMFS"),
];

/// Name of a well-known GPT partition type GUID.
pub fn gpt_type_name(guid: &Uuid) -> Option<&'static str> {
    GPT_TYPES.iter().find(|(g, _)| g == guid).map(|(_, n)| *n)
}
