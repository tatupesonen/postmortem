#![allow(clippy::unwrap_used, reason = "dev tool")]
use pm_testkit::ext::{ExtKind, Op, build};
fn main() {
    let out = std::env::args().nth(1).unwrap();
    let pattern: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    for (name, kind) in [
        ("ext2", ExtKind::Ext2),
        ("ext3", ExtKind::Ext3),
        ("ext4", ExtKind::Ext4),
    ] {
        let img = build(
            kind,
            1024,
            8192,
            &[
                Op::Dir("/home"),
                Op::Dir("/home/alice"),
                Op::File("/home/alice/notes.txt", b"hello ext\n".to_vec()),
                Op::File("/home/alice/big.bin", pattern.clone()),
                Op::Symlink("/home/alice/link", "notes.txt"),
                Op::Sparse("/home/alice/sparse.img", 5_000_000),
                Op::File("/home/alice/gone.txt", b"deleted ext file\n".to_vec()),
                Op::Remove("/home/alice/gone.txt"),
            ],
        );
        std::fs::write(format!("{out}/{name}.img"), img).unwrap();
    }
}
