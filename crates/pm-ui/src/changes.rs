//! Case change bus → view refreshes (ADR-023): what each part of the
//! window shows is refreshed when the case database reports a committed
//! change in the domains it reads, instead of by hand-written calls after
//! each action.
//!
//! Without a running ingest every change refreshes at once (a tag, a
//! report, the end of an analysis). While an ingest runs, counts refresh
//! at most every [`LIVE_REFRESH`] and tables and listings reload once the
//! writes have stopped for that long, so they do not flicker.

use std::time::{Duration, Instant};

use pm_casedb::changes::{Changes, Domain};

/// Throttle while writes keep arriving.
pub(crate) const LIVE_REFRESH: Duration = Duration::from_secs(2);

/// Groups of domains that drive one kind of refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Watch {
    /// File rows: listings, views, hashes and MIME types in the viewer.
    Files,
    /// Data artifacts and analysis results: the results tree and tables.
    Results,
    /// Tags and scores: the S/C columns, Tags and Score nodes.
    Annotations,
    /// Data sources and hosts: the Data Sources tree.
    Sources,
    /// Reports node.
    Reports,
    /// Timeline events (artifact events and file times).
    Timeline,
    /// The examiner's notes.
    Notes,
}

impl Watch {
    pub(crate) const ALL: [Self; 7] = [
        Self::Files,
        Self::Results,
        Self::Annotations,
        Self::Sources,
        Self::Reports,
        Self::Timeline,
        Self::Notes,
    ];

    fn domains(self) -> &'static [Domain] {
        match self {
            Self::Files => &[Domain::Files],
            Self::Results => &[Domain::Artifacts],
            Self::Annotations => &[Domain::Tags, Domain::Scores],
            Self::Sources => &[Domain::DataSources, Domain::Hosts],
            Self::Reports => &[Domain::Reports],
            Self::Timeline => &[Domain::Events, Domain::Files],
            Self::Notes => &[Domain::Notes],
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct State {
    /// Generation last acted on.
    seen: u64,
    /// Generation observed most recently and when it last moved.
    latest: u64,
    moved_at: Instant,
    refreshed_at: Instant,
}

/// What changed since the last frame, per watch.
#[derive(Debug, Default)]
pub(crate) struct ChangeWatch {
    states: Vec<(Watch, State)>,
}

/// The refreshes due this frame.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Due {
    /// Counts and other cheap summaries may reload.
    pub(crate) counts: bool,
    /// Tables and listings may reload (writes have settled).
    pub(crate) rows: bool,
}

impl ChangeWatch {
    /// Forgets everything (another case was opened).
    pub(crate) fn reset(&mut self) {
        self.states.clear();
    }

    /// Checks `watch` against the case's generations; `live` while an
    /// ingest is writing.
    pub(crate) fn poll(&mut self, changes: &Changes, watch: Watch, live: bool) -> Due {
        let now = Instant::now();
        let generation = changes.of(watch.domains());
        let Some(state) = self
            .states
            .iter_mut()
            .find(|(w, _)| *w == watch)
            .map(|(_, s)| s)
        else {
            self.states.push((
                watch,
                State {
                    seen: generation,
                    latest: generation,
                    moved_at: now,
                    refreshed_at: now,
                },
            ));
            return Due::default();
        };
        if generation != state.latest {
            state.latest = generation;
            state.moved_at = now;
        }
        if state.seen == state.latest {
            return Due::default();
        }
        if !live {
            state.seen = state.latest;
            state.refreshed_at = now;
            return Due {
                counts: true,
                rows: true,
            };
        }
        let settled = now.duration_since(state.moved_at) >= LIVE_REFRESH;
        let counts = settled || now.duration_since(state.refreshed_at) >= LIVE_REFRESH;
        if !counts {
            return Due::default();
        }
        state.refreshed_at = now;
        if settled {
            state.seen = state.latest;
        }
        Due {
            counts: true,
            rows: settled,
        }
    }

    /// The soonest moment a pending change becomes due (for repaints).
    pub(crate) fn pending(&self) -> bool {
        self.states.iter().any(|(_, s)| s.seen != s.latest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_generations_are_quiet() {
        let changes = Changes::default();
        let mut w = ChangeWatch::default();
        let d = w.poll(&changes, Watch::Files, false);
        assert!(!d.counts && !d.rows, "first sight records the baseline");
        let d = w.poll(&changes, Watch::Files, true);
        assert!(!d.counts && !d.rows);
        assert!(!w.pending());
    }
}
