//! The system section of the flake pull request: what the update does to the NixOS and darwin
//! hosts the flake describes, and what it adds to the release notes of NixOS. It is left out
//! for a flake without hosts, and when no host changes.

use anyhow::Result;
use async_trait::async_trait;

use super::nix_flake::{code, plural, Locks};
use crate::core::engine::ecosystems::nix_flake::internal::lock_diff::{ChangeKind, Pin};
use crate::core::engine::ecosystems::nix_flake::internal::release_notes::{self, ReleaseNotes};
use crate::core::engine::ecosystems::nix_flake::internal::system_facts::{
    self, HostChange, HostKind,
};
use crate::core::engine::pull_request_generator::budget::{fit_items, SectionKind};
use crate::core::engine::pull_request_generator::context::PullRequestGenerationContext;
use crate::core::engine::pull_request_generator::sections::PullRequestSectionGenerator;

/// How many services of a host are named, of those added and of those removed.
const SERVICES_SHOWN: usize = 5;

/// How many updates of nixpkgs get their release notes looked up.
const NOTES_SHOWN: usize = 2;

/// Says what the update does to the hosts of the flake.
pub struct NixFlakeSystemSection;

#[async_trait]
impl PullRequestSectionGenerator for NixFlakeSystemSection {
    fn name(&self) -> &'static str {
        "nix-flake system"
    }

    fn kind(&self) -> SectionKind {
        SectionKind::System
    }

    async fn generate(
        &self,
        ctx: &PullRequestGenerationContext<'_>,
        budget: usize,
    ) -> Result<Option<String>> {
        let (Some(github), Some(work_dir)) = (ctx.github, ctx.work_dir) else {
            return Ok(None);
        };

        let hosts = system_facts::analyze(github, ctx.snapshot, work_dir, ctx.modifications)
            .await?
            .unwrap_or_default();
        if hosts.is_empty() {
            return Ok(None);
        }

        let notes = if hosts.iter().any(|host| host.kind == HostKind::Nixos) {
            release_notes_of(ctx, github, &hosts).await
        } else {
            Vec::new()
        };

        Ok(render(&hosts, &notes, budget))
    }
}

/// The nixpkgs updates of the lock, as pairs of the pin before and after.
fn nixpkgs_updates(locks: &Locks) -> Vec<(Pin, Pin)> {
    let Some(diff) = locks.diff() else {
        return Vec::new();
    };

    let mut updates: Vec<(Pin, Pin)> = Vec::new();
    for change in diff.direct.iter().chain(&diff.indirect) {
        let ChangeKind::Updated { old, new } = &change.kind else {
            continue;
        };
        let known = updates.iter().any(|(known_old, known_new)| {
            known_old.rev() == old.rev() && known_new.rev() == new.rev()
        });
        if release_notes::is_nixpkgs(old) && release_notes::is_nixpkgs(new) && !known {
            updates.push((old.clone(), new.clone()));
        }
    }

    updates.truncate(NOTES_SHOWN);
    updates
}

/// What the nixpkgs updates of the lock add to the release notes of the releases the changed
/// hosts are on, where those are known.
async fn release_notes_of(
    ctx: &PullRequestGenerationContext<'_>,
    github: &crate::core::clients::github::GitHub,
    hosts: &[HostChange],
) -> Vec<ReleaseNotes> {
    let Some(locks) = Locks::read(ctx.snapshot, ctx.modifications).await else {
        return Vec::new();
    };
    let releases: Vec<&str> = hosts
        .iter()
        .filter(|host| host.kind == HostKind::Nixos)
        .filter_map(|host| host.after.release.as_deref())
        .collect();

    futures::future::join_all(
        nixpkgs_updates(&locks)
            .iter()
            .map(|(old, new)| release_notes::between(github, old, new)),
    )
    .await
    .into_iter()
    .flatten()
    .filter(|notes| releases.is_empty() || releases.contains(&notes.release.as_str()))
    .collect()
}

/// `before → after` for a fact that differs, the value for one that doesn't, `None` for one
/// that isn't known.
fn fact(before: &Option<String>, after: &Option<String>) -> Option<String> {
    match (before, after) {
        (Some(before), Some(after)) if before != after => {
            Some(format!("{} → {}", code(before), code(after)))
        }
        (_, Some(value)) | (Some(value), None) => Some(code(value)),
        (None, None) => None,
    }
}

/// How a package differs, or `None` when it doesn't.
fn package_change(before: Option<&str>, after: Option<&str>) -> Option<String> {
    match (before, after) {
        (Some(before), Some(after)) if before != after => {
            Some(format!("{} → {}", code(before), code(after)))
        }
        (None, Some(after)) => Some(format!("{} (new)", code(after))),
        (Some(before), None) => Some(format!("{} (gone)", code(before))),
        _ => None,
    }
}

/// `a`, `b` and `c`.
fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [only] => only.to_string(),
        [rest @ .., last] => format!("{} and {}", rest.join(", "), last),
    }
}

/// The names of `services`, the first few of them.
fn some_services(services: &[&str]) -> String {
    let mut shown: Vec<String> = services
        .iter()
        .take(SERVICES_SHOWN)
        .map(|service| code(service))
        .collect();
    if services.len() > SERVICES_SHOWN {
        shown.push(format!("{} more", services.len() - SERVICES_SHOWN));
    }
    shown.join(", ")
}

/// The bullet of a host with what changes about it below.
fn host_block(host: &HostChange) -> String {
    let (before, after) = (&host.before, &host.after);

    let mut facts: Vec<String> = Vec::new();
    match host.kind {
        HostKind::Nixos => facts.push(match fact(&before.release, &after.release) {
            Some(release) => format!("NixOS {release}"),
            None => "NixOS".to_string(),
        }),
        HostKind::Darwin => facts.push("darwin".to_string()),
    }
    if let Some(kernel) = fact(&before.kernel, &after.kernel) {
        facts.push(format!("kernel {kernel}"));
    }
    if let Some(state_version) = fact(&before.state_version, &after.state_version) {
        facts.push(format!("`system.stateVersion` {state_version}"));
    }

    let mut block = format!("- **{}**: {}\n", code(&host.name), facts.join(", "));

    let (added, removed) = host.services();
    let mut services: Vec<String> = Vec::new();
    if !added.is_empty() {
        services.push(format!("{} added ({})", added.len(), some_services(&added)));
    }
    if !removed.is_empty() {
        services.push(format!(
            "{} removed ({})",
            removed.len(),
            some_services(&removed)
        ));
    }
    if !services.is_empty() {
        block.push_str(&format!("  - Services: {}\n", services.join(", ")));
    }

    let packages: Vec<String> = before
        .packages
        .versions()
        .into_iter()
        .zip(after.packages.versions())
        .filter_map(|((name, before), (_, after))| {
            Some(format!("{} {}", code(name), package_change(before, after)?))
        })
        .collect();
    if !packages.is_empty() {
        block.push_str(&format!("  - Packages: {}\n", packages.join(", ")));
    }

    let reasons: Vec<&str> = host
        .reboot_reasons()
        .into_iter()
        .map(|part| part.label())
        .collect();
    if !reasons.is_empty() {
        block.push_str(&format!(
            "  - **Reboot recommended** (best effort): the {} changed\n",
            and_list(&reasons)
        ));
    }

    block
}

/// Says how far to trust the reboot hint.
const REBOOT_NOTE: &str = "The reboot hint compares the initrd, kernel and kernel modules of the new system with the old one, as `system.autoUpgrade.allowReboot` does. It doesn't see firmware or microcode, or processes that still use old libraries.\n\n";

/// The notes of one release as a heading and a list of the commits that touched them, no
/// longer than `limit`.
fn render_notes(notes: &ReleaseNotes, limit: usize) -> Option<String> {
    let count = if notes.truncated {
        format!(
            "at least {}",
            plural(notes.commits.len(), "commit", "commits")
        )
    } else {
        plural(notes.commits.len(), "commit", "commits")
    };
    let prefix = format!(
        "**NixOS {} release notes** ([{}]({})) changed in {}:\n\n",
        notes.release,
        code(notes.file.rsplit('/').next().unwrap_or(&notes.file)),
        notes.url,
        count
    );
    let items: Vec<String> = notes
        .commits
        .iter()
        .map(|commit| format!("- [{}]({})\n", code(&commit.title), commit.url))
        .collect();

    fit_items(&prefix, &items, "\n", limit, |n| {
        format!("- … and {n} more\n")
    })
}

/// The section for the changed `hosts`, no longer than `budget`. The hosts are what the section
/// is about, so the release notes get a share of what is left, and the reboot note, which only
/// explains, goes first. `None` if not even one host fits.
fn render(hosts: &[HostChange], notes: &[ReleaseNotes], budget: usize) -> Option<String> {
    let head = "# System\n\n";
    let reboot_note = if hosts.iter().any(|host| !host.reboot_reasons().is_empty()) {
        REBOOT_NOTE
    } else {
        ""
    };

    let mut remaining = budget.checked_sub(head.len() + reboot_note.len())?;

    let share = remaining / 3;
    let mut notes_text = String::new();
    for notes in notes {
        let limit = share.saturating_sub(notes_text.len());
        if let Some(rendered) = render_notes(notes, limit) {
            notes_text.push_str(&rendered);
        }
    }
    remaining -= notes_text.len();

    let blocks: Vec<String> = hosts.iter().map(host_block).collect();
    let hosts_text = fit_items("", &blocks, "\n", remaining, |n| {
        format!(
            "- … and {n} more {}\n",
            if n == 1 { "host" } else { "hosts" }
        )
    })?;

    Some(format!("{head}{hosts_text}{notes_text}{reboot_note}")).filter(|text| text.len() <= budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::ecosystems::nix_flake::internal::release_notes::NoteCommit;
    use crate::core::engine::ecosystems::nix_flake::internal::system_facts::{
        BootInputs, HostFacts, Packages,
    };

    fn facts(release: &str, kernel: &str, services: &[&str]) -> HostFacts {
        HostFacts {
            release: Some(release.to_string()),
            kernel: Some(kernel.to_string()),
            state_version: Some("25.11".to_string()),
            services: services.iter().map(|s| s.to_string()).collect(),
            packages: Packages {
                nix: Some("2.31.1".to_string()),
                openssh: Some("10.0p2".to_string()),
                systemd: Some("257".to_string()),
            },
            boot: BootInputs {
                initrd: Some("initrd-a".to_string()),
                kernel: Some("kernel-a".to_string()),
                kernel_modules: Some("modules-a".to_string()),
            },
        }
    }

    fn host(name: &str, before: HostFacts, after: HostFacts) -> HostChange {
        HostChange {
            name: name.to_string(),
            kind: HostKind::Nixos,
            before,
            after,
        }
    }

    fn notes(commits: usize) -> ReleaseNotes {
        ReleaseNotes {
            release: "26.11".to_string(),
            url: "https://github.com/NixOS/nixpkgs/blob/abc/nixos/doc/manual/release-notes/rl-2611.section.md".to_string(),
            file: "nixos/doc/manual/release-notes/rl-2611.section.md".to_string(),
            commits: (0..commits)
                .map(|i| NoteCommit {
                    title: format!("nixos/service{i}: change the default"),
                    url: format!("https://github.com/NixOS/nixpkgs/commit/{i:040x}"),
                })
                .collect(),
            truncated: false,
        }
    }

    #[test]
    fn a_host_says_what_changes_about_it() {
        let before = facts("26.05", "6.12.1", &["cron.service", "sshd.service"]);
        let mut after = facts("26.11", "6.12.3", &["ntpd.service", "sshd.service"]);
        after.packages.nix = Some("2.32.0".to_string());
        after.boot.kernel = Some("kernel-b".to_string());
        after.boot.initrd = Some("initrd-b".to_string());

        let block = host_block(&host("web", before, after));

        assert_eq!(
            block,
            "- **`web`**: NixOS `26.05` → `26.11`, kernel `6.12.1` → `6.12.3`, `system.stateVersion` `25.11`\n\
             \x20 - Services: 1 added (`ntpd.service`), 1 removed (`cron.service`)\n\
             \x20 - Packages: `nix` `2.31.1` → `2.32.0`\n\
             \x20 - **Reboot recommended** (best effort): the initrd and kernel changed\n"
        );
    }

    #[test]
    fn unchanged_facts_are_shown_as_they_are_and_nothing_else() {
        let before = facts("26.05", "6.12.1", &[]);
        let mut after = before.clone();
        after.packages.openssh = Some("10.1p1".to_string());

        let block = host_block(&host("mail", before, after));

        assert_eq!(
            block,
            "- **`mail`**: NixOS `26.05`, kernel `6.12.1`, `system.stateVersion` `25.11`\n\
             \x20 - Packages: `openssh` `10.0p2` → `10.1p1`\n"
        );
    }

    #[test]
    fn many_services_are_counted_and_the_first_few_named() {
        let before = facts("26.05", "6.12.1", &[]);
        let names: Vec<String> = (0..8).map(|i| format!("s{i}.service")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let after = facts("26.05", "6.12.1", &refs);

        let block = host_block(&host("build", before, after));

        assert!(
            block.contains(
                "8 added (`s0.service`, `s1.service`, `s2.service`, `s3.service`, `s4.service`, 3 more)"
            ),
            "{block}"
        );
    }

    #[test]
    fn a_darwin_host_has_no_release_kernel_or_reboot() {
        let before = HostFacts {
            state_version: Some("6".to_string()),
            services: vec!["nix-daemon".to_string()],
            packages: Packages {
                nix: Some("2.31.1".to_string()),
                ..Packages::default()
            },
            ..HostFacts::default()
        };
        let mut after = before.clone();
        after.packages.nix = Some("2.32.0".to_string());

        let mut change = host("laptop", before, after);
        change.kind = HostKind::Darwin;

        assert_eq!(
            host_block(&change),
            "- **`laptop`**: darwin, `system.stateVersion` `6`\n\
             \x20 - Packages: `nix` `2.31.1` → `2.32.0`\n"
        );
    }

    #[test]
    fn the_section_has_the_hosts_the_notes_and_the_reboot_note() {
        let before = facts("26.11", "6.12.1", &[]);
        let mut after = before.clone();
        after.boot.kernel = Some("kernel-b".to_string());

        let section = render(&[host("web", before, after)], &[notes(2)], 4_000).unwrap();

        assert!(section.starts_with("# System\n\n- **`web`**"), "{section}");
        assert!(
            section.contains("**NixOS 26.11 release notes** ([`rl-2611.section.md`](https://github.com/NixOS/nixpkgs/blob/abc/nixos/doc/manual/release-notes/rl-2611.section.md)) changed in 2 commits:"),
            "{section}"
        );
        assert!(
            section.contains(
                "- [`nixos/service0: change the default`](https://github.com/NixOS/nixpkgs/commit/"
            ),
            "{section}"
        );
        assert!(section.ends_with(REBOOT_NOTE), "{section}");
    }

    #[test]
    fn without_a_reboot_there_is_no_note_about_it() {
        let before = facts("26.05", "6.12.1", &[]);
        let mut after = before.clone();
        after.packages.nix = Some("2.32.0".to_string());

        let section = render(&[host("web", before, after)], &[], 4_000).unwrap();

        assert!(!section.contains("reboot"), "{section}");
        assert!(!section.contains("Reboot"), "{section}");
    }

    #[test]
    fn the_section_stays_within_its_budget() {
        let before = facts("26.05", "6.12.1", &[]);
        let mut after = before.clone();
        after.packages.nix = Some("2.32.0".to_string());
        after.boot.kernel = Some("kernel-b".to_string());
        let hosts: Vec<HostChange> = (0..30)
            .map(|i| host(&format!("host{i}"), before.clone(), after.clone()))
            .collect();

        for budget in (0..4_000).step_by(37) {
            if let Some(section) = render(&hosts, &[notes(40)], budget) {
                assert!(section.len() <= budget, "{budget}: {}", section.len());
            }
        }

        let section = render(&hosts, &[notes(40)], 4_000).unwrap();
        assert!(section.contains("more hosts"), "{section}");
        assert!(section.contains("… and"), "{section}");
        assert!(section.contains(REBOOT_NOTE), "{section}");
    }

    #[test]
    fn release_notes_say_when_there_are_more_commits_than_listed() {
        let mut long = notes(3);
        long.truncated = true;

        let text = render_notes(&long, 2_000).unwrap();

        assert!(text.contains("changed in at least 3 commits:"), "{text}");
    }
}
