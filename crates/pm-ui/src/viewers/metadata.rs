//! File metadata viewer (Autopsy's "File Metadata" tab).

use std::sync::Arc;

use pm_fs::{ExtentKind, FileSystem, MetaFlags, Times};

use crate::format::{DisplayPrefs, size_long};
use crate::model::Content;

fn row(ui: &mut egui::Ui, k: &str, v: impl Into<egui::WidgetText>) {
    ui.label(egui::RichText::new(k).strong());
    ui.add(egui::Label::new(v).selectable(true));
    ui.end_row();
}

fn times(ui: &mut egui::Ui, title: &str, t: &Times, prefs: &DisplayPrefs) {
    ui.label(egui::RichText::new(title).strong());
    ui.end_row();
    row(ui, "  Modified", prefs.time(t.modified));
    row(ui, "  Accessed", prefs.time(t.accessed));
    row(ui, "  Changed", prefs.time(t.changed));
    row(ui, "  Created", prefs.time(t.created));
}

pub(super) fn ui(
    ui: &mut egui::Ui,
    c: &Content,
    fs: Option<&Arc<dyn FileSystem>>,
    prefs: &DisplayPrefs,
    hashes: Option<&super::FileHashes>,
) {
    // Live metadata from the file system has what the case database does
    // not keep ($FILE_NAME times, link count); case rows fall back to the
    // stored values.
    let live = fs.and_then(|fs| fs.metadata(c.row.inode).ok());
    let meta = live.as_ref().or(c.row.meta.as_ref());
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            egui::Grid::new("metadata")
                .num_columns(2)
                .striped(true)
                .spacing([24.0, 4.0])
                .show(ui, |ui| {
                    let r = &c.row;
                    row(ui, "Name", &r.name);
                    row(ui, "Path", &r.path);
                    row(ui, "Type", format!("{:?}", r.kind));
                    // Autopsy's "MIME Type" (from File Type Identification).
                    match hashes.and_then(|h| h.mime.as_deref()) {
                        Some(m) => row(ui, "MIME Type", m),
                        None if hashes.is_some() => {
                            row(ui, "MIME Type", egui::RichText::new("Not detected").weak());
                        }
                        None => {}
                    }
                    row(ui, "Size", size_long(r.size));
                    // Hashes from the case database (computed by Hash Lookup),
                    // right after the size as in Autopsy.
                    if let Some(h) = hashes {
                        let hash = |ui: &mut egui::Ui, k: &str, v: &Option<String>| match v {
                            Some(v) => row(ui, k, egui::RichText::new(v).monospace()),
                            None => row(ui, k, egui::RichText::new("Not calculated").weak()),
                        };
                        hash(ui, "MD5", &h.md5);
                        hash(ui, "SHA-1", &h.sha1);
                        hash(ui, "SHA-256", &h.sha256);
                        row(ui, "Known", &h.known);
                    }
                    row(
                        ui,
                        "Name",
                        if r.name_allocated {
                            "Allocated"
                        } else {
                            "Unallocated (deleted entry)"
                        },
                    );
                    if let Some(m) = meta {
                        row(
                            ui,
                            "Metadata",
                            if m.is_allocated() {
                                "Allocated"
                            } else {
                                "Unallocated"
                            },
                        );
                        let mut flags = Vec::new();
                        for (f, n) in [
                            (MetaFlags::COMPRESSED, "compressed"),
                            (MetaFlags::ENCRYPTED, "encrypted"),
                            (MetaFlags::SPARSE, "sparse"),
                            (
                                MetaFlags::RECOVERED_BEST_EFFORT,
                                "content recovered on a best-effort basis",
                            ),
                        ] {
                            if m.flags.contains(f) {
                                flags.push(n);
                            }
                        }
                        if !flags.is_empty() {
                            row(ui, "Flags", flags.join(", "));
                        }
                        row(ui, "Inode / MFT entry", m.inode.to_string());
                        if let Some(seq) = m.seq {
                            row(ui, "Sequence", seq.to_string());
                        }
                        if let Some(a) = m.attributes {
                            row(ui, "Attributes", format!("0x{a:08X}"));
                        }
                        if let Some(mode) = m.mode {
                            row(ui, "Mode", format!("{mode:o}"));
                        }
                        row(ui, "Link count", m.link_count.to_string());
                        times(
                            ui,
                            if m.fn_times.is_some() {
                                "$STANDARD_INFORMATION times"
                            } else {
                                "Times"
                            },
                            &m.times,
                            prefs,
                        );
                        if let Some(fnt) = &m.fn_times {
                            times(ui, "$FILE_NAME times", fnt, prefs);
                        }
                    }
                    if let Some(fs) = fs {
                        row(
                            ui,
                            "File system",
                            format!("{} ({} byte blocks)", fs.info().kind, fs.info().block_size),
                        );
                    }
                    if let Some((_, f)) = c.case_file() {
                        row(ui, "Object ID", f.id.to_string());
                        row(ui, "Origin", format!("{:?}", f.origin));
                    }
                });
            let Some(fs) = fs else {
                return;
            };
            if let Ok(streams) = fs.streams(c.row.inode)
                && streams.len() > 1
            {
                ui.separator();
                ui.strong("Streams");
                for s in streams {
                    ui.label(format!(
                        "#{} {} — {:?}, {}",
                        s.id,
                        s.name.as_deref().unwrap_or("(default)"),
                        s.kind,
                        size_long(s.size)
                    ));
                }
            }
            if let Ok(extents) = fs.extents(c.row.inode, c.row.stream)
                && !extents.is_empty()
            {
                ui.separator();
                ui.strong(format!("File layout ({} runs)", extents.len()));
                for e in extents.iter().take(200) {
                    let what = match e.kind {
                        ExtentKind::Allocated => format!(
                            "bytes {}–{} on volume",
                            e.physical_offset,
                            e.physical_offset + e.len - 1
                        ),
                        ExtentKind::Sparse => "sparse (zeros)".to_owned(),
                        ExtentKind::Resident => "resident in metadata record".to_owned(),
                    };
                    ui.monospace(format!(
                        "offset {:>12}  length {:>10}  {what}",
                        e.logical_offset, e.len
                    ));
                }
            }
        });
}
