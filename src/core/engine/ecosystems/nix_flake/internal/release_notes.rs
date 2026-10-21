//! What a nixpkgs update adds to the release notes of NixOS.
//!
//! The notes of the release in development are one file in nixpkgs, which keeps being edited
//! (`nixos/doc/manual/release-notes/rl-2611.section.md` for 26.11). The commits that touched it
//! between the two revisions are the notable changes. They are listed through the commits API
//! with a path filter: the comparison API caps the files it lists at 300, which a range of
//! nixpkgs easily exceeds.

use chrono::{DateTime, Utc};

use crate::core::clients::github::GitHub;

use super::lock_diff::Pin;

const OWNER: &str = "NixOS";
const REPO: &str = "nixpkgs";

/// How many commits are asked for. More than this is reported as such.
const PAGE: usize = 30;

/// How long the title of a commit may get.
const TITLE_LIMIT: usize = 100;

/// A commit that touched the release notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteCommit {
    /// The first line of its message.
    pub title: String,
    pub url: String,
}

/// The changes to the release notes between two revisions of nixpkgs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseNotes {
    /// The release the notes are of, such as `26.11`.
    pub release: String,
    /// The notes at the new revision.
    pub url: String,
    /// The path of the notes in nixpkgs.
    pub file: String,
    /// The commits that touched them, the oldest first.
    pub commits: Vec<NoteCommit>,
    /// Whether there are more commits than are listed.
    pub truncated: bool,
}

/// Whether `pin` is a revision of nixpkgs.
pub fn is_nixpkgs(pin: &Pin) -> bool {
    pin.github_repo() == Some((OWNER, REPO))
}

/// The path of the release notes of `release` (`26.11`).
fn notes_file(release: &str) -> Option<String> {
    let (year, month) = release.split_once('.')?;
    let is_number = |part: &str| part.len() == 2 && part.bytes().all(|b| b.is_ascii_digit());

    (is_number(year) && is_number(month))
        .then(|| format!("nixos/doc/manual/release-notes/rl-{year}{month}.section.md"))
}

/// The release a revision of nixpkgs is of, as its `.version` says.
async fn release_of(github: &GitHub, rev: &str) -> Option<String> {
    let version = github
        .get_foreign_file(OWNER, REPO, ".version", Some(rev))
        .await
        .ok()??;
    Some(version.trim().to_string()).filter(|release| notes_file(release).is_some())
}

/// The route that lists the commits that touched `file` up to `new_rev`, committed after
/// `since` and up to `until`.
fn commits_route(file: &str, new_rev: &str, since: DateTime<Utc>, until: DateTime<Utc>) -> String {
    let time = |time: DateTime<Utc>| time.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    format!(
        "/repos/{OWNER}/{REPO}/commits?sha={}&path={}&since={}&until={}&per_page={PAGE}",
        urlencoding::encode(new_rev),
        urlencoding::encode(file),
        time(since),
        time(until),
    )
}

/// The first line of a commit message, no longer than a title should be.
fn title_of(message: &str) -> String {
    let line = message.lines().next().unwrap_or_default().trim();

    match line.char_indices().nth(TITLE_LIMIT) {
        Some((end, _)) => format!("{}…", line[..end].trim_end()),
        None => line.to_string(),
    }
}

/// The commits in the answer of the commits API, the oldest first. Commits without a message
/// or a revision that can be put in a link are left out.
fn parse_commits(answer: &serde_json::Value) -> Vec<NoteCommit> {
    let mut commits: Vec<NoteCommit> = answer
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|commit| {
            let sha = commit.get("sha")?.as_str()?;
            if sha.is_empty() || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let title = title_of(commit.get("commit")?.get("message")?.as_str()?);
            (!title.is_empty()).then(|| NoteCommit {
                title,
                url: format!("https://github.com/{OWNER}/{REPO}/commit/{sha}"),
            })
        })
        .collect();

    // The API lists the newest first.
    commits.reverse();
    commits
}

/// What the update from `old` to `new` adds to the release notes. `None` when the revisions
/// are of different releases, which are not comparable, the notes can't be found, or nothing
/// touched them.
pub async fn between(github: &GitHub, old: &Pin, new: &Pin) -> Option<ReleaseNotes> {
    let (old_rev, new_rev) = (old.rev()?, new.rev()?);
    let (since, until) = (
        DateTime::from_timestamp(old.last_modified?.checked_add(1)?, 0)?,
        DateTime::from_timestamp(new.last_modified?, 0)?,
    );
    if since > until {
        return None;
    }

    let (old_release, new_release) =
        futures::future::join(release_of(github, old_rev), release_of(github, new_rev)).await;
    let release = new_release.filter(|release| old_release.as_ref() == Some(release))?;
    let file = notes_file(&release)?;

    let answer = github
        .get_json(&commits_route(&file, new_rev, since, until))
        .await
        .ok()?;
    let commits = parse_commits(&answer);
    if commits.is_empty() {
        return None;
    }

    Some(ReleaseNotes {
        url: format!("https://github.com/{OWNER}/{REPO}/blob/{new_rev}/{file}"),
        truncated: answer.as_array().is_some_and(|list| list.len() >= PAGE),
        release,
        file,
        commits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_have_their_notes_in_a_file_of_their_own() {
        assert_eq!(
            notes_file("26.11").as_deref(),
            Some("nixos/doc/manual/release-notes/rl-2611.section.md")
        );
        assert_eq!(notes_file("unstable"), None);
        assert_eq!(notes_file("26.1"), None);
        assert_eq!(notes_file("../../26.11"), None);
    }

    #[test]
    fn the_route_filters_by_path_and_time() {
        let since = DateTime::from_timestamp(1_760_000_000, 0).unwrap();
        let until = DateTime::from_timestamp(1_760_100_000, 0).unwrap();

        assert_eq!(
            commits_route("nixos/doc/rl-2611.section.md", "abc123", since, until),
            "/repos/NixOS/nixpkgs/commits?sha=abc123&path=nixos%2Fdoc%2Frl-2611.section.md\
             &since=2025-10-09T08:53:20Z&until=2025-10-10T12:40:00Z&per_page=30"
        );
    }

    #[test]
    fn commits_are_listed_oldest_first_by_the_first_line_of_their_message() {
        let answer = serde_json::json!([
            { "sha": "bbb222", "commit": { "message": "nixos/foo: drop bar\n\nBecause." } },
            { "sha": "aaa111", "commit": { "message": "nixos/foo: add bar" } },
            { "sha": "not a sha", "commit": { "message": "ignored" } },
            { "sha": "ccc333", "commit": { "message": "" } },
        ]);

        assert_eq!(
            parse_commits(&answer),
            vec![
                NoteCommit {
                    title: "nixos/foo: add bar".to_string(),
                    url: "https://github.com/NixOS/nixpkgs/commit/aaa111".to_string(),
                },
                NoteCommit {
                    title: "nixos/foo: drop bar".to_string(),
                    url: "https://github.com/NixOS/nixpkgs/commit/bbb222".to_string(),
                },
            ]
        );
        assert!(parse_commits(&serde_json::json!({ "message": "Not Found" })).is_empty());
    }

    #[test]
    fn long_titles_are_cut_at_a_character() {
        let long = "é".repeat(TITLE_LIMIT + 20);
        let title = title_of(&long);

        assert_eq!(title.chars().count(), TITLE_LIMIT + 1);
        assert!(title.ends_with('…'));
        assert_eq!(title_of("short"), "short");
    }
}
