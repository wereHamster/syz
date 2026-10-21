//! What a lock update does to the machines a flake describes: the facts of each NixOS and
//! darwin host before and after it, and whether the NixOS ones should be rebooted.
//!
//! Every host is evaluated on its own, so that one that can't be evaluated (an input of it needs
//! a token that isn't there, say) is left out without taking the others with it. Nothing is
//! built: the facts are read off the evaluated configuration, and the reboot hint compares the
//! derivations of the initrd, the kernel and the kernel modules.
//!
//! Only call it from a job, for the same reasons as [`super::affected_outputs`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::core::clients::github::GitHub;
use crate::core::engine::repository::{FileModification, ProjectRepositorySnapshot};

use super::apply_updates::NixAccess;
use super::flake_eval::{self, nix_string};

/// How many hosts are evaluated. A flake with more of them is not worth the time it takes.
const MAX_HOSTS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostKind {
    Nixos,
    Darwin,
}

impl HostKind {
    /// The output of the flake the hosts of this kind are in.
    fn output(self) -> &'static str {
        match self {
            HostKind::Nixos => "nixosConfigurations",
            HostKind::Darwin => "darwinConfigurations",
        }
    }
}

/// The versions of a few packages that say something about the whole system.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Packages {
    pub nix: Option<String>,
    pub openssh: Option<String>,
    pub systemd: Option<String>,
}

impl Packages {
    /// The packages with a version, by name.
    pub fn versions(&self) -> [(&'static str, Option<&str>); 3] {
        [
            ("nix", self.nix.as_deref()),
            ("openssh", self.openssh.as_deref()),
            ("systemd", self.systemd.as_deref()),
        ]
    }
}

/// The derivations a boot depends on. NixOS's own rule for rebooting after an upgrade
/// (`system.autoUpgrade.allowReboot`) is that one of them differs from the booted system's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct BootInputs {
    pub initrd: Option<String>,
    pub kernel: Option<String>,
    #[serde(rename = "kernelModules")]
    pub kernel_modules: Option<String>,
}

/// What is known about a host in one state of the flake. Anything a host doesn't have, or that
/// couldn't be evaluated, is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct HostFacts {
    /// The release of NixOS, such as `26.05`.
    pub release: Option<String>,
    pub kernel: Option<String>,
    #[serde(rename = "stateVersion")]
    pub state_version: Option<String>,
    /// The names of the services the host runs: systemd services on NixOS, launchd daemons and
    /// agents on darwin.
    pub services: Vec<String>,
    pub packages: Packages,
    pub boot: BootInputs,
}

impl HostFacts {
    /// Whether anything about the host could be evaluated. A configuration that throws on
    /// everything is as good as one that can't be evaluated.
    fn is_evaluated(&self) -> bool {
        self.release.is_some()
            || self.kernel.is_some()
            || self.state_version.is_some()
            || !self.services.is_empty()
            || self.packages != Packages::default()
    }
}

/// A host in the flake, with its facts before and after the update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostChange {
    pub name: String,
    pub kind: HostKind,
    pub before: HostFacts,
    pub after: HostFacts,
}

/// The kernel, initrd or kernel modules of a NixOS host, as far as they differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootPart {
    Initrd,
    Kernel,
    KernelModules,
}

impl BootPart {
    pub fn label(self) -> &'static str {
        match self {
            BootPart::Initrd => "initrd",
            BootPart::Kernel => "kernel",
            BootPart::KernelModules => "kernel modules",
        }
    }
}

impl HostChange {
    /// The services the update adds, and those it removes.
    pub fn services(&self) -> (Vec<&str>, Vec<&str>) {
        let before: BTreeSet<&str> = self.before.services.iter().map(String::as_str).collect();
        let after: BTreeSet<&str> = self.after.services.iter().map(String::as_str).collect();

        (
            after.difference(&before).copied().collect(),
            before.difference(&after).copied().collect(),
        )
    }

    /// Which of the things a boot depends on the update changes. Always empty for darwin, which
    /// has nothing to reboot into. Only what could be evaluated both before and after is
    /// compared.
    pub fn reboot_reasons(&self) -> Vec<BootPart> {
        if self.kind != HostKind::Nixos {
            return Vec::new();
        }

        let differs = |before: &Option<String>, after: &Option<String>| matches!((before, after), (Some(before), Some(after)) if before != after);
        let (before, after) = (&self.before.boot, &self.after.boot);

        [
            (BootPart::Initrd, differs(&before.initrd, &after.initrd)),
            (BootPart::Kernel, differs(&before.kernel, &after.kernel)),
            (
                BootPart::KernelModules,
                differs(&before.kernel_modules, &after.kernel_modules),
            ),
        ]
        .into_iter()
        .filter_map(|(part, differs)| differs.then_some(part))
        .collect()
    }

    /// Whether the update changes anything about the host that is reported.
    pub fn is_changed(&self) -> bool {
        let (before, after) = (&self.before, &self.after);
        let (added, removed) = self.services();

        before.release != after.release
            || before.kernel != after.kernel
            || before.state_version != after.state_version
            || before.packages != after.packages
            || !added.is_empty()
            || !removed.is_empty()
            || !self.reboot_reasons().is_empty()
    }
}

/// The hosts the update changes, in the order of their names, NixOS ones first. Hosts that
/// aren't evaluated both before and after are not in `before` and `after`.
pub fn compare(
    before: &BTreeMap<(HostKind, String), HostFacts>,
    after: &BTreeMap<(HostKind, String), HostFacts>,
) -> Vec<HostChange> {
    before
        .iter()
        .filter_map(|((kind, name), before)| {
            let after = after.get(&(*kind, name.clone()))?;
            Some(HostChange {
                name: name.clone(),
                kind: *kind,
                before: before.clone(),
                after: after.clone(),
            })
        })
        .filter(HostChange::is_changed)
        .collect()
}

/// Evaluates the hosts of the flake of `snapshot` with its lock and with the lock the
/// `modifications` write, and says which of them the update changes.
///
/// `None` when the update doesn't touch `flake.lock`, or there is no lock to compare. An empty
/// list when the flake has no hosts, or the update changes none of them. An error when the
/// repository can't be put on disk, or the flake can't be evaluated at all.
pub async fn analyze(
    github: &GitHub,
    snapshot: &dyn ProjectRepositorySnapshot,
    work_dir: &Path,
    modifications: &[FileModification],
) -> Result<Option<Vec<HostChange>>> {
    let Some(prepared) = flake_eval::prepare(github, snapshot, work_dir, modifications).await?
    else {
        return Ok(None);
    };
    let dir = prepared.dir.path();

    std::fs::write(dir.join("flake.lock"), &prepared.old_lock)?;
    let hosts = list_hosts(dir, &prepared.access)
        .await
        .context("Before the update")?;
    if hosts.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let before = evaluate_hosts(dir, &prepared.access, &hosts).await;
    if before.is_empty() {
        return Ok(Some(Vec::new()));
    }

    flake_eval::apply(dir, modifications)?;
    let after = evaluate_hosts(dir, &prepared.access, &hosts).await;

    Ok(Some(compare(&before, &after)))
}

/// The hosts of the flake in `dir`.
async fn list_hosts(dir: &Path, access: &NixAccess) -> Result<Vec<(HostKind, String)>> {
    let names: BTreeMap<String, Vec<String>> =
        flake_eval::evaluate(dir, access, HOSTS_BODY).await?;

    let mut hosts = Vec::new();
    for kind in [HostKind::Nixos, HostKind::Darwin] {
        for name in names.get(kind.output()).into_iter().flatten() {
            hosts.push((kind, name.clone()));
        }
    }

    if hosts.len() > MAX_HOSTS {
        tracing::info!(
            "The flake has {} hosts, only the first {} are evaluated",
            hosts.len(),
            MAX_HOSTS
        );
        hosts.truncate(MAX_HOSTS);
    }
    Ok(hosts)
}

/// The facts of each of `hosts` that can be evaluated, one nix at a time since memory is what a
/// host takes. A host that can't be evaluated is left out, and logged: it is for the checks of
/// the pull request to say what is wrong with it.
async fn evaluate_hosts(
    dir: &Path,
    access: &NixAccess,
    hosts: &[(HostKind, String)],
) -> BTreeMap<(HostKind, String), HostFacts> {
    let mut facts = BTreeMap::new();

    for (kind, name) in hosts {
        match flake_eval::evaluate(dir, access, &host_body(*kind, name)).await {
            Ok(host) if HostFacts::is_evaluated(&host) => {
                facts.insert((*kind, name.clone()), host);
            }
            Ok(_) => tracing::info!("Nothing about host {} could be evaluated", name),
            Err(e) => tracing::info!("Could not evaluate host {}: {:#}", name, e),
        }
    }

    facts
}

/// The names of the hosts of each kind.
const HOSTS_BODY: &str = r#"
let
  names = set:
    let result = builtins.tryEval (if builtins.isAttrs set then builtins.attrNames set else [ ]);
    in if result.success then result.value else [ ];
in {
  nixosConfigurations = names (flake.nixosConfigurations or { });
  darwinConfigurations = names (flake.darwinConfigurations or { });
}
"#;

/// What evaluates the facts of the host `name`.
///
/// Each fact is evaluated within `tryEval` and with `or`, so that a configuration that lacks
/// something, or throws on it, only loses that fact.
fn host_body(kind: HostKind, name: &str) -> String {
    let attribute = format!("${{{}}}", nix_string(name));
    let facts = match kind {
        HostKind::Nixos => NIXOS_FACTS,
        HostKind::Darwin => DARWIN_FACTS,
    };

    format!(
        "let host = flake.{}.{attribute}; config = host.config; {HELPERS} in {}",
        kind.output(),
        facts.trim_start()
    )
}

/// Bindings both kinds of host use.
const HELPERS: &str = r#"
  guard = value:
    let result = builtins.tryEval value;
    in if result.success then result.value else null;
  text = value:
    if builtins.isString value then value
    else if builtins.isInt value then toString value
    else null;
  version = package:
    guard (if builtins.isAttrs package then text (package.version or null) else null);
  drvPath = x:
    guard (
      if builtins.isAttrs x && x ? drvPath
      then builtins.unsafeDiscardStringContext x.drvPath
      else null);
  names = set:
    let result = builtins.tryEval (if builtins.isAttrs set then builtins.attrNames set else [ ]);
    in if result.success then result.value else [ ];
"#;

const NIXOS_FACTS: &str = r#"
{
  release = guard (text (config.system.nixos.release or null));
  kernel = version (config.boot.kernelPackages.kernel or null);
  stateVersion = guard (text (config.system.stateVersion or null));
  services = names (config.systemd.services or { });
  packages = {
    nix = version (config.nix.package or null);
    openssh = version (config.programs.ssh.package or null);
    systemd = version (config.systemd.package or null);
  };
  boot = {
    initrd = drvPath (config.system.build.initialRamdisk or null);
    kernel = drvPath (config.boot.kernelPackages.kernel or null);
    kernelModules = drvPath (config.system.modulesTree or null);
  };
}
"#;

const DARWIN_FACTS: &str = r#"
{
  stateVersion = guard (text (config.system.stateVersion or null));
  services = builtins.sort builtins.lessThan
    (names (config.launchd.daemons or { }) ++ names (config.launchd.agents or { }));
  packages.nix = version (config.nix.package or null);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::repository::test_support::MockSnapshot;

    fn facts(release: &str, kernel: &str, services: &[&str]) -> HostFacts {
        HostFacts {
            release: Some(release.to_string()),
            kernel: Some(kernel.to_string()),
            state_version: Some("25.11".to_string()),
            services: services.iter().map(|s| s.to_string()).collect(),
            packages: Packages {
                nix: Some("2.31.1".to_string()),
                ..Packages::default()
            },
            boot: BootInputs {
                initrd: Some("initrd-a".to_string()),
                kernel: Some("kernel-a".to_string()),
                kernel_modules: Some("modules-a".to_string()),
            },
        }
    }

    fn change(before: HostFacts, after: HostFacts) -> HostChange {
        HostChange {
            name: "web".to_string(),
            kind: HostKind::Nixos,
            before,
            after,
        }
    }

    #[test]
    fn a_host_the_update_leaves_alone_is_not_changed() {
        let host = facts("26.05", "6.12.1", &["sshd.service"]);
        assert!(!change(host.clone(), host).is_changed());
    }

    #[test]
    fn services_are_added_and_removed_by_name() {
        let before = facts("26.05", "6.12.1", &["a.service", "b.service", "c.service"]);
        let after = facts("26.05", "6.12.1", &["b.service", "c.service", "d.service"]);

        let host = change(before, after);
        let (added, removed) = host.services();

        assert_eq!(added, vec!["d.service"]);
        assert_eq!(removed, vec!["a.service"]);
        assert!(host.is_changed());
    }

    #[test]
    fn a_reboot_is_recommended_for_what_a_boot_depends_on() {
        let before = facts("26.05", "6.12.1", &[]);
        let mut after = before.clone();
        after.boot.kernel = Some("kernel-b".to_string());
        after.boot.kernel_modules = Some("modules-b".to_string());

        let host = change(before, after);

        assert_eq!(
            host.reboot_reasons(),
            vec![BootPart::Kernel, BootPart::KernelModules]
        );
        assert!(host.is_changed());
    }

    #[test]
    fn what_could_not_be_evaluated_is_not_a_reason_to_reboot() {
        let before = facts("26.05", "6.12.1", &[]);
        let mut after = before.clone();
        after.boot.initrd = None;

        assert!(change(before, after).reboot_reasons().is_empty());
    }

    #[test]
    fn darwin_has_nothing_to_reboot_into() {
        let before = facts("26.05", "6.12.1", &[]);
        let mut after = before.clone();
        after.boot.kernel = Some("kernel-b".to_string());

        let mut host = change(before, after);
        host.kind = HostKind::Darwin;

        assert!(host.reboot_reasons().is_empty());
        assert!(!host.is_changed());
    }

    #[test]
    fn only_hosts_evaluated_both_times_and_changed_are_compared() {
        let key = |name: &str| (HostKind::Nixos, name.to_string());
        let host = facts("26.05", "6.12.1", &[]);
        let newer = facts("26.05", "6.12.2", &[]);

        let before = BTreeMap::from([
            (key("changed"), host.clone()),
            (key("same"), host.clone()),
            (key("gone"), host.clone()),
        ]);
        let after = BTreeMap::from([
            (key("changed"), newer),
            (key("same"), host.clone()),
            (key("new"), host),
        ]);

        let names: Vec<String> = compare(&before, &after)
            .into_iter()
            .map(|host| host.name)
            .collect();
        assert_eq!(names, vec!["changed"]);
    }

    #[test]
    fn host_names_are_escaped_into_the_expression() {
        let body = host_body(HostKind::Nixos, r#"a"b${c}"#);
        assert!(body.starts_with(r#"let host = flake.nixosConfigurations.${"a\"b\${c}"}; "#));
    }

    /// A flake whose hosts have just what the facts are read from, the way the modules of NixOS
    /// and nix-darwin put it. `version.txt` stands for the update.
    const FLAKE_NIX: &str = r#"{
  outputs = { self }:
    let
      system = builtins.currentSystem;
      version = builtins.replaceStrings [ "\n" ] [ "" ] (builtins.readFile ./version.txt);
      drv = name: derivation {
        inherit name system;
        builder = "/bin/sh";
        args = [ "-c" "echo ${name} > $out" ];
      };
      services = if version == "2" then [ "sshd" "ntpd" ] else [ "sshd" "cron" ];
    in {
      nixosConfigurations."host a".config = {
        system.nixos.release = "26.05";
        system.stateVersion = "25.11";
        boot.kernelPackages.kernel = (drv "linux-${version}") // { version = "6.12.${version}"; };
        system.build.initialRamdisk = drv "initrd";
        system.modulesTree = drv "modules";
        systemd.services = builtins.listToAttrs (map (n: { name = n; value = { }; }) services);
        nix.package = { version = "2.31.${version}"; };
        systemd.package.version = "257";
      };
      nixosConfigurations.broken.config = throw "this host does not evaluate";
      darwinConfigurations.mac.config = {
        system.stateVersion = 6;
        launchd.daemons.nix-daemon = { };
        nix.package.version = "2.31.1";
      };
    };
}
"#;

    #[tokio::test]
    async fn evaluates_the_hosts_of_a_real_flake_before_and_after() {
        if !flake_eval::nix_works() {
            eprintln!("nix isn't usable here, skipping");
            return;
        }

        let lock = "{\n  \"nodes\": {\n    \"root\": {}\n  },\n  \"root\": \"root\",\n  \"version\": 7\n}\n";
        let snapshot = MockSnapshot::with_files(&[
            ("flake.nix", FLAKE_NIX),
            ("flake.lock", lock),
            ("version.txt", "1\n"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        flake_eval::materialize(&snapshot, dir.path())
            .await
            .unwrap();
        std::fs::write(dir.path().join("flake.lock"), lock).unwrap();
        let access = NixAccess {
            config: None,
            secrets: Vec::new(),
        };

        let hosts = list_hosts(dir.path(), &access).await.unwrap();
        assert_eq!(
            hosts,
            vec![
                (HostKind::Nixos, "broken".to_string()),
                (HostKind::Nixos, "host a".to_string()),
                (HostKind::Darwin, "mac".to_string()),
            ]
        );

        let before = evaluate_hosts(dir.path(), &access, &hosts).await;
        flake_eval::write_file(dir.path(), "version.txt", "2\n").unwrap();
        let after = evaluate_hosts(dir.path(), &access, &hosts).await;

        // The host that throws is left out.
        assert_eq!(before.len(), 2);

        let changes = compare(&before, &after);
        assert_eq!(changes.len(), 1);
        let host = &changes[0];
        assert_eq!(host.name, "host a");
        assert_eq!(host.before.release.as_deref(), Some("26.05"));
        assert_eq!(host.before.state_version.as_deref(), Some("25.11"));
        assert_eq!(host.before.kernel.as_deref(), Some("6.12.1"));
        assert_eq!(host.after.kernel.as_deref(), Some("6.12.2"));
        assert_eq!(host.before.packages.nix.as_deref(), Some("2.31.1"));
        assert_eq!(host.after.packages.nix.as_deref(), Some("2.31.2"));
        assert_eq!(host.after.packages.systemd.as_deref(), Some("257"));
        assert_eq!(host.after.packages.openssh, None);
        assert_eq!(host.services(), (vec!["ntpd"], vec!["cron"]));
        assert_eq!(host.reboot_reasons(), vec![BootPart::Kernel]);

        let mac = &before[&(HostKind::Darwin, "mac".to_string())];
        assert_eq!(mac.state_version.as_deref(), Some("6"));
        assert_eq!(mac.services, vec!["nix-daemon"]);
    }
}
