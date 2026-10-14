//! Picture viewer: decoded off the UI thread, shown with fit/zoom.

use pm_core::SharedSource;

use crate::task::Task;

/// Pictures larger than this are not decoded (protects memory).
const MAX_PICTURE_BYTES: u64 = 128 * 1024 * 1024;

/// Recognises the picture formats we can decode from magic bytes.
pub(super) fn is_picture(head: &[u8]) -> bool {
    head.starts_with(&[0x89, b'P', b'N', b'G'])
        || head.starts_with(&[0xFF, 0xD8, 0xFF])
        || head.starts_with(b"GIF87a")
        || head.starts_with(b"GIF89a")
        || head.starts_with(b"BM")
        || (head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP"))
        || head.starts_with(b"II*\0")
        || head.starts_with(b"MM\0*")
        || head.starts_with(&[0, 0, 1, 0])
}

type Decoded = Result<egui::ColorImage, String>;

#[derive(Default)]
pub(crate) struct ImageView {
    task: Option<Task<Decoded>>,
    texture: Option<egui::TextureHandle>,
    fit: bool,
    zoom: f32,
}

fn decode(src: &SharedSource) -> Decoded {
    if src.size() > MAX_PICTURE_BYTES {
        return Err(format!(
            "picture larger than {}",
            pm_core::units::format_size(MAX_PICTURE_BYTES)
        ));
    }
    let bytes = src
        .read_vec_at(0, src.size() as usize)
        .map_err(|e| e.to_string())?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(1024 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

impl std::fmt::Debug for ImageView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageView")
            .field("loaded", &self.texture.is_some())
            .finish_non_exhaustive()
    }
}

impl ImageView {
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, src: &SharedSource, name: &str) {
        let task = self.task.get_or_insert_with(|| {
            let src = src.clone();
            Task::spawn(ui.ctx(), move || decode(&src))
        });
        let Some(result) = task.poll() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Decoding picture…");
            });
            return;
        };
        let image = match result {
            Ok(img) => img,
            Err(e) => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Cannot decode picture: {e}"),
                );
                return;
            }
        };
        if self.texture.is_none() {
            self.texture = Some(ui.ctx().load_texture(
                name,
                image.clone(),
                egui::TextureOptions::LINEAR,
            ));
            self.fit = true;
            self.zoom = 1.0;
        }
        let Some(tex) = &self.texture else { return };
        ui.horizontal(|ui| {
            ui.label(format!("{} × {} px", image.size[0], image.size[1]));
            ui.separator();
            ui.checkbox(&mut self.fit, "Fit to window");
            ui.add_enabled(
                !self.fit,
                egui::Slider::new(&mut self.zoom, 0.1..=8.0)
                    .logarithmic(true)
                    .text("Zoom"),
            );
            if ui.button("100 %").clicked() {
                self.fit = false;
                self.zoom = 1.0;
            }
        });
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            let native = tex.size_vec2();
            let size = if self.fit {
                let avail = ui.available_size();
                let scale = (avail.x / native.x).min(avail.y / native.y).min(1.0);
                native * scale
            } else {
                native * self.zoom
            };
            ui.image((tex.id(), size));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic() {
        assert!(is_picture(&[0x89, b'P', b'N', b'G', 0x0D]));
        assert!(is_picture(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!is_picture(b"RIFF\0\0\0\0WAVEfmt "));
        assert!(!is_picture(b"hello"));
    }

    #[test]
    fn decodes_png() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let src: SharedSource = std::sync::Arc::new(pm_core::io::MemSource::new(png));
        let img = decode(&src).unwrap();
        assert_eq!(img.size, [3, 2]);
        assert!(
            decode(
                &(std::sync::Arc::new(pm_core::io::MemSource::new(b"nope".to_vec()))
                    as SharedSource)
            )
            .is_err()
        );
    }
}
