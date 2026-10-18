//! Time-zone check after analysis (owner decision in `DIRECTION.md`): when
//! Recent Activity finds that a data source's system ran in a zone other
//! than the one the data source was added with, the examiner is told and
//! offered to switch. Switching re-reads the FAT and exFAT times (the only
//! ones stored in local time); keeping it is remembered in the case.

use std::sync::Arc;

use pm_case::{Case, ZoneMismatch};

use crate::task::{Cached, Task};

/// Pending warnings and a running switch.
#[derive(Debug, Default)]
pub(crate) struct TimeZoneCheck {
    found: Cached<Vec<ZoneMismatch>>,
    switching: Option<(String, Task<Result<u64, String>>)>,
    error: Option<String>,
}

impl TimeZoneCheck {
    /// Looks again (new results, data sources or a different case).
    pub(crate) fn refresh(&mut self) {
        self.found.refresh();
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Shows the first warning, unless `busy` (an ingest or another
    /// dialog is running).
    pub(crate) fn ui(&mut self, ctx: &egui::Context, case: &Arc<Case>, busy: bool) {
        if let Some((zone, task)) = &mut self.switching {
            match task.poll() {
                None => {
                    let zone = zone.clone();
                    egui::Modal::new(egui::Id::new("tz-switching")).show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Re-reading FAT and exFAT times in {zone}…"));
                        });
                    });
                    return;
                }
                Some(r) => {
                    if let Err(e) = r {
                        self.error = Some(e.clone());
                    }
                    self.switching = None;
                    self.found.refresh();
                }
            }
        }
        if let Some(e) = self.error.clone() {
            let modal = egui::Modal::new(egui::Id::new("tz-error")).show(ctx, |ui| {
                ui.heading("Time zone not changed");
                ui.label(&e);
                ui.button("OK").clicked()
            });
            if modal.inner || modal.should_close() {
                self.error = None;
            }
            return;
        }
        let load_case = Arc::clone(case);
        let Some(found) = self.found.get(ctx, move || {
            let mut out = Vec::new();
            for ds in load_case.db().data_sources().map_err(|e| e.to_string())? {
                if let Some(m) = load_case
                    .time_zone_mismatch(ds.id)
                    .map_err(|e| e.to_string())?
                {
                    out.push(m);
                }
            }
            Ok(out)
        }) else {
            return;
        };
        let Some(m) = found.first() else {
            return;
        };
        if busy {
            return;
        }
        let mut choice = None;
        egui::Modal::new(egui::Id::new("tz-mismatch")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading("Time zone differs");
            ui.label(format!(
                "Recent Activity found that the system on {} was set to {} ({}). \
                 The data source was added with the time zone {}.",
                m.ds_name, m.system, m.suggested, m.current
            ));
            ui.label(
                "FAT and exFAT store local times, so those times are only right in the \
                 system's zone. Other file systems and results store UTC and do not change.",
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(format!("Switch to {}", m.suggested)).clicked() {
                    choice = Some(true);
                }
                if ui.button(format!("Keep {}", m.current)).clicked() {
                    choice = Some(false);
                }
            });
        });
        match choice {
            Some(true) => {
                let (case, ds, zone) = (Arc::clone(case), m.ds_id, m.suggested.clone());
                let shown = zone.clone();
                self.switching = Some((
                    shown,
                    Task::spawn(ctx, move || {
                        let n = case
                            .set_data_source_timezone(ds, &zone)
                            .map_err(|e| e.to_string())?;
                        case.mark_time_zone_reviewed(ds)
                            .map_err(|e| e.to_string())?;
                        Ok(n)
                    }),
                ));
            }
            Some(false) => {
                if let Err(e) = case.mark_time_zone_reviewed(m.ds_id) {
                    self.error = Some(e.to_string());
                }
                // Drop the answered warning now; a refresh would keep
                // showing it until the reload lands.
                self.found.clear();
            }
            None => {}
        }
    }
}
