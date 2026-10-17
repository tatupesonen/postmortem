//! Every supported format carved out of generated "unallocated space":
//! files at sector boundaries between random-looking and zeroed sectors.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use pm_carve::{Carved, Filter, Options, carve};
use pm_core::io::MemSource;
use pm_testkit::archives as ak;
use pm_testkit::media;

/// Pseudo-random filler that contains no signatures at sector starts.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            // Keep sector starts away from 0xFF / signature bytes.
            if i % 512 == 0 { 0x20 } else { x as u8 }
        })
        .collect()
}

fn sqlite_db() -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.db");
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch(
        "PRAGMA journal_mode=DELETE; CREATE TABLE t(a TEXT); INSERT INTO t VALUES ('hello'), ('world');",
    )
    .unwrap();
    drop(c);
    std::fs::read(&p).unwrap()
}

/// `(name, expected extension, bytes)` of every fixture.
fn fixtures() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let png = ak::png(16, 16);
    let jpg = ak::jpeg(32, 32);
    vec![
        ("jpeg", "jpg", jpg.clone()),
        ("png", "png", png.clone()),
        ("gif", "gif", ak::gif(8, 8)),
        ("bmp", "bmp", pm_testkit::demo::gradient_bmp(20, 10)),
        ("tiff", "tif", media::tiff(12, 9)),
        (
            "pdf",
            "pdf",
            ak::pdf(
                &[("a.txt", b"attached")],
                &[ak::PdfImage::Jpeg(jpg.clone())],
            ),
        ),
        (
            "zip",
            "zip",
            ak::zip(&[("a.txt", b"hello"), ("b.txt", b"world")]),
        ),
        (
            "docx",
            "docx",
            ak::ooxml("word", &[("image1.png", &png)], &[]),
        ),
        (
            "xlsx",
            "xlsx",
            ak::ooxml("xl", &[("image1.png", &png)], &[]),
        ),
        ("doc", "doc", ak::doc(&[ak::blip(ak::BLIP_PNG, &png, 1)])),
        ("xls", "xls", ak::xls(&[ak::blip(ak::BLIP_PNG, &png, 1)])),
        ("ppt", "ppt", ak::ppt(&[ak::blip(ak::BLIP_PNG, &png, 1)])),
        (
            "7z",
            "7z",
            ak::sevenz(&[("a.txt", b"hello hello hello")], &[], false, None, false),
        ),
        (
            "gzip",
            "gz",
            ak::gzip(Some("a.txt"), &b"gzip data ".repeat(300)),
        ),
        ("mp3 tagged", "mp3", media::mp3(12, true)),
        ("mp3 bare", "mp3", media::mp3(20, false)),
        ("wav", "wav", media::wav(3000)),
        ("avi", "avi", media::avi(5000)),
        ("mp4", "mp4", media::mp4(b"isom", 7000)),
        ("mov", "mov", media::mp4(b"qt  ", 3000)),
        ("m4a", "m4a", media::mp4(b"M4A ", 2000)),
        ("sqlite", "sqlite", sqlite_db()),
        ("exe", "exe", media::pe(false, 3000)),
        ("dll", "dll", media::pe(true, 1000)),
        ("evtx", "evtx", media::evtx(2)),
    ]
}

/// Lays the fixtures out at sector boundaries with filler between them;
/// returns the image and `(offset, fixture index)`.
fn layout(fx: &[(&str, &str, Vec<u8>)]) -> (Vec<u8>, Vec<(u64, usize)>) {
    let mut img = noise(4096, 1);
    let mut at = Vec::new();
    for (i, (_, _, data)) in fx.iter().enumerate() {
        at.push((img.len() as u64, i));
        img.extend(data);
        // Pad to the next sector, then some filler.
        let pad = (512 - img.len() % 512) % 512;
        img.extend(std::iter::repeat_n(0u8, pad));
        if i % 2 == 0 {
            img.extend(noise(1536, i as u64 + 7));
        } else {
            img.extend(vec![0u8; 1024]);
        }
    }
    (img, at)
}

fn run(img: &[u8], opts: &Options) -> Vec<Carved> {
    let src = MemSource::new(img.to_vec());
    let mut out = Vec::new();
    let stats = carve(&src, opts, &|| false, &mut |c| out.push(c)).unwrap();
    assert_eq!(stats.files, out.len() as u64);
    out
}

#[test]
fn every_format_is_carved_with_its_exact_length() {
    let fx = fixtures();
    let (img, at) = layout(&fx);
    let found = run(&img, &Options::default());
    for (off, i) in &at {
        let (name, ext, data) = &fx[*i];
        let c = found
            .iter()
            .find(|c| c.offset == *off)
            .unwrap_or_else(|| panic!("{name} at {off} not carved; found {found:?}"));
        assert_eq!(c.ext, *ext, "{name}");
        assert_eq!(c.len, data.len() as u64, "{name}");
        assert!(!c.corrupted);
        assert_eq!(&img[*off as usize..(*off + c.len) as usize], &data[..]);
    }
    assert_eq!(found.len(), fx.len(), "no false positives: {found:?}");
    // Results come in offset order.
    assert!(found.windows(2).all(|w| w[0].offset < w[1].offset));
}

#[test]
fn small_chunks_and_unaligned_sizes_give_the_same_result() {
    let fx = fixtures();
    let (mut img, _) = layout(&fx);
    img.extend([1, 2, 3]);
    let a = run(&img, &Options::default());
    let b = run(
        &img,
        &Options {
            chunk: 4096,
            ..Options::default()
        },
    );
    assert_eq!(a, b);
}

#[test]
fn filters_select_photorec_families() {
    let fx = fixtures();
    let (img, _) = layout(&fx);
    let include = run(
        &img,
        &Options {
            filter: Filter::Include(["jpg".to_owned(), "zip".to_owned()].into()),
            ..Options::default()
        },
    );
    let mut exts: Vec<&str> = include.iter().map(|c| c.ext).collect();
    exts.sort_unstable();
    exts.dedup();
    assert_eq!(exts, ["docx", "jpg", "xlsx", "zip"]);
    let exclude = run(
        &img,
        &Options {
            filter: Filter::Exclude(["mp3".to_owned(), "mov".to_owned()].into()),
            ..Options::default()
        },
    );
    assert!(
        exclude
            .iter()
            .all(|c| c.family != "mp3" && c.family != "mov")
    );
    assert_eq!(exclude.len(), fx.len() - 5);
}

#[test]
fn truncated_files_are_kept_only_on_request() {
    let jpg = ak::jpeg(64, 64);
    let mut img = noise(1024, 3);
    let at = img.len();
    // Cut the JPEG in its entropy-coded data, then zeros.
    img.extend(&jpg[..jpg.len() - 40]);
    img.extend(vec![0u8; 4096 - (img.len() % 512)]);
    assert!(run(&img, &Options::default()).is_empty());
    let kept = run(
        &img,
        &Options {
            keep_corrupted: true,
            ..Options::default()
        },
    );
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].offset, at as u64);
    assert!(kept[0].corrupted);
    assert_eq!(kept[0].ext, "jpg");
}

#[test]
fn stored_inner_zip_does_not_end_the_outer_one() {
    let inner = ak::zip(&[("in.txt", b"inner")]);
    let mut b = ak::ZipBuilder::new();
    b.add(ak::ZipEntry::new("inner.zip", inner).method(ak::Method::Stored));
    b.file("after.txt", b"after");
    let outer = b.build();
    let mut img = outer.clone();
    img.extend(vec![0u8; 1024]);
    let found = run(&img, &Options::default());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].len, outer.len() as u64);
}

#[test]
fn pdf_incremental_updates_are_included() {
    let base = ak::pdf(&[], &[]);
    let mut pdf = base.clone();
    pdf.extend(b"5 0 obj\n<< /Type /Annot >>\nendobj\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 6 >>\nstartxref\n0\n%%EOF\n");
    let mut img = pdf.clone();
    img.extend(vec![0u8; 2048]);
    let found = run(&img, &Options::default());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].len, pdf.len() as u64);
}

#[test]
fn cancellation_stops_the_scan() {
    let fx = fixtures();
    let (img, _) = layout(&fx);
    let src = MemSource::new(img);
    let mut n = 0;
    let stats = carve(
        &src,
        &Options {
            chunk: 4096,
            ..Options::default()
        },
        &|| true,
        &mut |_| n += 1,
    )
    .unwrap();
    assert!(stats.cancelled);
    assert_eq!(n, 0);
}
