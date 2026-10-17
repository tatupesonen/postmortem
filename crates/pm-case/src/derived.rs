//! Storage of derived files in the case folder.
//!
//! Modules write extracted bytes under
//! `<case>/ModuleOutput/<module folder>/<source file id>/…` and record the
//! path relative to the case folder in `files.local_path`. Like Autopsy,
//! the bytes are normally XOR1-encoded (Autopsy `EncodedFileOutputStream`): a
//! 32-byte plain-text header followed by every byte XOR 0xCA. This keeps
//! antivirus software from quarantining (deleting) malware found in the
//! evidence (ADR-016). The encoding is recognised from the header when the file is
//! opened (Autopsy `EncodedFileUtil.getEncoding`), so the case database needs
//! no extra column; plain files are read as well.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use pm_core::ReadAt;

/// How the bytes of a derived file are stored (Autopsy `EncodingType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Encoding {
    /// Plain bytes.
    None,
    /// Autopsy's XOR1: a 32-byte plain-text header, then every byte XOR 0xCA.
    #[default]
    Xor1,
}

/// The XOR1 header written before the encoded bytes (32 bytes, unencoded).
pub const XOR1_HEADER: &[u8; 32] = b"KAAMOS_CONTAINER_XOR1_xxxxxxxxxx";
/// The XOR1 key.
pub const XOR1_KEY: u8 = 0xCA;

/// Detects the encoding of a stored file from its first bytes (Autopsy
/// `EncodedFileUtil.getEncoding`).
pub fn sniff_encoding(head: &[u8]) -> Encoding {
    if head.starts_with(XOR1_HEADER) {
        Encoding::Xor1
    } else {
        Encoding::None
    }
}

/// Decoded view of a stored derived file.
#[derive(Debug)]
pub struct EncodedSource<S> {
    inner: S,
    encoding: Encoding,
}

impl<S: ReadAt> EncodedSource<S> {
    pub fn new(inner: S, encoding: Encoding) -> Self {
        Self { inner, encoding }
    }

    fn header_len(&self) -> u64 {
        match self.encoding {
            Encoding::None => 0,
            Encoding::Xor1 => XOR1_HEADER.len() as u64,
        }
    }
}

impl<S: ReadAt> ReadAt for EncodedSource<S> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let Some(pos) = offset.checked_add(self.header_len()) else {
            return Ok(0);
        };
        let n = self.inner.read_at(pos, buf)?;
        if self.encoding == Encoding::Xor1 {
            for b in buf.iter_mut().take(n) {
                *b ^= XOR1_KEY;
            }
        }
        Ok(n)
    }

    fn size(&self) -> u64 {
        self.inner.size().saturating_sub(self.header_len())
    }
}

/// Writes a derived file in the given encoding (buffered).
#[derive(Debug)]
pub struct EncodedWriter {
    out: BufWriter<File>,
    encoding: Encoding,
    scratch: Vec<u8>,
    written: u64,
}

impl EncodedWriter {
    /// Creates (truncates) `path`, creating missing parent folders.
    pub fn create(path: &Path, encoding: Encoding) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = BufWriter::with_capacity(256 * 1024, File::create(path)?);
        if encoding == Encoding::Xor1 {
            out.write_all(XOR1_HEADER)?;
        }
        Ok(Self {
            out,
            encoding,
            scratch: Vec::new(),
            written: 0,
        })
    }

    /// Content bytes written so far (without the header).
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Flushes and closes the file; returns the content size.
    pub fn finish(mut self) -> io::Result<u64> {
        self.out.flush()?;
        self.out.get_ref().sync_data().ok();
        Ok(self.written)
    }
}

impl Write for EncodedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = match self.encoding {
            Encoding::None => self.out.write(buf)?,
            Encoding::Xor1 => {
                self.scratch.clear();
                self.scratch.extend(buf.iter().map(|b| b ^ XOR1_KEY));
                self.out.write_all(&self.scratch)?;
                buf.len()
            }
        };
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Location of a module's derived files for one source file:
/// `ModuleOutput/<module_folder>/<source_id>` (relative to the case
/// folder; `/`-separated so case folders move between OSes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDir {
    case_dir: PathBuf,
    relative: String,
}

impl OutputDir {
    pub fn new(case_dir: &Path, module_folder: &str, source_id: i64) -> Self {
        Self {
            case_dir: case_dir.to_path_buf(),
            relative: format!("ModuleOutput/{module_folder}/{source_id}"),
        }
    }

    /// Relative path (for `files.local_path`) of `name` inside the folder;
    /// `name` may contain `/`.
    pub fn relative(&self, name: &str) -> String {
        format!("{}/{name}", self.relative)
    }

    /// Absolute path of a relative path returned by [`Self::relative`].
    pub fn absolute(&self, relative: &str) -> PathBuf {
        resolve(&self.case_dir, relative)
    }

    /// Absolute path of the folder itself.
    pub fn path(&self) -> PathBuf {
        resolve(&self.case_dir, &self.relative)
    }

    pub fn exists(&self) -> bool {
        self.path().is_dir()
    }
}

/// Joins a stored `/`-separated relative path to the case folder. Stored
/// paths written by Autopsy on Windows use `\`; both are accepted, and
/// `..` / absolute components are ignored so a crafted case database
/// cannot point outside the case folder.
pub fn resolve(case_dir: &Path, relative: &str) -> PathBuf {
    let mut p = case_dir.to_path_buf();
    for part in relative.split(['/', '\\']) {
        if part.is_empty() || part == "." || part == ".." || part.contains(':') {
            continue;
        }
        p.push(part);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    #[test]
    fn xor1_round_trip_matches_the_container_layout() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b/f.bin");
        let mut w = EncodedWriter::create(&p, Encoding::Xor1).unwrap();
        w.write_all(b"hello").unwrap();
        w.write_all(&[0xCA, 0x00]).unwrap();
        assert_eq!(w.finish().unwrap(), 7);
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(&raw[..32], XOR1_HEADER);
        assert_eq!(raw[32], b'h' ^ 0xCA);
        assert_eq!(raw[37], 0x00);
        assert_eq!(raw[38], 0xCA);
        assert_eq!(sniff_encoding(&raw), Encoding::Xor1);
        let src = EncodedSource::new(MemSource::new(raw), Encoding::Xor1);
        assert_eq!(src.size(), 7);
        assert_eq!(src.read_vec_at(0, 7).unwrap(), b"hello\xCA\x00");
        assert_eq!(src.read_vec_at(5, 2).unwrap(), b"\xCA\x00");
        assert_eq!(src.read_at(u64::MAX, &mut [0; 4]).unwrap(), 0);
    }

    #[test]
    fn plain_files_pass_through() {
        let src = EncodedSource::new(MemSource::new(b"abc".to_vec()), Encoding::None);
        assert_eq!(src.size(), 3);
        assert_eq!(sniff_encoding(b"abc"), Encoding::None);
        assert_eq!(src.read_vec_at(1, 2).unwrap(), b"bc");
    }

    #[test]
    fn resolve_stays_inside_the_case() {
        let base = Path::new("/case");
        assert_eq!(
            resolve(base, "ModuleOutput/EFE/7/0/1"),
            Path::new("/case/ModuleOutput/EFE/7/0/1")
        );
        assert_eq!(
            resolve(base, "ModuleOutput\\EFE\\7"),
            Path::new("/case/ModuleOutput/EFE/7")
        );
        assert_eq!(
            resolve(base, "../../etc/passwd"),
            Path::new("/case/etc/passwd")
        );
        assert_eq!(resolve(base, "/abs/x"), Path::new("/case/abs/x"));
        assert_eq!(resolve(base, "C:/x"), Path::new("/case/x"));
        let out = OutputDir::new(base, "EFE", 42);
        assert_eq!(out.relative("0/3"), "ModuleOutput/EFE/42/0/3");
        assert_eq!(
            out.absolute(&out.relative("0/3")),
            Path::new("/case/ModuleOutput/EFE/42/0/3")
        );
    }
}
