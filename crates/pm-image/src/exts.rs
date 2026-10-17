//! File-name extensions of disk images, as used by Autopsy's
//! `GeneralFilter` (Add Data Source file chooser, VM Extractor).

/// `GeneralFilter.RAW_IMAGE_EXTS`.
pub const RAW_IMAGE_EXTS: &[&str] = &[".img", ".dd", ".001", ".aa", ".raw", ".bin"];
/// `GeneralFilter.ENCASE_IMAGE_EXTS`.
pub const ENCASE_IMAGE_EXTS: &[&str] = &[".e01"];
/// `GeneralFilter.VIRTUAL_MACHINE_EXTS`: what the VM Extractor looks for.
pub const VIRTUAL_MACHINE_EXTS: &[&str] = &[".vmdk", ".vhd", ".vhdx"];
/// Every extension [`crate::open`] accepts: Autopsy's lists plus QCOW2,
/// which TSK cannot open but postmortem can.
pub const SUPPORTED_IMAGE_EXTS: &[&str] = &[
    ".img", ".dd", ".001", ".aa", ".raw", ".bin", ".e01", ".vmdk", ".vhd", ".vhdx", ".qcow2",
    ".qcow",
];

fn has_ext(name: &str, exts: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    exts.iter().any(|e| lower.ends_with(e))
}

/// True if `name` has one of [`VIRTUAL_MACHINE_EXTS`] (case-insensitive),
/// the VM Extractor's selection rule.
pub fn is_virtual_machine_file(name: &str) -> bool {
    has_ext(name, VIRTUAL_MACHINE_EXTS)
}

/// True if `name` has one of [`SUPPORTED_IMAGE_EXTS`] (case-insensitive).
pub fn is_supported_image_file(name: &str) -> bool {
    has_ext(name, SUPPORTED_IMAGE_EXTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autopsy_lists_are_covered() {
        for e in RAW_IMAGE_EXTS
            .iter()
            .chain(ENCASE_IMAGE_EXTS)
            .chain(VIRTUAL_MACHINE_EXTS)
        {
            assert!(SUPPORTED_IMAGE_EXTS.contains(e), "{e}");
        }
    }

    #[test]
    fn vm_files_match_case_insensitively() {
        assert!(is_virtual_machine_file("Windows 10.VMDK"));
        assert!(is_virtual_machine_file("disk.vhdx"));
        assert!(!is_virtual_machine_file("disk.qcow2"));
        assert!(!is_virtual_machine_file("vmdk"));
        assert!(is_supported_image_file("disk.qcow2"));
        assert!(!is_supported_image_file("notes.txt"));
    }
}
