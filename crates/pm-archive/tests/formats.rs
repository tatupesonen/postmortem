//! Every container format on fixtures generated with `pm-testkit`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

mod common;

use common::{detect, member, paths, run, run_with};
use pm_archive::{ArchiveError, BombReason, Format, MemberEnd, Options};
use pm_testkit::archives::{self as ak, Method, ZipBuilder, ZipEntry};

const TEXT: &[u8] = b"The quick brown fox jumps over the lazy dog.\n";

fn big_text() -> Vec<u8> {
    TEXT.repeat(500)
}

// ------------------------------------------------------------------ zip

#[test]
fn zip_methods_names_and_times() {
    let mut b = ZipBuilder::new();
    b.dir("docs")
        .add(ZipEntry::new("docs/a.txt", TEXT).method(Method::Stored))
        .add(ZipEntry::new("docs/b.txt", big_text()).unix_mtime(1_600_000_000))
        .add(ZipEntry::new("c.bz2.txt", big_text()).method(Method::Bzip2))
        .add(ZipEntry::new("d.zst.txt", big_text()).method(Method::Zstd))
        .add(ZipEntry::new("e.xz.txt", big_text()).method(Method::Xz))
        .add(ZipEntry::new("päivä.txt".as_bytes(), TEXT).utf8());
    let z = b.build();
    assert_eq!(detect(&z), Some(Format::Zip));
    let (s, ms) = run(Format::Zip, &z);
    assert_eq!(s.members, 7);
    assert_eq!(s.encrypted, 0);
    assert!(member(&ms, "docs/").entry.is_dir);
    assert_eq!(member(&ms, "docs/a.txt").data, TEXT);
    for p in ["docs/b.txt", "c.bz2.txt", "d.zst.txt", "e.xz.txt"] {
        let m = member(&ms, p);
        assert_eq!(m.data, big_text(), "{p}");
        assert_eq!(m.end, MemberEnd::Complete, "{p}");
        assert_eq!(m.entry.size, Some(big_text().len() as u64));
    }
    assert_eq!(
        member(&ms, "docs/b.txt").entry.modified,
        Some(1_600_000_000)
    );
    assert_eq!(member(&ms, "docs/a.txt").entry.modified, Some(ak::MTIME));
    let utf = member(&ms, "päivä.txt");
    assert_eq!(utf.entry.raw_path, None);
}

#[test]
fn zip_cp437_names_keep_raw_bytes() {
    let mut b = ZipBuilder::new();
    b.add(ZipEntry::new(
        &[b'M', 0x84, b'r', b'z', b'.', b't', b'x', b't'][..],
        TEXT,
    ));
    let (_, ms) = run(Format::Zip, &b.build());
    assert_eq!(ms[0].entry.path, "März.txt");
    assert_eq!(ms[0].entry.raw_path.as_deref(), Some(&b"M\x84rz.txt"[..]));
}

#[test]
fn zip_encryption_is_reported_and_zipcrypto_decrypted_with_password() {
    let mut b = ZipBuilder::new();
    b.add(ZipEntry::new("plain.txt", TEXT))
        .add(ZipEntry::new("secret.txt", big_text()).password("pw"))
        .add(
            ZipEntry::new("stored-secret.txt", TEXT)
                .method(Method::Stored)
                .password("pw"),
        )
        .add(ZipEntry::new("aes.txt", TEXT).fake_aes());
    let z = b.build();
    let (s, ms) = run(Format::Zip, &z);
    assert_eq!(s.encrypted, 3);
    let sec = member(&ms, "secret.txt");
    assert!(sec.entry.encrypted);
    assert!(sec.data.is_empty());
    assert!(matches!(sec.end, MemberEnd::Skipped(_)));
    assert!(member(&ms, "aes.txt").entry.encrypted);
    assert_eq!(member(&ms, "plain.txt").data, TEXT);

    let opts = Options {
        password: Some(b"pw".to_vec()),
        ..Options::default()
    };
    let (r, ms) = run_with(Format::Zip, &z, &opts);
    assert_eq!(
        r.unwrap().encrypted,
        1,
        "only the AES entry stays encrypted"
    );
    assert_eq!(member(&ms, "secret.txt").data, big_text());
    assert_eq!(member(&ms, "secret.txt").end, MemberEnd::Complete);
    assert_eq!(member(&ms, "stored-secret.txt").data, TEXT);

    let wrong = Options {
        password: Some(b"nope".to_vec()),
        ..Options::default()
    };
    let (r, _) = run_with(Format::Zip, &z, &wrong);
    assert_eq!(r.unwrap().encrypted, 3);
}

#[test]
fn zip_without_central_directory_and_with_descriptors() {
    let mut b = ZipBuilder::new();
    b.add(ZipEntry::new("a.txt", big_text()).descriptor())
        .add(
            ZipEntry::new("b.txt", TEXT)
                .method(Method::Stored)
                .descriptor(),
        )
        .add(ZipEntry::new("c.txt", TEXT));
    let full = b.build();
    let (_, ms) = run(Format::Zip, &full);
    assert_eq!(member(&ms, "a.txt").data, big_text());
    assert_eq!(member(&ms, "b.txt").data, TEXT);

    b.omit_central = true;
    let (s, ms) = run(Format::Zip, &b.build());
    assert_eq!(paths(&ms), ["a.txt", "b.txt", "c.txt"]);
    assert_eq!(member(&ms, "a.txt").data, big_text());
    assert_eq!(member(&ms, "b.txt").data, TEXT);
    assert_eq!(member(&ms, "c.txt").end, MemberEnd::Complete);
    assert!(s.warnings.iter().any(|w| w.contains("central directory")));

    // A carved/truncated archive: the central directory is cut off.
    let cut = &full[..full.len() - 30];
    let (_, ms) = run(Format::Zip, cut);
    assert_eq!(member(&ms, "c.txt").data, TEXT);
}

#[test]
fn zip_with_prepended_data() {
    let mut b = ZipBuilder::new();
    b.prefix = b"MZ self-extractor stub ".repeat(40);
    b.file("x/y.txt", TEXT);
    let (_, ms) = run(Format::Zip, &b.build());
    assert_eq!(member(&ms, "x/y.txt").data, TEXT);
}

#[test]
fn zip_declared_ratio_bomb_follows_autopsy() {
    // 600 MB declared from a few hundred bytes: ratio far above 600.
    let mut b = ZipBuilder::new();
    b.file("ok.txt", TEXT)
        .add(ZipEntry::new("huge.bin", vec![0u8; 1000]).declared_size(600_000_000));
    let z = b.build();
    let (r, ms) = run_with(Format::Zip, &z, &Options::default());
    match r {
        Err(ArchiveError::Bomb(BombReason::DeclaredRatio { path, ratio })) => {
            assert_eq!(path, "huge.bin");
            assert!(ratio >= 600);
        }
        other => panic!("{other:?}"),
    }
    assert!(ms.is_empty(), "nothing is extracted before the check");
    // Below 500 MB the ratio is not checked.
    let mut b = ZipBuilder::new();
    b.add(ZipEntry::new("big.bin", vec![0u8; 1000]).declared_size(499_999_999));
    let (r, _) = run_with(Format::Zip, &b.build(), &Options::default());
    assert!(r.is_ok());
    // Deleted archives skip the declared check (Autopsy).
    let opts = Options {
        check_declared: false,
        ..Options::default()
    };
    assert!(run_with(Format::Zip, &z, &opts).0.is_ok());
}

#[test]
fn lying_headers_are_caught_by_the_decoded_byte_meter() {
    let data = vec![0u8; 2_000_000];
    let mut b = ZipBuilder::new();
    b.add(ZipEntry::new("liar.bin", data).declared_size(10));
    let opts = Options {
        ratio_min_size: 1_000_000,
        ..Options::default()
    };
    let (r, _) = run_with(Format::Zip, &b.build(), &opts);
    assert!(
        matches!(r, Err(ArchiveError::Bomb(BombReason::MemberRatio { .. }))),
        "{r:?}"
    );
}

/// David Fifield's non-recursive bomb: many central-directory entries
/// share one compressed local entry.
#[test]
fn overlapping_entries_are_caught_by_the_container_meter() {
    let one = {
        let mut b = ZipBuilder::new();
        b.file("k", &vec![0u8; 1 << 20]);
        b.build()
    };
    let eocd = one.len() - 22;
    let cd_size = u32::from_le_bytes(one[eocd + 12..eocd + 16].try_into().unwrap()) as usize;
    let cd_start = eocd - cd_size;
    let entry = &one[cd_start..eocd];
    let n = 50u16;
    let mut z = one[..cd_start].to_vec();
    let new_cd = z.len() as u32;
    for _ in 0..n {
        z.extend(entry);
    }
    z.extend(b"PK\x05\x06\0\0\0\0");
    z.extend(n.to_le_bytes());
    z.extend(n.to_le_bytes());
    z.extend((entry.len() as u32 * u32::from(n)).to_le_bytes());
    z.extend(new_cd.to_le_bytes());
    z.extend([0, 0]);
    let opts = Options {
        ratio_min_size: 10 << 20,
        ..Options::default()
    };
    let (r, ms) = run_with(Format::Zip, &z, &opts);
    assert!(
        matches!(
            r,
            Err(ArchiveError::Bomb(BombReason::ContainerRatio { .. }))
        ),
        "{r:?}"
    );
    assert!(ms.len() < 20);
}

#[test]
fn member_cap() {
    let mut b = ZipBuilder::new();
    for i in 0..20 {
        b.file(&format!("f{i}"), b"x");
    }
    let opts = Options {
        max_members: 10,
        ..Options::default()
    };
    let (r, _) = run_with(Format::Zip, &b.build(), &opts);
    assert!(matches!(
        r,
        Err(ArchiveError::Bomb(BombReason::TooManyMembers { limit: 10 }))
    ));
}

#[test]
fn corrupt_member_data_fails_only_that_member() {
    let mut b = ZipBuilder::new();
    b.file("a.txt", &big_text()).file("b.txt", TEXT);
    let mut z = b.build();
    // Corrupt the deflate stream of a.txt.
    for byte in &mut z[40..80] {
        *byte ^= 0x55;
    }
    let (s, ms) = run(Format::Zip, &z);
    assert!(matches!(
        member(&ms, "a.txt").end,
        MemberEnd::Failed(_) | MemberEnd::ChecksumMismatch
    ));
    assert_eq!(member(&ms, "b.txt").data, TEXT);
    assert!(s.failed + s.complete >= 2);
}

// ------------------------------------------------------------------ tar

#[test]
fn tar_variants() {
    use ak::TarEntry as T;
    let long = format!("{}/file.txt", "d".repeat(150));
    let t = ak::tar(&[
        T::Dir("dir".into()),
        T::File("dir/a.txt".into(), TEXT.to_vec()),
        T::GnuLong(long.clone(), big_text()),
        T::Pax("pax/ünïcode name.txt".into(), TEXT.to_vec()),
        T::Symlink("link".into(), "dir/a.txt".into()),
        T::File("empty".into(), Vec::new()),
    ]);
    assert_eq!(detect(&t), Some(Format::Tar));
    let (s, ms) = run(Format::Tar, &t);
    assert_eq!(s.members, 6);
    assert!(member(&ms, "dir/").entry.is_dir);
    assert_eq!(member(&ms, "dir/a.txt").data, TEXT);
    assert_eq!(member(&ms, "dir/a.txt").entry.modified, Some(ak::MTIME));
    assert_eq!(member(&ms, &long).data, big_text());
    assert_eq!(member(&ms, "pax/ünïcode name.txt").data, TEXT);
    assert!(member(&ms, "link").data.is_empty());
    assert!(member(&ms, "empty").data.is_empty());
}

#[test]
fn tar_resynchronises_after_a_damaged_header() {
    use ak::TarEntry as T;
    let mut t = ak::tar(&[
        T::File("a.txt".into(), TEXT.to_vec()),
        T::File("b.txt".into(), TEXT.to_vec()),
        T::File("c.txt".into(), TEXT.to_vec()),
    ]);
    // Damage b.txt's header (second header at 1024).
    t[1024 + 148] = b'9';
    let (s, ms) = run(Format::Tar, &t);
    assert_eq!(paths(&ms), ["a.txt", "c.txt"]);
    assert!(!s.warnings.is_empty());
}

// ------------------------------------------------------- single streams

#[test]
fn single_stream_compressors() {
    let data = big_text();
    let gz = ak::gzip(Some("orig.txt"), &data);
    assert_eq!(detect(&gz), Some(Format::Gzip));
    let (_, ms) = run(Format::Gzip, &gz);
    assert_eq!(ms[0].entry.path, "orig.txt");
    assert_eq!(ms[0].entry.modified, Some(ak::MTIME));
    assert_eq!(ms[0].data, data);
    // Multi-member gzip decodes as one stream.
    let mut two = ak::gzip(None, b"one ");
    two.extend(ak::gzip(None, b"two"));
    let (_, ms) = run(Format::Gzip, &two);
    assert_eq!(ms[0].entry.path, "");
    assert_eq!(ms[0].data, b"one two");

    for (f, bytes) in [
        (Format::Bzip2, ak::bzip2_bytes(&data)),
        (Format::Xz, ak::xz_bytes(&data)),
        (Format::Zstd, ak::zstd_bytes(&data)),
    ] {
        assert_eq!(detect(&bytes), Some(f));
        let (s, ms) = run(f, &bytes);
        assert_eq!(s.members, 1);
        assert_eq!(ms[0].data, data, "{f:?}");
        assert_eq!(ms[0].end, MemberEnd::Complete);
    }
    // Truncated streams fail the member, not the extraction.
    let gz_cut = &gz[..gz.len() / 2];
    let (_, ms) = run(Format::Gzip, gz_cut);
    assert!(matches!(ms[0].end, MemberEnd::Failed(_)));
}

#[test]
fn single_stream_bomb_is_metered() {
    let zeros = vec![0u8; 8 << 20];
    let gz = ak::gzip(None, &zeros);
    let opts = Options {
        ratio_min_size: 4 << 20,
        ..Options::default()
    };
    let (r, _) = run_with(Format::Gzip, &gz, &opts);
    assert!(matches!(r, Err(ArchiveError::Bomb(_))), "{r:?}");
}

// ------------------------------------------------------------------- 7z

#[test]
fn sevenz_plain_solid_and_encrypted() {
    let files: Vec<(&str, &[u8])> = vec![("a.txt", TEXT), ("d/b.txt", b"bee")];
    for solid in [false, true] {
        let z = ak::sevenz(&files, &["d"], solid, None, false);
        assert_eq!(detect(&z), Some(Format::SevenZip));
        let (s, ms) = run(Format::SevenZip, &z);
        assert_eq!(s.members, 3, "solid={solid}");
        assert_eq!(member(&ms, "a.txt").data, TEXT);
        assert_eq!(member(&ms, "d/b.txt").data, b"bee");
        assert!(member(&ms, "d").entry.is_dir);
    }
    let enc = ak::sevenz(&files, &[], false, Some("pw"), false);
    let (s, ms) = run(Format::SevenZip, &enc);
    assert_eq!(s.encrypted, 2);
    assert!(ms.iter().all(|m| m.entry.encrypted && m.data.is_empty()));
    let opts = Options {
        password: Some(b"pw".to_vec()),
        ..Options::default()
    };
    let (r, ms) = run_with(Format::SevenZip, &enc, &opts);
    assert_eq!(r.unwrap().encrypted, 0);
    assert_eq!(member(&ms, "a.txt").data, TEXT);

    let hidden = ak::sevenz(&files, &[], false, Some("pw"), true);
    let (r, _) = run_with(Format::SevenZip, &hidden, &Options::default());
    assert!(matches!(r, Err(ArchiveError::Encrypted)), "{r:?}");
}

#[test]
fn sevenz_dictionary_cap() {
    let z = ak::sevenz(&[("a.txt", TEXT)], &[], false, None, false);
    let opts = Options {
        max_dictionary: 1024,
        ..Options::default()
    };
    let (r, ms) = run_with(Format::SevenZip, &z, &opts);
    r.unwrap();
    assert!(matches!(&ms[0].end, MemberEnd::Skipped(why) if why.contains("dictionary")));
}

// ------------------------------------------------------------------ RAR

#[test]
fn rar_versions() {
    use rars::ArchiveVersion as V;
    let files: Vec<(&str, &[u8])> = vec![("a.txt", TEXT), ("dir/b.txt", &[7u8; 5000])];
    for v in [V::Rar15, V::Rar29, V::Rar50] {
        let r = ak::rar(v, &files, None);
        assert_eq!(detect(&r), Some(Format::Rar), "{v:?}");
        let (_, ms) = run(Format::Rar, &r);
        assert_eq!(member(&ms, "a.txt").data, TEXT, "{v:?}");
        assert_eq!(member(&ms, "dir/b.txt").data, vec![7u8; 5000], "{v:?}");
    }
}

#[test]
fn rar_encryption() {
    use rars::ArchiveVersion as V;
    let files: Vec<(&str, &[u8])> = vec![("a.txt", TEXT), ("b.txt", b"bee")];
    for v in [V::Rar29, V::Rar50] {
        let r = ak::rar(v, &files, Some("pw"));
        let (s, ms) = run(Format::Rar, &r);
        assert_eq!(s.encrypted, 2, "{v:?}");
        assert!(ms.iter().all(|m| m.data.is_empty()));
        let opts = Options {
            password: Some(b"pw".to_vec()),
            ..Options::default()
        };
        let (res, ms) = run_with(Format::Rar, &r, &opts);
        assert_eq!(res.unwrap().encrypted, 0, "{v:?}");
        assert_eq!(member(&ms, "a.txt").data, TEXT, "{v:?}");
    }
}

#[test]
fn rar_spools_large_archives() {
    use rars::ArchiveVersion as V;
    let big = vec![42u8; 300_000];
    let r = ak::rar(V::Rar50, &[("big.bin", &big[..])], None);
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        max_in_memory: 1000,
        temp_dir: Some(dir.path().to_path_buf()),
        ..Options::default()
    };
    let (res, ms) = run_with(Format::Rar, &r, &opts);
    res.unwrap();
    assert_eq!(ms[0].data, big);
}

// ------------------------------------------------------------------ ISO

#[test]
fn iso_joliet_rock_ridge_and_plain() {
    let files: Vec<(&str, &[u8])> = vec![
        ("readme.txt", TEXT),
        ("Long Directory Name/a file with spaces.txt", b"inner"),
    ];
    for (joliet, rr) in [(true, true), (true, false), (false, true), (false, false)] {
        let img = ak::iso(&files, joliet, rr);
        assert_eq!(detect(&img), Some(Format::Iso9660));
        let (_, ms) = run(Format::Iso9660, &img);
        let names = paths(&ms);
        let readme = ms
            .iter()
            .find(|m| m.entry.path.eq_ignore_ascii_case("readme.txt"))
            .unwrap_or_else(|| panic!("{names:?}"));
        assert_eq!(readme.data, TEXT);
        assert_eq!(readme.entry.modified, Some(ak::MTIME), "{names:?}");
        let inner = ms.iter().find(|m| m.data == b"inner").expect("nested file");
        if joliet || rr {
            assert_eq!(
                inner.entry.path, "Long Directory Name/a file with spaces.txt",
                "joliet={joliet} rr={rr}"
            );
        }
    }
}

// --------------------------------------------------------------- Office

#[test]
fn ooxml_media_and_embeddings() {
    let png = ak::png(4, 4);
    let jpg = ak::jpeg(8, 8);
    let doc = ak::ooxml(
        "word",
        &[("image1.png", &png), ("image2.jpeg", &jpg)],
        &[("oleObject1.bin", b"ole")],
    );
    assert_eq!(detect(&doc), Some(Format::Docx));
    let (_, ms) = run(Format::Docx, &doc);
    assert_eq!(paths(&ms), ["image1.png", "image2.jpeg", "oleObject1.bin"]);
    assert_eq!(member(&ms, "image1.png").data, png);
    assert_eq!(detect(&ak::ooxml("xl", &[], &[])), Some(Format::Xlsx));
    assert_eq!(detect(&ak::ooxml("ppt", &[], &[])), Some(Format::Pptx));
    let (_, ms) = run(Format::Pptx, &ak::ooxml("ppt", &[("a.png", &png)], &[]));
    assert_eq!(paths(&ms), ["a.png"]);
}

#[test]
fn legacy_office_pictures() {
    let png = ak::png(4, 4);
    let jpg = ak::jpeg(8, 8);
    let emf = {
        let mut e = 1u32.to_le_bytes().to_vec();
        e.extend([0u8; 36]);
        e.extend(b" EMF");
        e.extend([0u8; 40]);
        e
    };
    let wmf = vec![1u8, 0, 9, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut dib = 40u32.to_le_bytes().to_vec();
    dib.extend([0u8; 60]);
    let blips = vec![
        ak::blip(ak::BLIP_PNG, &png, 1),
        ak::blip(ak::BLIP_JPEG, &jpg, 2),
        ak::blip(ak::BLIP_EMF, &emf, 3),
        ak::blip(ak::BLIP_WMF, &wmf, 4),
        ak::blip(ak::BLIP_DIB, &dib, 5),
    ];

    let doc = ak::doc(&blips);
    assert_eq!(detect(&doc), Some(Format::Doc));
    let (_, ms) = run(Format::Doc, &doc);
    assert_eq!(
        paths(&ms),
        [
            "image_0.png",
            "image_1.jpg",
            "image_2.emf",
            "image_3.wmf",
            "image_4.dib"
        ]
    );
    assert_eq!(ms[0].data, png);
    assert_eq!(ms[1].data, jpg);
    assert_eq!(ms[2].data, emf, "EMF is inflated");
    assert_eq!(
        &ms[3].data[..4],
        &[0xD7, 0xCD, 0xC6, 0x9A],
        "placeable header"
    );
    assert_eq!(&ms[3].data[22..], &wmf[..]);
    assert_eq!(&ms[4].data[..2], b"BM");

    let xls = ak::xls(&blips);
    assert_eq!(detect(&xls), Some(Format::Xls));
    let (_, ms) = run(Format::Xls, &xls);
    assert_eq!(ms.len(), 5);
    assert_eq!(ms[1].entry.path, "image_1.jpeg");
    assert_eq!(ms[0].data, png);

    // Drawing groups longer than one BIFF record use Continue records.
    let big = ak::png(200, 200);
    let xls = ak::xls(&[ak::blip(ak::BLIP_PNG, &big, 9)]);
    let (_, ms) = run(Format::Xls, &xls);
    assert_eq!(ms[0].data, big);

    let ppt = ak::ppt(&blips);
    assert_eq!(detect(&ppt), Some(Format::Ppt));
    let (_, ms) = run(Format::Ppt, &ppt);
    // Autopsy keeps JPEG, PNG, WMF, EMF and PICT from presentations.
    assert_eq!(
        paths(&ms),
        ["image_0.png", "image_1.jpg", "image_2.emf", "image_3.wmf"]
    );
}

// ------------------------------------------------------------------ PDF

#[test]
fn pdf_attachments_and_images() {
    let jpg = ak::jpeg(8, 8);
    let pixels: Vec<u8> = (0..16 * 16).map(|i| i as u8).collect();
    let p = ak::pdf(
        &[("report.docx", b"attached bytes"), ("naïve.txt", TEXT)],
        &[
            ak::PdfImage::Jpeg(jpg.clone()),
            ak::PdfImage::Gray {
                w: 16,
                h: 16,
                pixels: pixels.clone(),
            },
        ],
    );
    assert_eq!(detect(&p), Some(Format::Pdf));
    let (_, ms) = run(Format::Pdf, &p);
    assert_eq!(
        paths(&ms),
        ["report.docx", "naïve.txt", "image0.jpg", "image1.png"]
    );
    assert_eq!(member(&ms, "report.docx").data, b"attached bytes");
    assert_eq!(member(&ms, "report.docx").entry.size, Some(14));
    assert_eq!(member(&ms, "naïve.txt").data, TEXT);
    assert_eq!(member(&ms, "image0.jpg").data, jpg);
    let png = &member(&ms, "image1.png").data;
    let decoded = image::load_from_memory(png).expect("valid PNG").to_luma8();
    assert_eq!(decoded.dimensions(), (16, 16));
    assert_eq!(decoded.into_raw(), pixels);
}

#[test]
fn pdf_encrypted_and_damaged() {
    let mut p = ak::pdf(&[("a.txt", TEXT)], &[]);
    let enc = String::from_utf8_lossy(&p).replace("/Root", "/Encrypt 99 0 R /Root");
    let (r, _) = run_with(Format::Pdf, enc.as_bytes(), &Options::default());
    assert!(matches!(r, Err(ArchiveError::Encrypted)), "{r:?}");
    // Without its xref table and trailer, objects are still found.
    let cut = p.windows(4).position(|w| w == b"xref").unwrap();
    p.truncate(cut);
    let (_, ms) = run(Format::Pdf, &p);
    assert_eq!(member(&ms, "a.txt").data, TEXT);
}

// ------------------------------------------------------------ detection

#[test]
fn mime_detection_is_content_based() {
    // A zip named .txt is still a zip (Autopsy's runTestExtension).
    assert_eq!(detect(&ak::zip(&[("file.txt", b"x")])), Some(Format::Zip));
    assert_eq!(detect(b"just text"), None);
    assert_eq!(
        detect(&pm_lnk::builder::compound_file(&[("Other", vec![1; 10])])),
        None
    );
}
