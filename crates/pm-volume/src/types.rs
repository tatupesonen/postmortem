//! Partition type identifiers and their names.

use uuid::{Uuid, uuid};

/// Whether an MBR type marks an extended (EBR chain) container.
pub(crate) fn is_extended(t: u8) -> bool {
    matches!(t, 0x05 | 0x0F | 0x85)
}

/// Name of a DOS partition type, exactly as TSK's `mmls` prints it (and
/// therefore as Autopsy shows it). Generated from TSK 4.15 by probing every
/// type byte; `tests/tsk_differential.rs` keeps it honest.
pub fn mbr_type_name(t: u8) -> &'static str {
    #[expect(
        clippy::indexing_slicing,
        reason = "a u8 always indexes a 256-entry table"
    )]
    MBR_NAMES[usize::from(t)]
}

#[rustfmt::skip]
static MBR_NAMES: [&str; 256] = [
    "Empty", // 0x00
    "DOS FAT12", // 0x01
    "XENIX root", // 0x02
    "XENIX /usr", // 0x03
    "DOS FAT16", // 0x04
    "DOS Extended", // 0x05
    "DOS FAT16", // 0x06
    "NTFS / exFAT", // 0x07
    "AIX Boot", // 0x08
    "AIX Data", // 0x09
    "OS/2 Boot Manager", // 0x0A
    "Win95 FAT32", // 0x0B
    "Win95 FAT32", // 0x0C
    "Unknown Type", // 0x0D
    "Win95 FAT16", // 0x0E
    "Win95 Extended", // 0x0F
    "OPUS", // 0x10
    "DOS FAT12 Hidden", // 0x11
    "Hibernation", // 0x12
    "Unknown Type", // 0x13
    "DOS FAT16 Hidden", // 0x14
    "Unknown Type", // 0x15
    "DOS FAT16 Hidden", // 0x16
    "Hidden IFS/HPFS", // 0x17
    "AST SmartSleep", // 0x18
    "Win95 FAT32 Hidden", // 0x19
    "Unknown Type", // 0x1A
    "Win95 FAT32 Hidden", // 0x1B
    "Win95 FAT32 Hidden", // 0x1C
    "Unknown Type", // 0x1D
    "Win95 FAT16 Hidden", // 0x1E
    "Unknown Type", // 0x1F
    "Unused", // 0x20
    "Reserved", // 0x21
    "Unused", // 0x22
    "Reserved", // 0x23
    "NEC DOS 3.x", // 0x24
    "Unknown Type", // 0x25
    "Reserved", // 0x26
    "Unknown Type", // 0x27
    "Unknown Type", // 0x28
    "Unknown Type", // 0x29
    "Unknown Type", // 0x2A
    "Unknown Type", // 0x2B
    "Unknown Type", // 0x2C
    "Unknown Type", // 0x2D
    "Unknown Type", // 0x2E
    "Unknown Type", // 0x2F
    "Unknown Type", // 0x30
    "Reserved", // 0x31
    "NOS", // 0x32
    "Reserved", // 0x33
    "Reserved", // 0x34
    "JFS on OS/2 or eCS ", // 0x35
    "Reserved", // 0x36
    "Unknown Type", // 0x37
    "THEOS v3.2 2gb", // 0x38
    "THEOS v4 Spanned", // 0x39
    "THEOS v4 4gb", // 0x3A
    "THEOS v4 Extended", // 0x3B
    "PartitionMagic Recovery", // 0x3C
    "Hidden NetWare", // 0x3D
    "Unknown Type", // 0x3E
    "Unknown Type", // 0x3F
    "Venix 80286", // 0x40
    "Linux/MINIX (Sharing Disk with DR-DOS)", // 0x41
    "Win LVM / Secure FS", // 0x42
    "Linux Native (Sharing Disk with DR-DOS)", // 0x43
    "GoBack", // 0x44
    "Boot-US Boot Manager", // 0x45
    "EUMEL/Elan ", // 0x46
    "EUMEL/Elan ", // 0x47
    "EUMEL/Elan ", // 0x48
    "Unknown Type", // 0x49
    "Mark Aitchison's ALFS/THIN Lightweight Filesystem", // 0x4A
    "Unknown Type", // 0x4B
    "Oberon", // 0x4C
    "QNX 4.x", // 0x4D
    "QNX 4.x", // 0x4E
    "QNX 4.x", // 0x4F
    "OnTrack Disk Manager", // 0x50
    "OnTrack Disk Manager", // 0x51
    "Microport SysV/AT", // 0x52
    "OnTrack Disk Manager", // 0x53
    "OnTrack Disk Manager", // 0x54
    "EZ-Drive", // 0x55
    "AT&T MS-DOS 3.x Logically Sectored FAT", // 0x56
    "DrivePro", // 0x57
    "Unknown Type", // 0x58
    "Unknown Type", // 0x59
    "Unknown Type", // 0x5A
    "Unknown Type", // 0x5B
    "Priam EDisk", // 0x5C
    "Unknown Type", // 0x5D
    "Unknown Type", // 0x5E
    "Unknown Type", // 0x5F
    "Unknown Type", // 0x60
    "SpeedStor", // 0x61
    "Unknown Type", // 0x62
    "UNIX System V", // 0x63
    "Novell Netware", // 0x64
    "Novell Netware", // 0x65
    "Novell Netware", // 0x66
    "Novell Netware", // 0x67
    "Novell Netware", // 0x68
    "Novell Netware", // 0x69
    "Unknown Type", // 0x6A
    "Unknown Type", // 0x6B
    "Unknown Type", // 0x6C
    "Unknown Type", // 0x6D
    "Unknown Type", // 0x6E
    "Unknown Type", // 0x6F
    "DiskSecure Multi-Boot", // 0x70
    "Reserved", // 0x71
    "Unknown Type", // 0x72
    "Reserved", // 0x73
    "Scramdisk", // 0x74
    "IBM PC/IX", // 0x75
    "Reserved", // 0x76
    "VNDI", // 0x77
    "XOSL FS", // 0x78
    "Unknown Type", // 0x79
    "Unknown Type", // 0x7A
    "Unknown Type", // 0x7B
    "Unknown Type", // 0x7C
    "Unknown Type", // 0x7D
    "Unused", // 0x7E
    "Unused", // 0x7F
    "MINIX <=v1.4a", // 0x80
    "MINIX >=v1.4b, Early Linux", // 0x81
    "Linux Swap / Solaris x86", // 0x82
    "Linux", // 0x83
    "Hibernation", // 0x84
    "Linux Extended", // 0x85
    "NTFS Volume Set", // 0x86
    "NTFS Volume Set", // 0x87
    "Unknown Type", // 0x88
    "Unknown Type", // 0x89
    "Linux Kernel", // 0x8A
    "Legacy Fault Tolerant FAT32", // 0x8B
    "Legacy Fault Tolerant FAT32 using BIOS extd INT 13h", // 0x8C
    "Free FDISK Hidden Primary DOS FAT12", // 0x8D
    "Linux Logical Volume Manager", // 0x8E
    "Unknown Type", // 0x8F
    "Free FDISK Hidden Primary DOS FAT16", // 0x90
    "Free FDISK Hidden DOS Extended", // 0x91
    "Free FDISK Hidden Primary DOS Large FAT16", // 0x92
    "Linux Hidden", // 0x93
    "Amoeba Bad Block Table", // 0x94
    "MIT EXOPC", // 0x95
    "Unknown Type", // 0x96
    "Free FDISK Hidden Primary DOS FAT32", // 0x97
    "Free FDISK Hidden Primary DOS FAT32 LBA", // 0x98
    "DCE376 Logical Drive", // 0x99
    "Free FDISK Hidden Primary DOS FAT16 LBA", // 0x9A
    "Free FDISK Hidden DOS Extended LBA", // 0x9B
    "Unknown Type", // 0x9C
    "Unknown Type", // 0x9D
    "Unknown Type", // 0x9E
    "BSD/OS", // 0x9F
    "Hibernation", // 0xA0
    "Hibernation", // 0xA1
    "Unknown Type", // 0xA2
    "HP Volume Expansion (SpeedStor Variant)", // 0xA3
    "HP Volume Expansion (SpeedStor Variant)", // 0xA4
    "BSD/386, 386BSD, NetBSD, FreeBSD", // 0xA5
    "OpenBSD", // 0xA6
    "NeXTSTEP", // 0xA7
    "Mac OS X", // 0xA8
    "NetBSD", // 0xA9
    "Olivetti Fat 12 1.44MB Service", // 0xAA
    "Mac OS X Boot Partition", // 0xAB
    "Unknown Type", // 0xAC
    "Unknown Type", // 0xAD
    "ShagOS Filesystem", // 0xAE
    "Mac OS X HFS", // 0xAF
    "BootStar Dummy", // 0xB0
    "HP Volume Expansion (SpeedStor Variant)", // 0xB1
    "Unknown Type", // 0xB2
    "HP Volume Expansion (SpeedStor Variant)", // 0xB3
    "HP Volume Expansion (SpeedStor Variant)", // 0xB4
    "Unknown Type", // 0xB5
    "Corrupted Windows NT Mirror Set Master FAT16", // 0xB6
    "BSDI", // 0xB7
    "BSDI Swap", // 0xB8
    "Unknown Type", // 0xB9
    "Unknown Type", // 0xBA
    "Boot Wizard Hidden", // 0xBB
    "Unknown Type", // 0xBC
    "Unknown Type", // 0xBD
    "Solaris 8 Boot", // 0xBE
    "Unknown Type", // 0xBF
    "DR-DOS Secured", // 0xC0
    "DR-DOS Secured FAT12", // 0xC1
    "Hidden Linux", // 0xC2
    "Hidden Linux Swap", // 0xC3
    "DR-DOS Secured FAT16 <32M", // 0xC4
    "DR-DOS Secured Extended", // 0xC5
    "Corrupted Windows NT Volume / Stripe Set", // 0xC6
    "Corrupted Windows NT Volume / Stripe Set", // 0xC7
    "Reserved for DR-DOS 8.0+", // 0xC8
    "Reserved for DR-DOS 8.0+", // 0xC9
    "Reserved for DR-DOS 8.0+", // 0xCA
    "DR-DOS 7.04+ Secured FAT32 CHS", // 0xCB
    "DR-DOS 7.04+ Secured FAT32 LBA", // 0xCC
    "CTOS Memdump?", // 0xCD
    "DR-DOS 7.04+ FAT16X LBA", // 0xCE
    "DR-DOS 7.04+ Secured EXT DOS LBA", // 0xCF
    "Multiuser DOS Secured", // 0xD0
    "Old Multiuser DOS Secured FAT12", // 0xD1
    "Unknown Type", // 0xD2
    "Unknown Type", // 0xD3
    "Old Multiuser DOS Secured FAT16 <32M", // 0xD4
    "Old Multiuser DOS Secured extended", // 0xD5
    "Old Multiuser DOS Secured FAT16 >=32M", // 0xD6
    "Unknown Type", // 0xD7
    "CP/M-86", // 0xD8
    "Unknown Type", // 0xD9
    "Non-FS Data", // 0xDA
    "Digital Research CP/M, Concurrent CP/M, Concurrent DOS", // 0xDB
    "Unknown Type", // 0xDC
    "Hidden CTOS Memdump? ", // 0xDD
    "Dell Utilities FAT", // 0xDE
    "Unknown Type", // 0xDF
    "Reserved by STMicroelectronics for ST AVFS.", // 0xE0
    "DOS Access or SpeedStor 12-bit FAT Extended", // 0xE1
    "Unknown Type", // 0xE2
    "DOS R/O or SpeedStor", // 0xE3
    "SpeedStor 16-bit FAT Extended <1024 cyl.", // 0xE4
    "Tandy MS-DOS with Logically Sectored FAT", // 0xE5
    "Storage Dimensions SpeedStor", // 0xE6
    "Unknown Type", // 0xE7
    "Unknown Type", // 0xE8
    "Unknown Type", // 0xE9
    "Unknown Type", // 0xEA
    "BeOS BFS", // 0xEB
    "Unknown Type", // 0xEC
    "Unused", // 0xED
    "GPT Safety Partition", // 0xEE
    "EFI File System", // 0xEF
    "Linux/PA-RISC Boot Loader", // 0xF0
    "Storage Dimensions SpeedStor", // 0xF1
    "DOS 3.3+ Secondary", // 0xF2
    "Reserved", // 0xF3
    "SpeedStor Large", // 0xF4
    "Prologue Multi-Volume", // 0xF5
    "Storage Dimensions SpeedStor", // 0xF6
    "Unused", // 0xF7
    "Unknown Type", // 0xF8
    "pCache", // 0xF9
    "Bochs", // 0xFA
    "VMWare File System", // 0xFB
    "VMWare Swap", // 0xFC
    "Linux RAID", // 0xFD
    "Windows NT Disk Administrator Hidden", // 0xFE
    "Xenix Bad Block Table", // 0xFF
];

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
