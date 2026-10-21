//! Which outputs of a flake a lock update changes, and which it leaves alone.
//!
//! The flake is evaluated with the lock before and after the update, and the derivation of
//! each output is compared. Only outputs that are derivations are covered: `devShells`,
//! `packages`, `checks`, `formatter`, `apps` (by their program), and the system of
//! `nixosConfigurations`, `darwinConfigurations` and `homeConfigurations`. `lib`, `overlays`,
//! `nixosModules`, `templates` and the like have no derivation to compare.
//!
//! The same evaluation reads the tools in each of the `devShells`, and the packages of each
//! home-manager user, which say more about what an update does to them than whether their
//! derivation differs. The users are those of `nixosConfigurations` and `darwinConfigurations`
//! (through the home-manager module, where they use it) and of `homeConfigurations`.
//!
//! Evaluating needs the whole repository on disk, since outputs may use any file of it, and
//! fetches the inputs of both locks. Only call it from a job.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::core::clients::github::GitHub;
use crate::core::engine::repository::{FileModification, ProjectRepositorySnapshot};

use super::apply_updates::NixAccess;
use super::dev_shells::{self, DevShellChanges};
use super::flake_eval;

/// What evaluating the flake yields: the derivation of each output, or `None` for the ones that
/// could not be evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Evaluation {
    /// The system that per-system outputs (`packages`, `devShells`, ...) were evaluated for.
    pub system: String,
    pub outputs: BTreeMap<String, Option<String>>,
    /// The tools of each dev shell, or `None` for a shell that could not be evaluated.
    #[serde(default, rename = "devShellTools")]
    pub dev_shell_tools: dev_shells::Shells,
    /// The packages of each home-manager user, named `host/user` (just the name, for a
    /// standalone configuration), or `None` for one that could not be evaluated.
    #[serde(default, rename = "homeEnvironments")]
    pub home_environments: dev_shells::Shells,
}

/// An output the update changes, with its derivation (or program, for apps) before and after.
/// `before` is `None` for an output the update adds, `after` for one it removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedOutput {
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// The name a store path has, without its hash and without the `.drv` of a derivation:
/// `tool-1.5.0` for `/nix/store/<hash>-tool-1.5.0.drv`. Taken from the first path component
/// that looks like a store entry, so it works for a program inside a store path too.
pub fn store_name(path: &str) -> Option<&str> {
    let entry = path
        .split('/')
        .find(|part| part.len() > 33 && part.as_bytes()[32] == b'-')?;
    let name = &entry[33..];
    Some(name.strip_suffix(".drv").unwrap_or(name))
}

/// Which outputs the update changes. The names are `category.name`, e.g. `packages.default`,
/// or just `formatter`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AffectedOutputs {
    pub system: String,
    /// Outputs the update changes.
    pub changed: Vec<ChangedOutput>,
    /// Outputs that change with any edit of the lock, such as a package built from the whole
    /// source, which includes `flake.lock`. Nothing can be said about what the update does to
    /// them.
    pub changes_on_any_lock_edit: Vec<String>,
    /// Outputs the update leaves alone.
    pub unchanged: Vec<String>,
    /// Outputs that could not be evaluated before or after the update.
    pub unevaluated: Vec<String>,
    /// The tools of the dev shells the update changes.
    pub dev_shells: DevShellChanges,
    /// The packages of the home-manager users the update changes. Each user counts as a "shell".
    pub home_environments: DevShellChanges,
}

impl AffectedOutputs {
    /// Whether the outputs include any that were evaluated at all.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
            && self.changes_on_any_lock_edit.is_empty()
            && self.unchanged.is_empty()
    }
}

/// How `old` and `new` differ per output. `control` is the evaluation of the old lock with a
/// trivial edit: an output that differs from `old` there changes with any edit of the lock.
pub fn compare(
    old: &Evaluation,
    new: &Evaluation,
    control: Option<&Evaluation>,
) -> AffectedOutputs {
    let mut result = AffectedOutputs {
        system: new.system.clone(),
        dev_shells: dev_shells::compare(&old.dev_shell_tools, &new.dev_shell_tools),
        home_environments: dev_shells::compare(&old.home_environments, &new.home_environments),
        ..AffectedOutputs::default()
    };

    let names: std::collections::BTreeSet<&String> =
        old.outputs.keys().chain(new.outputs.keys()).collect();

    for name in names {
        let name = name.clone();
        let (before, after) = (old.outputs.get(&name), new.outputs.get(&name));

        match (before, after) {
            (Some(None), _) | (_, Some(None)) => result.unevaluated.push(name),
            (Some(Some(before)), Some(Some(after))) if before == after => {
                result.unchanged.push(name)
            }
            (Some(Some(before)), Some(Some(after))) => {
                let moves_with_any_edit = control
                    .and_then(|control| control.outputs.get(&name))
                    .is_some_and(|edited| edited.as_ref().is_some_and(|edited| edited != before));

                if moves_with_any_edit {
                    result.changes_on_any_lock_edit.push(name);
                } else {
                    result.changed.push(ChangedOutput {
                        name,
                        before: Some(before.clone()),
                        after: Some(after.clone()),
                    });
                }
            }
            // An output the update adds or removes.
            _ => result.changed.push(ChangedOutput {
                before: before.cloned().flatten(),
                after: after.cloned().flatten(),
                name,
            }),
        }
    }

    result
}

/// Evaluates the flake of `snapshot` with its lock and with the lock the `modifications`
/// write, and says what the update changes.
///
/// `None` when the update doesn't touch `flake.lock`, or there is no lock to compare. An error
/// when the flake can't be evaluated, e.g. because an input can't be fetched; the caller
/// decides how much that matters.
pub async fn analyze(
    github: &GitHub,
    snapshot: &dyn ProjectRepositorySnapshot,
    work_dir: &Path,
    modifications: &[FileModification],
) -> Result<Option<AffectedOutputs>> {
    let Some(prepared) = flake_eval::prepare(github, snapshot, work_dir, modifications).await?
    else {
        return Ok(None);
    };

    let comparison = evaluate_states(
        snapshot,
        prepared.dir.path(),
        &prepared.old_lock,
        modifications,
        &prepared.access,
    )
    .await?;

    Ok(Some(comparison))
}

/// Evaluates the repository in `dir`, which has the files of `snapshot`, before and after the
/// `modifications`, and compares them.
///
/// The control evaluation only happens if something changed, since it is what tells whether
/// that is the update's doing.
async fn evaluate_states(
    snapshot: &dyn ProjectRepositorySnapshot,
    dir: &Path,
    old_lock: &str,
    modifications: &[FileModification],
    access: &NixAccess,
) -> Result<AffectedOutputs> {
    std::fs::write(dir.join("flake.lock"), old_lock)?;
    let old = evaluate(dir, access).await.context("Before the update")?;

    flake_eval::apply(dir, modifications)?;
    let new = evaluate(dir, access).await.context("After the update")?;

    let comparison = compare(&old, &new, None);
    if comparison.changed.is_empty() {
        return Ok(comparison);
    }

    // The old state again, with an edit that changes nothing about the inputs.
    flake_eval::restore(snapshot, dir, modifications).await?;
    std::fs::write(dir.join("flake.lock"), format!("{old_lock}\n"))?;
    let control = match evaluate(dir, access).await {
        Ok(control) => Some(control),
        Err(e) => {
            tracing::warn!(
                "Could not tell which flake outputs change with any lock edit: {:#}",
                e
            );
            None
        }
    };

    Ok(compare(&old, &new, control.as_ref()))
}

/// Evaluates the flake in `dir`.
async fn evaluate(dir: &Path, access: &NixAccess) -> Result<Evaluation> {
    flake_eval::evaluate(dir, access, BODY).await
}

/// The nix expression that evaluates the flake to an [`Evaluation`].
///
/// Every output is evaluated within `tryEval`, so that one that throws shows up as `null` and
/// doesn't take the others with it.
const BODY: &str = r#"
let
  system = builtins.currentSystem;
  guard = value:
    let result = builtins.tryEval value;
    in if result.success then result.value else null;
  names = set:
    let result = builtins.tryEval (if builtins.isAttrs set then builtins.attrNames set else [ ]);
    in if result.success then result.value else [ ];
  drvPath = x:
    if builtins.isAttrs x && x ? drvPath
    then builtins.unsafeDiscardStringContext x.drvPath
    else null;
  program = x:
    if builtins.isAttrs x && x ? program
    then builtins.unsafeDiscardStringContext x.program
    else null;
  entries = prefix: set: pick:
    builtins.listToAttrs (map
      (name: { name = "${prefix}.${name}"; value = guard (pick set.${name}); })
      (names set));
  single = name: value: pick:
    if guard (value == null) == true then { } else { ${name} = guard (pick value); };
  guardDeep = value: guard (builtins.deepSeq value value);
  flatten = x: if builtins.isList x then builtins.concatMap flatten x else [ x ];
  # A tool is an input that is a derivation, known by its pname and version, or, for one that
  # has neither, by its name split where the version starts.
  tool = x:
    if builtins.isAttrs x && (x.type or null) == "derivation" && builtins.isString (x.name or null)
    then
      let
        parsed = builtins.parseDrvName x.name;
        name = x.pname or parsed.name;
        version = x.version or parsed.version;
      in
      if builtins.isString name && builtins.isString version
      then [{ inherit name; value = version; }]
      else [ ]
    else [ ];
  # The tools among `inputs` (which may be nested lists) whose version passes `keep`.
  toolsIn = keep: inputs:
    builtins.listToAttrs (builtins.filter
      (found: keep found.value)
      (builtins.concatMap
        (input:
          let found = guardDeep (tool input);
          in if found == null then [ ] else found)
        (flatten inputs)));
  toolsOf = shell:
    if !(builtins.isAttrs shell) then null
    else toolsIn (_: true) (map (attribute: shell.${attribute} or [ ]) [
      "nativeBuildInputs"
      "buildInputs"
      "propagatedNativeBuildInputs"
      "propagatedBuildInputs"
    ]);
  # The packages of a home-manager user. What has no version is a wrapper or a generated file,
  # such as a session script, and says nothing about an update.
  packagesOf = home:
    if !(builtins.isAttrs home) then null
    else toolsIn (version: version != "") [ (home.packages or [ ]) ];
  # The users home-manager manages in a NixOS or darwin configuration, if it is used at all.
  usersOf = configuration:
    let users = guard ((configuration.config.home-manager or { }).users or { });
    in if builtins.isAttrs users then users else { };
  # `host/user` to the packages of the user, for each configuration that manages users.
  moduleEnvironments = configurations:
    builtins.concatMap
      (host:
        let users = usersOf configurations.${host};
        in map
          (user: {
            name = "${host}/${user}";
            value = guardDeep (packagesOf users.${user}.home);
          })
          (names users))
      (names configurations);
  # A standalone home-manager configuration is one user, named after the configuration.
  standaloneEnvironments = configurations:
    map
      (name: { inherit name; value = guardDeep (packagesOf configurations.${name}.config.home); })
      (names configurations);
in {
  inherit system;
  devShellTools = builtins.listToAttrs (map
    (name: { inherit name; value = guardDeep (toolsOf flake.devShells.${system}.${name}); })
    (names (flake.devShells.${system} or { })));
  homeEnvironments = builtins.listToAttrs (
    moduleEnvironments (flake.nixosConfigurations or { })
    ++ moduleEnvironments (flake.darwinConfigurations or { })
    ++ standaloneEnvironments (flake.homeConfigurations or { })
  );
  outputs = builtins.foldl' (a: b: a // b) { } [
    (entries "devShells" (flake.devShells.${system} or { }) drvPath)
    (entries "packages" (flake.packages.${system} or { }) drvPath)
    (entries "checks" (flake.checks.${system} or { }) drvPath)
    (entries "apps" (flake.apps.${system} or { }) program)
    (single "formatter" (flake.formatter.${system} or null) drvPath)
    (entries "nixosConfigurations" (flake.nixosConfigurations or { })
      (c: drvPath (c.config.system.build.toplevel or null)))
    (entries "darwinConfigurations" (flake.darwinConfigurations or { })
      (c: drvPath (c.config.system.build.toplevel or null)))
    (entries "homeConfigurations" (flake.homeConfigurations or { })
      (c: drvPath (c.activationPackage or null)))
  ];
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::repository::test_support::MockSnapshot;
    use crate::core::engine::repository::FileState;

    fn evaluation(outputs: &[(&str, Option<&str>)]) -> Evaluation {
        Evaluation {
            system: "x86_64-linux".to_string(),
            outputs: outputs
                .iter()
                .map(|(name, drv)| (name.to_string(), drv.map(str::to_string)))
                .collect(),
            dev_shell_tools: Default::default(),
            home_environments: Default::default(),
        }
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn changed_names(result: &AffectedOutputs) -> Vec<String> {
        result
            .changed
            .iter()
            .map(|change| change.name.clone())
            .collect()
    }

    #[test]
    fn tells_changed_from_unchanged_outputs() {
        let old = evaluation(&[
            ("packages.tool", Some("a")),
            ("packages.docs", Some("b")),
            ("devShells.default", Some("c")),
        ]);
        let new = evaluation(&[
            ("packages.tool", Some("a2")),
            ("packages.docs", Some("b")),
            ("devShells.default", Some("c")),
        ]);

        let result = compare(&old, &new, None);

        assert_eq!(changed_names(&result), names(&["packages.tool"]));
        assert_eq!(
            result.unchanged,
            names(&["devShells.default", "packages.docs"])
        );
        assert!(result.changes_on_any_lock_edit.is_empty());
        assert!(result.unevaluated.is_empty());
    }

    #[test]
    fn an_output_that_moves_with_any_lock_edit_is_not_blamed_on_the_update() {
        let old = evaluation(&[
            ("packages.default", Some("a")),
            ("devShells.default", Some("c")),
        ]);
        let new = evaluation(&[
            ("packages.default", Some("a2")),
            ("devShells.default", Some("c2")),
        ]);
        let control = evaluation(&[
            ("packages.default", Some("a3")),
            ("devShells.default", Some("c")),
        ]);

        let result = compare(&old, &new, Some(&control));

        assert_eq!(
            result.changes_on_any_lock_edit,
            names(&["packages.default"])
        );
        assert_eq!(changed_names(&result), names(&["devShells.default"]));
    }

    #[test]
    fn outputs_that_could_not_be_evaluated_are_reported_as_such() {
        let old = evaluation(&[
            ("nixosConfigurations.host-a", None),
            ("packages.tool", Some("a")),
        ]);
        let new = evaluation(&[
            ("nixosConfigurations.host-a", Some("b")),
            ("packages.tool", None),
        ]);

        let result = compare(&old, &new, None);

        assert_eq!(
            result.unevaluated,
            names(&["nixosConfigurations.host-a", "packages.tool"])
        );
        assert!(result.is_empty());
    }

    #[test]
    fn added_and_removed_outputs_count_as_changed() {
        let old = evaluation(&[("packages.gone", Some("a"))]);
        let new = evaluation(&[("packages.fresh", Some("b"))]);

        let result = compare(&old, &new, None);

        assert_eq!(
            changed_names(&result),
            names(&["packages.fresh", "packages.gone"])
        );
    }

    #[test]
    fn changed_outputs_keep_what_they_were_before_and_after() {
        let old = evaluation(&[("packages.kept", Some("a")), ("packages.gone", Some("b"))]);
        let new = evaluation(&[("packages.kept", Some("c")), ("packages.fresh", Some("d"))]);

        let result = compare(&old, &new, None);

        let find = |name: &str| result.changed.iter().find(|c| c.name == name).unwrap();
        assert_eq!(find("packages.kept").before.as_deref(), Some("a"));
        assert_eq!(find("packages.kept").after.as_deref(), Some("c"));
        assert_eq!(find("packages.fresh").before, None);
        assert_eq!(find("packages.gone").after, None);
    }

    #[test]
    fn store_names_lose_their_hash_and_extension() {
        let hash = "c3yh3hnrjnfp3a7kb9h5q8x1ylmcr2kh";
        assert_eq!(
            store_name(&format!("/nix/store/{hash}-tool-1.5.0.drv")),
            Some("tool-1.5.0")
        );
        assert_eq!(
            store_name(&format!("/nix/store/{hash}-tool-1.5.0/bin/tool")),
            Some("tool-1.5.0")
        );
        assert_eq!(store_name("/nix/store/short"), None);
    }

    const FLAKE_NIX: &str = r#"{
  outputs = { self }:
    let
      system = builtins.currentSystem;
      build = name: script: derivation {
        inherit name system;
        builder = "/bin/sh";
        args = [ "-c" script ];
      };
    in {
      packages.${system} = {
        # Built from the whole source, which includes flake.lock.
        whole = build "whole" "echo ${self} > $out";
        data = build "data" "echo ${builtins.readFile ./data.txt} > $out";
        fixed = build "fixed" "echo fixed > $out";
        broken = throw "this one does not evaluate";
      };
      # Its tools are named after what data.txt says, which is what the update changes.
      devShells.${system} = {
        default = (build "shell" "echo shell > $out") // {
          nativeBuildInputs = [ (build "mytool-${builtins.readFile ./data.txt}" "echo tool > $out") ];
        };
        agent = (build "agent" "echo agent > $out") // {
          buildInputs = [
            (build "mytool-${builtins.readFile ./data.txt}" "echo tool > $out")
            [ (build "extra-3.1" "echo extra > $out") ]
            null
          ];
        };
      };
      formatter.${system} = build "fmt" "echo fmt > $out";
      # Two users through the home-manager module. A package without a version is a wrapper
      # or a generated file, and `broken` is a user that can't be evaluated.
      nixosConfigurations.host-a.config = {
        system.build.toplevel = build "host-a" "echo a > $out";
        home-manager.users = {
          alice.home.packages = [
            (build "mytool-${builtins.readFile ./data.txt}" "echo tool > $out")
            (build "steady-4.2" "echo steady > $out")
            (build "wrapper" "echo wrapper > $out")
            (build "session-vars.sh" "echo vars > $out")
          ];
          bob.home.packages = [
            (build "mytool-${builtins.readFile ./data.txt}" "echo tool > $out")
          ];
          broken.home.packages = throw "this user does not evaluate";
        };
      };
      # A host that doesn't use home-manager has nothing to report.
      darwinConfigurations.host-b.config.system.build.toplevel = build "host-b" "echo b > $out";
      homeConfigurations.carol = {
        activationPackage = build "carol" "echo carol > $out";
        config.home.packages = [ (build "mytool-${builtins.readFile ./data.txt}" "echo tool > $out") ];
      };
      lib.not-a-derivation = 1;
    };
}
"#;

    #[tokio::test]
    async fn evaluates_and_compares_a_real_flake() {
        if !flake_eval::nix_works() {
            eprintln!("nix isn't usable here, skipping");
            return;
        }

        let lock = "{\n  \"nodes\": {\n    \"root\": {}\n  },\n  \"root\": \"root\",\n  \"version\": 7\n}\n";
        let snapshot = MockSnapshot::with_files(&[
            ("flake.nix", FLAKE_NIX),
            ("flake.lock", lock),
            ("data.txt", "1.0"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        flake_eval::materialize(&snapshot, dir.path())
            .await
            .unwrap();

        // Stands in for an update: it changes a file that one output reads, as an update
        // changes the sources an output is built from.
        let modifications = vec![FileModification {
            path: "data.txt".to_string(),
            state: FileState::Write("2.0".to_string()),
        }];
        let access = NixAccess {
            config: None,
            secrets: Vec::new(),
        };

        let result = evaluate_states(&snapshot, dir.path(), lock, &modifications, &access)
            .await
            .unwrap();

        assert_eq!(changed_names(&result), names(&["packages.data"]));
        assert_eq!(result.changes_on_any_lock_edit, names(&["packages.whole"]));
        assert_eq!(
            result.unchanged,
            names(&[
                "darwinConfigurations.host-b",
                "devShells.agent",
                "devShells.default",
                "formatter",
                "homeConfigurations.carol",
                "nixosConfigurations.host-a",
                "packages.fixed",
            ])
        );
        assert_eq!(result.unevaluated, names(&["packages.broken"]));
        assert!(!result.system.is_empty());

        // The tools are read from the inputs of the shells, however they are nested, and
        // what the update changes about them is one change, for both shells.
        let dev_shells = &result.dev_shells;
        assert_eq!(dev_shells.shells, names(&["default", "agent"]));
        assert_eq!(dev_shells.unevaluated, 0);
        assert_eq!(dev_shells.changes.len(), 1, "{dev_shells:?}");
        let tool = &dev_shells.changes[0];
        assert_eq!(tool.name, "mytool");
        assert_eq!(tool.before.as_deref(), Some("1.0"));
        assert_eq!(tool.after.as_deref(), Some("2.0"));
        assert_eq!(tool.shells, names(&["default", "agent"]));

        // The packages of the users are read through the home-manager module and from the
        // standalone configuration alike, and what the update changes about them is one change
        // for all of them. Packages without a version, and hosts without users, don't show up.
        let environments = &result.home_environments;
        assert_eq!(
            environments.shells,
            names(&["carol", "host-a/alice", "host-a/bob"])
        );
        assert_eq!(environments.unevaluated, 1);
        assert_eq!(environments.changes.len(), 1, "{environments:?}");
        let package = &environments.changes[0];
        assert_eq!(package.name, "mytool");
        assert_eq!(package.before.as_deref(), Some("1.0"));
        assert_eq!(package.after.as_deref(), Some("2.0"));
        assert_eq!(
            package.shells,
            names(&["carol", "host-a/alice", "host-a/bob"])
        );
    }
}
