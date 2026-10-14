//! Thumbnail cache shared by the Image Gallery and Discovery: decodes in the
//! background on first request and keeps GPU textures for reuse.

use std::collections::HashMap;
use std::hash::Hash;

use egui::{Color32, Rect};

use crate::model::Content;
use crate::task::Task;

/// Pictures larger than this are not thumbnailed.
const MAX_THUMB_SOURCE: u64 = 64 * 1024 * 1024;
pub(crate) const THUMB_PX: u32 = 192;

pub(crate) enum Thumb {
    Loading(Task<Option<egui::ColorImage>>),
    Ready(egui::TextureHandle),
    Failed,
}

/// Decodes a picture into a ≤ `THUMB_PX` thumbnail.
pub(crate) fn decode_thumb(src: &pm_core::SharedSource) -> Option<egui::ColorImage> {
    if src.size() > MAX_THUMB_SOURCE {
        return None;
    }
    let bytes = src.read_vec_at(0, src.size() as usize).ok()?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    reader.limits(limits);
    let thumb = reader
        .decode()
        .ok()?
        .thumbnail(THUMB_PX, THUMB_PX)
        .to_rgba8();
    let size = [thumb.width() as usize, thumb.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(
        size,
        thumb.as_raw(),
    ))
}

/// Cache of thumbnails keyed by `K`.
pub(crate) struct ThumbCache<K> {
    map: HashMap<K, Thumb>,
}

impl<K> Default for ThumbCache<K> {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
        }
    }
}

impl<K: Hash + Eq + Clone + std::fmt::Debug> ThumbCache<K> {
    pub(crate) fn clear(&mut self) {
        self.map.clear();
    }

    #[cfg(test)]
    pub(crate) fn any_ready(&self) -> bool {
        self.map.values().any(|t| matches!(t, Thumb::Ready(_)))
    }

    /// Paints the thumbnail for `key` into `rect`, starting a decode on first
    /// use. `content` is only called when a decode must start.
    pub(crate) fn paint(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        key: &K,
        content: impl FnOnce() -> Option<Content>,
    ) {
        if !self.map.contains_key(key) {
            let c = content();
            let task = Task::spawn(ui.ctx(), move || {
                c.and_then(|c| c.source().ok())
                    .and_then(|s| decode_thumb(&s))
            });
            self.map.insert(key.clone(), Thumb::Loading(task));
        }
        let painter = ui.painter_at(rect);
        let weak = ui.visuals().weak_text_color();
        let warn = ui.visuals().warn_fg_color;
        let Some(t) = self.map.get_mut(key) else {
            return;
        };
        if let Thumb::Loading(task) = t
            && let Some(result) = task.poll()
        {
            *t = match result {
                Some(img) => Thumb::Ready(ui.ctx().load_texture(
                    format!("thumb-{key:?}"),
                    img.clone(),
                    egui::TextureOptions::LINEAR,
                )),
                None => Thumb::Failed,
            };
        }
        match t {
            Thumb::Ready(tex) => {
                let size = tex.size_vec2();
                let scale = (rect.width() / size.x).min(rect.height() / size.y);
                let r = Rect::from_center_size(rect.center(), size * scale);
                painter.image(
                    tex.id(),
                    r,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            Thumb::Loading(_) => {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "…",
                    egui::FontId::proportional(20.0),
                    weak,
                );
            }
            Thumb::Failed => {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "⚠ no preview",
                    egui::FontId::proportional(12.0),
                    warn,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn thumbnails_are_bounded() {
        let bmp = pm_testkit::demo::gradient_bmp(640, 320);
        let src: pm_core::SharedSource = Arc::new(pm_core::io::MemSource::new(bmp));
        let t = decode_thumb(&src).unwrap();
        assert_eq!(t.size, [192, 96]);
        let junk: pm_core::SharedSource =
            Arc::new(pm_core::io::MemSource::new(b"not a picture".to_vec()));
        assert!(decode_thumb(&junk).is_none());
    }
}
