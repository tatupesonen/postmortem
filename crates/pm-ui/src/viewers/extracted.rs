//! Extracted Text viewer (Autopsy `ExtractedTextViewer`): the text the
//! keyword index holds for a file, page by page (Autopsy's chunks), with
//! the hits of the current keyword search highlighted and navigable.
//!
//! Files that are not in the index are extracted on the fly (Autopsy's
//! `FileReaderExtractedText`), keeping at most [`MAX_EXTRACTED_PAGES`].

use std::ops::Range;
use std::sync::Arc;

use pm_case::Case;
use pm_casedb::FileRecord;
use pm_search::{CaseIndex, ObjRef, TextQuery};

use crate::task::Task;

/// Pages kept when extracting a file that is not indexed (about 1 MB of
/// text per 32 pages).
const MAX_EXTRACTED_PAGES: usize = 200;

/// One page: its text and where the overlap with the next page starts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Page {
    text: String,
    base: usize,
}

/// Where the pages come from.
enum Source {
    Indexed {
        ix: Arc<CaseIndex>,
        obj: ObjRef,
    },
    /// Extracted now; all pages kept.
    Extracted(Vec<Arc<Page>>),
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Indexed { obj, .. } => write!(f, "Indexed({obj:?})"),
            Self::Extracted(p) => write!(f, "Extracted({} pages)", p.len()),
        }
    }
}

/// A document opened in the viewer.
#[derive(Debug)]
struct Doc {
    source: Source,
    pages: u32,
    /// More text than was kept (on-the-fly extraction only).
    capped: bool,
    /// Every hit as `(page, byte range)`, in reading order.
    hits: Vec<(u32, Range<usize>)>,
}

/// Opens the text of `file` and finds `query`'s hits in it.
fn open(case: &Case, file: &FileRecord, query: Option<&TextQuery>) -> Result<Option<Doc>, String> {
    let ix = case.index().map_err(|e| e.to_string())?;
    let content = ix.content();
    content.reload().map_err(|e| e.to_string())?;
    let obj = ObjRef::File(file.id);
    if let Some(pages) = content.num_chunks(obj).map_err(|e| e.to_string())? {
        let hits = match query {
            Some(q) => content
                .object_hits(obj, q)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|h| h.page > 0)
                .flat_map(|h| h.ranges.into_iter().map(move |r| (h.page, r)))
                .collect(),
            None => Vec::new(),
        };
        return Ok(Some(Doc {
            source: Source::Indexed { ix, obj },
            pages,
            capped: false,
            hits,
        }));
    }
    extract_now(case, file)
}

/// Autopsy's `FileReaderExtractedText`: not for empty files or files
/// without a text extractor.
fn extract_now(case: &Case, file: &FileRecord) -> Result<Option<Doc>, String> {
    let Some(kind) = file
        .mime
        .as_deref()
        .and_then(|m| pm_text::kind_for_mime(m, file.size))
    else {
        return Ok(None);
    };
    if file.size == 0 {
        return Ok(None);
    }
    let src = case.content(file).map_err(|e| e.to_string())?;
    let mut pages = Vec::new();
    let mut capped = false;
    let mut sink = pm_text::ChunkingSink::new(|c: pm_text::Chunk| {
        if pages.len() == MAX_EXTRACTED_PAGES {
            capped = true;
            return Err(pm_text::Stop);
        }
        pages.push(Arc::new(Page {
            base: c.base_bytes,
            text: c.text,
        }));
        Ok(())
    });
    // A stop or a parser error keeps the text extracted so far.
    let _ = pm_text::extract(kind, &*src, &mut sink, &pm_text::ExtractOptions::default());
    let _ = sink.finish();
    if pages.is_empty() {
        return Ok(None);
    }
    Ok(Some(Doc {
        pages: u32::try_from(pages.len()).unwrap_or(u32::MAX),
        source: Source::Extracted(pages),
        capped,
        hits: Vec::new(),
    }))
}

fn load_page(source: &Source, page: u32) -> Result<Option<Arc<Page>>, String> {
    match source {
        Source::Indexed { ix, obj } => Ok(ix
            .content()
            .page(*obj, page)
            .map_err(|e| e.to_string())?
            .map(|p| {
                Arc::new(Page {
                    base: p.base.min(p.text.len()),
                    text: p.text,
                })
            })),
        Source::Extracted(pages) => Ok(pages.get(page.saturating_sub(1) as usize).cloned()),
    }
}

/// Loads one page.
type PageTask = Task<Result<Option<Arc<Page>>, String>>;

/// The viewer's state for the shown file.
#[derive(Debug, Default)]
pub(crate) struct ExtractedView {
    doc: Option<Task<Result<Option<Arc<Doc>>, String>>>,
    page: u32,
    page_task: Option<(u32, PageTask)>,
    /// Index into `Doc::hits` of the current hit.
    hit: Option<usize>,
    scroll_to_hit: bool,
}

impl ExtractedView {
    /// Draws the viewer for `file` of `case`, highlighting `query`.
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        case: &Arc<Case>,
        file: &FileRecord,
        query: Option<&TextQuery>,
    ) {
        let task = self.doc.get_or_insert_with(|| {
            let (case, file, query) = (Arc::clone(case), file.clone(), query.cloned());
            Task::spawn(ui.ctx(), move || {
                open(&case, &file, query.as_ref()).map(|d| d.map(Arc::new))
            })
        });
        let doc = match task.poll() {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                return;
            }
            Some(Ok(None)) => {
                ui.weak("No text available for this file.");
                return;
            }
            Some(Ok(Some(d))) => Arc::clone(d),
        };
        if self.page == 0 {
            // Open on the first hit, like Autopsy's "Search Results" source.
            self.hit = (!doc.hits.is_empty()).then_some(0);
            self.page = doc.hits.first().map_or(1, |(p, _)| *p);
            self.scroll_to_hit = self.hit.is_some();
        }
        self.toolbar(ui, &doc);
        ui.separator();
        let page = self.page;
        if self.page_task.as_ref().is_none_or(|(p, _)| *p != page) {
            let doc = Arc::clone(&doc);
            self.page_task = Some((
                page,
                Task::spawn(ui.ctx(), move || load_page(&doc.source, page)),
            ));
        }
        let Some((_, t)) = &mut self.page_task else {
            return;
        };
        let text = match t.poll() {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                return;
            }
            Some(Ok(None)) => {
                ui.weak("No text on this page.");
                return;
            }
            Some(Ok(Some(p))) => Arc::clone(p),
        };
        let on_page: Vec<(usize, Range<usize>)> = doc
            .hits
            .iter()
            .enumerate()
            .filter(|(_, (p, _))| *p == page)
            .map(|(i, (_, r))| (i, r.clone()))
            .collect();
        let job = highlight(
            &Look::of(ui),
            &text,
            &on_page,
            self.hit,
            ui.available_width(),
        );
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let (pos, galley, _) = egui::Label::new(job).wrap().layout_in_ui(ui);
                ui.painter()
                    .galley(pos, Arc::clone(&galley), ui.visuals().text_color());
                if self.scroll_to_hit
                    && let Some(r) = self
                        .hit
                        .and_then(|h| on_page.iter().find(|(i, _)| *i == h))
                        .map(|(_, r)| r)
                {
                    let chars = text.text.get(..r.start).map_or(0, |s| s.chars().count());
                    let rect = galley
                        .pos_from_cursor(egui::text::CCursor::new(chars))
                        .translate(pos.to_vec2());
                    ui.scroll_to_rect(rect, Some(egui::Align::Center));
                    self.scroll_to_hit = false;
                }
            });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, doc: &Doc) {
        ui.horizontal(|ui| {
            ui.label(format!("Page: {} of {}", self.page, doc.pages));
            if ui
                .add_enabled(self.page > 1, egui::Button::new("◀"))
                .on_hover_text("Previous page")
                .clicked()
            {
                self.page -= 1;
            }
            if ui
                .add_enabled(self.page < doc.pages, egui::Button::new("▶"))
                .on_hover_text("Next page")
                .clicked()
            {
                self.page += 1;
            }
            if doc.capped {
                ui.weak(format!("(first {MAX_EXTRACTED_PAGES} pages)"));
            }
            if doc.hits.is_empty() {
                return;
            }
            ui.separator();
            let on_page: Vec<usize> = (0..doc.hits.len())
                .filter(|&i| doc.hits.get(i).is_some_and(|(p, _)| *p == self.page))
                .collect();
            let current = self
                .hit
                .and_then(|h| on_page.iter().position(|&i| i == h))
                .map_or(0, |i| i + 1);
            ui.label(format!("Matches on page: {current} of {}", on_page.len()));
            let total = doc.hits.len();
            let goto = |this: &mut Self, i: usize| {
                this.hit = Some(i);
                this.page = doc.hits.get(i).map_or(this.page, |(p, _)| *p);
                this.scroll_to_hit = true;
            };
            let prev = self.hit.and_then(|h| h.checked_sub(1));
            if ui
                .add_enabled(prev.is_some(), egui::Button::new("⏶"))
                .on_hover_text("Previous match")
                .clicked()
                && let Some(i) = prev
            {
                goto(self, i);
            }
            let next = self.hit.map_or(0, |h| h + 1);
            if ui
                .add_enabled(next < total, egui::Button::new("⏷"))
                .on_hover_text("Next match")
                .clicked()
            {
                goto(self, next);
            }
            ui.weak(if total == 1 {
                "1 match in all".to_owned()
            } else {
                format!("{total} matches in all")
            });
        });
    }
}

/// Colours and font of the extracted text.
#[derive(Debug, Clone)]
struct Look {
    font: egui::FontId,
    normal: egui::Color32,
    dim: egui::Color32,
}

impl Look {
    fn of(ui: &egui::Ui) -> Self {
        Self {
            font: egui::TextStyle::Monospace.resolve(ui.style()),
            normal: ui.visuals().text_color(),
            dim: ui.visuals().weak_text_color(),
        }
    }

    fn push(
        &self,
        job: &mut egui::text::LayoutJob,
        s: &str,
        color: egui::Color32,
        bg: egui::Color32,
    ) {
        job.append(
            s,
            0.0,
            egui::TextFormat {
                font_id: self.font.clone(),
                color,
                background: bg,
                ..Default::default()
            },
        );
    }

    /// `text[from..to]` unhighlighted: the overlap after `base` dimmed.
    fn plain(&self, job: &mut egui::text::LayoutJob, page: &Page, from: usize, to: usize) {
        for (a, b, color) in [
            (from, to.min(page.base), self.normal),
            (from.max(page.base), to, self.dim),
        ] {
            if a < b
                && let Some(s) = page.text.get(a..b)
            {
                self.push(job, s, color, egui::Color32::TRANSPARENT);
            }
        }
    }
}

/// The page as a layout job: hits highlighted (the current one stronger),
/// the overlap that repeats on the next page dimmed. `hits` are
/// `(hit index, byte range)` in order.
fn highlight(
    look: &Look,
    page: &Page,
    hits: &[(usize, Range<usize>)],
    current: Option<usize>,
    width: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let mut at = 0;
    for (i, r) in hits {
        let Some(s) = page.text.get(r.clone()).filter(|_| r.start >= at) else {
            continue;
        };
        look.plain(&mut job, page, at, r.start);
        let bg = if Some(*i) == current {
            egui::Color32::from_rgb(0xFF, 0x9F, 0x1C)
        } else {
            egui::Color32::from_rgb(0xFF, 0xEB, 0x3B)
        };
        look.push(&mut job, s, egui::Color32::BLACK, bg);
        at = r.end;
    }
    look.plain(&mut job, page, at, page.text.len());
    job.wrap.max_width = width;
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_splits_hits_and_dims_the_overlap() {
        let look = Look {
            font: egui::FontId::monospace(12.0),
            normal: egui::Color32::WHITE,
            dim: egui::Color32::GRAY,
        };
        let page = Page {
            text: "abc hit def tail".into(),
            base: 12,
        };
        let job = highlight(&look, &page, &[(0, 4..7), (1, 99..120)], Some(0), 100.0);
        let parts: Vec<&str> = job
            .sections
            .iter()
            .map(|s| {
                job.text
                    .get(s.byte_range.start.0..s.byte_range.end.0)
                    .unwrap()
            })
            .collect();
        assert_eq!(
            parts,
            ["abc ", "hit", " def ", "tail"],
            "bad ranges skipped"
        );
        let colors: Vec<_> = job.sections.iter().map(|s| s.format.color).collect();
        assert_eq!(
            colors,
            [
                egui::Color32::WHITE,
                egui::Color32::BLACK,
                egui::Color32::WHITE,
                egui::Color32::GRAY
            ]
        );
    }
}
