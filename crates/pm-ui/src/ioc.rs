//! IOC import (Tools › IOC Search): paste indicators — hashes, IP and
//! e-mail addresses, domains, file names, one per line — and see which
//! occur in the case. Hashes are looked up in the files' hash columns;
//! everything else runs a pivot (results with the value, file names,
//! accounts, text). Every hit opens its file or result.

use std::sync::Arc;

use pm_case::Case;

use crate::pivot::{Found, Item, Open};
use crate::task::Keyed;

/// Indicators searched at once.
const MAX_IOCS: usize = 500;

/// What kind of indicator a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Md5,
    Sha1,
    Sha256,
    Ip,
    Email,
    Domain,
    Other,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
            Self::Sha256 => "SHA-256",
            Self::Ip => "IP address",
            Self::Email => "E-mail address",
            Self::Domain => "Domain",
            Self::Other => "Name or text",
        }
    }
}

/// One indicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ioc {
    pub(crate) value: String,
    pub(crate) kind: Kind,
}

/// Indicators in `text`, one per line; defanged forms (`hxxp`, `[.]`,
/// `(.)`) are restored, comments (`#`) and blanks skipped, duplicates
/// dropped.
pub(crate) fn parse(text: &str) -> Vec<Ioc> {
    let mut out: Vec<Ioc> = Vec::new();
    for line in text.lines() {
        let v = line.split('#').next().unwrap_or("").trim();
        if v.is_empty() {
            continue;
        }
        let v = v
            .replace("[.]", ".")
            .replace("(.)", ".")
            .replace("[@]", "@")
            .replace("hxxp", "http");
        let kind = classify(&v);
        let value = match kind {
            Kind::Md5 | Kind::Sha1 | Kind::Sha256 | Kind::Domain | Kind::Email => v.to_lowercase(),
            _ => v,
        };
        if !out.iter().any(|i| i.value == value) {
            out.push(Ioc { value, kind });
        }
        if out.len() >= MAX_IOCS {
            break;
        }
    }
    out
}

/// Extensions that make `name.ext` a file name rather than a domain.
const FILE_EXTENSIONS: [&str; 16] = [
    "exe", "dll", "sys", "bat", "cmd", "ps1", "vbs", "js", "doc", "docx", "xls", "xlsx", "pdf",
    "zip", "rar", "txt",
];

fn classify(v: &str) -> Kind {
    let hex = v.chars().all(|c| c.is_ascii_hexdigit());
    match (hex, v.len()) {
        (true, 32) => return Kind::Md5,
        (true, 40) => return Kind::Sha1,
        (true, 64) => return Kind::Sha256,
        _ => {}
    }
    if v.parse::<std::net::IpAddr>().is_ok() {
        return Kind::Ip;
    }
    if v.contains('@') && !v.contains(' ') {
        return Kind::Email;
    }
    // `rclone.exe` is a file name, not a domain.
    if v.rsplit_once('.')
        .is_some_and(|(_, ext)| FILE_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
    {
        return Kind::Other;
    }
    let labels: Vec<&str> = v.split('.').collect();
    if labels.len() >= 2
        && labels
            .iter()
            .all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && labels
            .last()
            .is_some_and(|t| t.chars().all(|c| c.is_ascii_alphabetic()))
    {
        return Kind::Domain;
    }
    Kind::Other
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Looks every indicator up.
fn search(case: &Case, iocs: &[Ioc]) -> Result<Vec<(Ioc, Found)>, String> {
    let mut out = Vec::with_capacity(iocs.len());
    for ioc in iocs {
        let found = match ioc.kind {
            Kind::Md5 | Kind::Sha1 | Kind::Sha256 => {
                let digest = decode_hex(&ioc.value).unwrap_or_default();
                let files = case
                    .db()
                    .files_with_hash(&digest, 200)
                    .map_err(|e| e.to_string())?;
                Found {
                    files: files
                        .into_iter()
                        .map(|f| Item {
                            title: f.name.clone(),
                            detail: f.parent_path.clone(),
                            open: Some(Open::File(f.id)),
                        })
                        .collect(),
                    ..Found::default()
                }
            }
            _ => crate::pivot::find(case, &ioc.value)?,
        };
        out.push((ioc.clone(), found));
    }
    Ok(out)
}

/// The IOC Search window.
#[derive(Debug, Default)]
pub(crate) struct IocWindow {
    input: String,
    /// The text last searched.
    searched: Option<String>,
    results: Keyed<String, Vec<(Ioc, Found)>>,
    only_hits: bool,
}

impl IocWindow {
    /// Draws the window; returns what to open and `false` once closed.
    pub(crate) fn ui(&mut self, ctx: &egui::Context, case: &Arc<Case>) -> (Option<Open>, bool) {
        let mut open = true;
        let mut chosen = None;
        egui::Window::new("IOC Search")
            .id(egui::Id::new("ioc-window"))
            .open(&mut open)
            .default_size([640.0, 600.0])
            .show(ctx, |ui| {
                ui.label("Indicators, one per line: hashes (MD5, SHA-1, SHA-256), IP and e-mail addresses, domains, names.");
                ui.add(
                    egui::TextEdit::multiline(&mut self.input)
                        .hint_text("d41d8cd98f00b204e9800998ecf8427e\n203.0.113.7\nevil[.]example\nrclone.exe")
                        .desired_rows(6)
                        .desired_width(f32::INFINITY)
                        .code_editor(),
                );
                let iocs = parse(&self.input);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!iocs.is_empty(), egui::Button::new(format!("Search {} indicators", iocs.len())))
                        .clicked()
                    {
                        self.searched = Some(self.input.clone());
                    }
                    ui.checkbox(&mut self.only_hits, "Only indicators found");
                });
                ui.separator();
                let Some(text) = self.searched.clone() else {
                    return;
                };
                let (case2, searched) = (Arc::clone(case), parse(&text));
                let results = self
                    .results
                    .get(ctx, &text, move || search(&case2, &searched));
                if let Some(e) = self.results.error() {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                if self.results.loading() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Searching…");
                    });
                }
                let Some(results) = results else { return };
                let hits = results
                    .iter()
                    .filter(|(_, f)| total(f) > 0)
                    .count();
                ui.strong(format!("{hits} of {} indicators found", results.len()));
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (ioc, f) in results.iter() {
                        let n = total(f);
                        if self.only_hits && n == 0 {
                            continue;
                        }
                        let title = format!("{}  ·  {}  ·  {n} hits", ioc.value, ioc.kind.label());
                        let text = if n > 0 {
                            egui::RichText::new(title).color(ui.visuals().warn_fg_color)
                        } else {
                            egui::RichText::new(title).weak()
                        };
                        egui::CollapsingHeader::new(text)
                            .id_salt(("ioc", &ioc.value))
                            .default_open(false)
                            .show(ui, |ui| {
                                for (section, items) in [
                                    ("Files", &f.files),
                                    ("Results", &f.results),
                                    ("Accounts", &f.accounts),
                                    ("Text", &f.text),
                                ] {
                                    if items.is_empty() {
                                        continue;
                                    }
                                    ui.strong(section);
                                    for it in items {
                                        ui.horizontal(|ui| {
                                            let r = match it.open {
                                                Some(_) => ui.link(&it.title),
                                                None => ui.label(&it.title),
                                            };
                                            if r.clicked() {
                                                chosen = it.open;
                                            }
                                            ui.weak(&it.detail);
                                        });
                                    }
                                }
                            });
                    }
                });
            });
        (chosen, open)
    }
}

fn total(f: &Found) -> usize {
    f.files.len() + f.results.len() + f.accounts.len() + f.text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indicators_are_classified_and_refanged() {
        let iocs = parse(
            "# campaign 42\n\
             D41D8CD98F00B204E9800998ECF8427E\n\
             203.0.113.7\n\
             evil[.]example\n\
             boss[@]evil.example\n\
             rclone.exe\n\
             \n\
             evil.example\n",
        );
        let kinds: Vec<(String, Kind)> = iocs.into_iter().map(|i| (i.value, i.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("d41d8cd98f00b204e9800998ecf8427e".to_owned(), Kind::Md5),
                ("203.0.113.7".to_owned(), Kind::Ip),
                ("evil.example".to_owned(), Kind::Domain),
                ("boss@evil.example".to_owned(), Kind::Email),
                ("rclone.exe".to_owned(), Kind::Other),
            ],
            "duplicates dropped"
        );
        assert_eq!(classify(&"a".repeat(64)), Kind::Sha256);
        assert_eq!(classify("secret plan"), Kind::Other);
        assert_eq!(decode_hex("00ff"), Some(vec![0, 255]));
    }
}
