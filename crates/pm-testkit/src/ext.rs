//! ext2/3/4 images built with the `fstool` writer (validated against TSK).

use std::path::Path;

use fstool::block::MemoryBackend;
use fstool::fs::Filesystem;
use fstool::fs::ext::{Ext, FormatOpts, FsKind};
use fstool::fs::{FileMeta, FileSource};

pub use fstool::fs::ext::FsKind as ExtKind;

/// Fixed timestamp for all test files: 2021-03-04 10:20:30 UTC.
pub const MTIME: u32 = 1_614_853_230;

/// A file-system operation for [`build`].
#[derive(Debug, Clone)]
pub enum Op {
    Dir(&'static str),
    File(&'static str, Vec<u8>),
    Symlink(&'static str, &'static str),
    /// Sparse file of the given length (all holes).
    Sparse(&'static str, u64),
    Remove(&'static str),
}

fn meta() -> FileMeta {
    FileMeta {
        mode: 0o644,
        uid: 1000,
        gid: 1000,
        mtime: MTIME,
        atime: MTIME,
        ctime: MTIME,
    }
}

/// Formats an ext volume of `blocks` × `block_size` and applies `ops`.
pub fn build(kind: FsKind, block_size: u32, blocks: u32, ops: &[Op]) -> Vec<u8> {
    let opts = FormatOpts {
        kind,
        block_size,
        blocks_count: blocks,
        inodes_count: 256,
        volume_label: *b"PMEXT\0\0\0\0\0\0\0\0\0\0\0",
        uuid: [0x5E; 16],
        mtime: MTIME,
        sparse: true,
        ..FormatOpts::default()
    };
    let mut dev = MemoryBackend::new(u64::from(blocks) * u64::from(block_size));
    let mut fs = Ext::format_with(&mut dev, &opts).unwrap();
    for op in ops {
        match op {
            Op::Dir(p) => fs
                .create_dir(
                    &mut dev,
                    Path::new(p),
                    FileMeta {
                        mode: 0o755,
                        ..meta()
                    },
                )
                .unwrap(),
            Op::File(p, data) => fs
                .create_file(
                    &mut dev,
                    Path::new(p),
                    FileSource::Reader {
                        reader: Box::new(std::io::Cursor::new(data.clone())),
                        len: data.len() as u64,
                    },
                    meta(),
                )
                .unwrap(),
            Op::Symlink(p, target) => fs
                .create_symlink(&mut dev, Path::new(p), Path::new(target), meta())
                .unwrap(),
            Op::Sparse(p, len) => fs
                .create_file(&mut dev, Path::new(p), FileSource::Zero(*len), meta())
                .unwrap(),
            Op::Remove(p) => fs.remove(&mut dev, Path::new(p)).unwrap(),
        }
    }
    fs.flush(&mut dev).unwrap();
    dev.as_slice().to_vec()
}
