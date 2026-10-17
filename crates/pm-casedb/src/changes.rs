//! The case change bus (Autopsy `Case.Events`, in-process): every committed
//! write bumps a generation counter per domain (files, artifacts, tags, …),
//! derived from SQLite's update hook on the writer connection, so views
//! refresh when — and only when — what they show has changed.
//!
//! Rows changed by a transaction are collected as pending domains and
//! published on commit (the commit hook); a rollback drops them. Changes
//! made by other processes (e.g. `postmortem mcp --allow-writes`) are not
//! seen.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use rusqlite::Connection;
use rusqlite::hooks::Action;

/// What changed, by the tables a domain covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Domain {
    /// `files` and file attributes (new files, hashes, MIME types).
    Files,
    /// Data artifacts and analysis results with their attributes.
    Artifacts,
    /// Tag names, tag sets and file/artifact tags.
    Tags,
    /// Aggregate scores.
    Scores,
    /// Timeline events.
    Events,
    /// Data sources, volume systems, volumes, file systems.
    DataSources,
    /// Hosts, persons, host addresses.
    Hosts,
    /// OS accounts, realms and instances.
    OsAccounts,
    /// Communication accounts and relationships.
    Accounts,
    /// Generated reports.
    Reports,
    /// Ingest job records.
    IngestJobs,
}

impl Domain {
    pub const ALL: [Self; 11] = [
        Self::Files,
        Self::Artifacts,
        Self::Tags,
        Self::Scores,
        Self::Events,
        Self::DataSources,
        Self::Hosts,
        Self::OsAccounts,
        Self::Accounts,
        Self::Reports,
        Self::IngestJobs,
    ];

    fn bit(self) -> u32 {
        1 << (self as u32)
    }

    /// The domain a table belongs to.
    pub fn of_table(table: &str) -> Option<Self> {
        Some(match table {
            "files" | "file_attributes" => Self::Files,
            "artifacts" | "attributes" => Self::Artifacts,
            "tag_names" | "tag_sets" | "file_tags" | "artifact_tags" => Self::Tags,
            "scores" => Self::Scores,
            "events" | "event_descriptions" => Self::Events,
            "data_sources" | "volume_systems" | "volumes" | "file_systems" => Self::DataSources,
            "hosts"
            | "persons"
            | "host_addresses"
            | "host_address_map"
            | "host_address_dns_ip_map"
            | "host_address_usage" => Self::Hosts,
            "os_accounts"
            | "os_account_realms"
            | "os_account_instances"
            | "os_account_attributes" => Self::OsAccounts,
            "accounts" | "account_types" | "account_relationships" => Self::Accounts,
            "reports" => Self::Reports,
            "ingest_jobs" | "ingest_job_modules" => Self::IngestJobs,
            _ => return None,
        })
    }
}

/// Generation counters of a case database.
#[derive(Debug, Default)]
pub struct Changes {
    generations: [AtomicU64; Domain::ALL.len()],
    /// Domains touched by the open transaction.
    pending: AtomicU32,
}

impl Changes {
    /// Increases whenever a committed write changed `domain`.
    pub fn generation(&self, domain: Domain) -> u64 {
        self.generations
            .get(domain as usize)
            .map_or(0, |g| g.load(Ordering::Acquire))
    }

    /// Sum over `domains`: a single number that moves when any of them
    /// changes.
    pub fn of(&self, domains: &[Domain]) -> u64 {
        domains.iter().map(|&d| self.generation(d)).sum()
    }

    fn publish(&self) {
        let bits = self.pending.swap(0, Ordering::AcqRel);
        for d in Domain::ALL {
            if bits & d.bit() != 0
                && let Some(g) = self.generations.get(d as usize)
            {
                g.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    /// Installs the hooks on the writer connection.
    pub(crate) fn install(self: &Arc<Self>, conn: &Connection) -> rusqlite::Result<()> {
        let on_change = Arc::clone(self);
        conn.update_hook(Some(move |_: Action, _db: &str, table: &str, _row: i64| {
            if let Some(d) = Domain::of_table(table) {
                on_change.pending.fetch_or(d.bit(), Ordering::AcqRel);
            }
        }))?;
        let on_commit = Arc::clone(self);
        conn.commit_hook(Some(move || {
            on_commit.publish();
            // false: let the commit proceed.
            false
        }))?;
        let on_rollback = Arc::clone(self);
        conn.rollback_hook(Some(move || {
            on_rollback.pending.store(0, Ordering::Release);
        }))?;
        Ok(())
    }
}
