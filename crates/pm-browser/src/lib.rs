//! Web browser artefacts for postmortem's Recent Activity module (ports
//! of Autopsy's `Chromium`, `Firefox`, `ExtractIE`, `ExtractEdge`,
//! `ExtractSafari`, `BinaryCookieReader`, `ChromeCacheExtractor`,
//! `SearchEngineURLQueryAnalyzer`, `ExtractWebAccountType` and
//! `DomainCategoryRunner` logic, without the case database).
//!
//! * Store readers: [`chromium`], [`firefox`], [`safari`], [`ie`],
//!   [`index_dat`] (MSIECF), [`binarycookies`], [`chrome_cache`]
//!   (blockfile cache). SQLite stores are read with [`sqlite::open`],
//!   which hardens the connection against hostile files.
//! * URL analytics: [`domain`] (Autopsy's `extractDomain`),
//!   [`search_engines`], [`account_type`], [`categories`].
//!
//! Binary parsers are bounds-checked and never panic on malformed input.

pub mod account_type;
pub mod binarycookies;
pub mod categories;
pub mod chrome_cache;
pub mod chromium;
pub mod domain;
pub mod firefox;
pub mod ie;
pub mod index_dat;
pub mod safari;
pub mod search_engines;
pub mod sqlite;
pub mod time;
