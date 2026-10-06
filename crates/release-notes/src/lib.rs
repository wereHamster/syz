//! `release-notes`: forge-agnostic release notes / changelog fetching for
//! package repositories.
//!
//! Given a package name, a version, and the repository the package is
//! published from, resolves the release notes (changelog fragment, release
//! body) for that version — first from the forge's release bodies, then
//! from changelog files in the repository's default branch — and
//! post-processes the markdown for embedding in a GitHub PR body.
//!
//! The crate is transport-injected ([`HttpClient`]), forge-agnostic
//! ([`Forge`]; GitHub is the first implementation), and returns typed
//! results ([`ReleaseNotes`], [`Source`]) with typed errors ([`Error`]).
//! Absence of notes is a normal outcome: `Ok(None)`.

pub mod forge;
pub mod github;
pub mod http;
pub mod markdown;
#[cfg(feature = "test-util")]
pub mod mock;
mod resolver;
mod types;

pub use forge::Forge;
pub use http::{Headers, HttpClient, HttpResponse, TransportError};
pub use resolver::{Options, Request, Resolver, Strategy};
pub use types::{
    base_version, candidate_tags, fallback_tag, Candidate, Error, PackageName, ReleaseInfo,
    ReleaseNotes, Repository, Result, Source, Tag, TagShape,
};

#[cfg(feature = "test-util")]
pub use mock::MockForge;
