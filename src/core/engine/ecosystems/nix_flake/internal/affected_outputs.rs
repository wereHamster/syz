//! Which outputs of a flake a lock update changes, and which it leaves alone.
//!
//! The flake is evaluated with the lock before and after the update, and the derivation of
//! each output is compared. Only outputs that are derivations are covered: `devShells`,
//! `packages`, `checks`, `formatter`, `apps` (by their program), and the system of
//! `nixosConfigurations`, `darwinConfigurations` and `homeConfigurations`. `lib`, `overlays`,
//! `nixosModules`, `templates` and the like have no derivation to compare.
//!
//! The same evaluation reads the tools in each of the `devShells`, which says more about what an
//! update does to them than whether their derivation differs.
//!
//! Evaluating needs the whole repository on disk, since outputs may use any file of it, and
//! fetches the inputs of both locks. Only call it from a job.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures::{stream, StreamExt};
use serde::Deserialize;
use tokio::process::Command;

use crate::core::clients::github::GitHub;
use crate::core::engine::repository::{FileModification, FileState, ProjectRepositorySnapshot};

use super::apply_updates::{redact, NixAccess};
use super::dev_shells::{self, DevShellChanges};
use super::flake_lock::{self, FlakeLock};

/// How many files of the repository are put on disk. A repository with more files than this
/// is not worth the requests it takes to read them.
const MAX_FILES: usize = 400;

/// How many bytes of the repository are put on disk.
const MAX_BYTES: usize = 32 * 1024 * 1024;

/// How many files are read at the same time.
const READ_CONCURRENCY: usize = 8;

/// How long one evaluation of the flake may take.
const EVAL_TIMEOUT: Duration = Duration::from_secs(600);

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
    let Some(new_lock) = modifications
        .iter()
        .find_map(|modification| match &modification.state {
            FileState::Write(content) if modification.path == "flake.lock" => Some(content),
            _ => None,
        })
    else {
        return Ok(None);
    };
    let Ok(old_lock) = snapshot.read_file("flake.lock").await else {
        return Ok(None);
    };

    // The inputs of both locks are fetched, and those of the new one may belong to owners the
    // old one doesn't mention.
    let mut owners = lock_owners(&old_lock)?;
    owners.extend(lock_owners(new_lock)?);
    owners.sort();
    owners.dedup();
    let access = NixAccess::for_owners(github, &owners).await;

    std::fs::create_dir_all(work_dir)
        .with_context(|| format!("Failed to create work directory {}", work_dir.display()))?;
    let dir = tempfile::TempDir::new_in(work_dir)
        .with_context(|| format!("Failed to create a directory in {}", work_dir.display()))?;

    materialize(snapshot, dir.path()).await?;
    let comparison =
        evaluate_states(snapshot, dir.path(), &old_lock, modifications, &access).await?;

    Ok(Some(comparison))
}

fn lock_owners(lock: &str) -> Result<Vec<String>> {
    let lock: FlakeLock = serde_json::from_str(lock).context("Failed to parse flake.lock")?;
    Ok(flake_lock::github_owners(&lock))
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

    apply(dir, modifications)?;
    let new = evaluate(dir, access).await.context("After the update")?;

    let comparison = compare(&old, &new, None);
    if comparison.changed.is_empty() {
        return Ok(comparison);
    }

    // The old state again, with an edit that changes nothing about the inputs.
    restore(snapshot, dir, modifications).await?;
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

/// Puts the files of `snapshot` into `dir`.
async fn materialize(snapshot: &dyn ProjectRepositorySnapshot, dir: &Path) -> Result<()> {
    let files: Vec<String> = snapshot
        .list_files()
        .await?
        .into_iter()
        .filter(|path| path != "flake.lock")
        .collect();
    if files.len() > MAX_FILES {
        bail!(
            "The repository has {} files, more than the {} that are put on disk",
            files.len(),
            MAX_FILES
        );
    }

    let contents: Vec<(String, Option<String>)> = stream::iter(files)
        .map(|path| async move {
            let content = snapshot.read_file(&path).await.ok();
            (path, content)
        })
        .buffer_unordered(READ_CONCURRENCY)
        .collect()
        .await;

    // Files that aren't text can't be read, and are left out.
    let mut bytes = 0;
    for (path, content) in contents {
        let Some(content) = content else { continue };
        bytes += content.len();
        if bytes > MAX_BYTES {
            bail!(
                "The repository is larger than the {} bytes that are put on disk",
                MAX_BYTES
            );
        }
        write_file(dir, &path, &content)?;
    }

    Ok(())
}

/// A path that stays inside the directory it is joined to.
fn is_contained(path: &str) -> bool {
    use std::path::Component;
    Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
}

fn write_file(dir: &Path, path: &str, content: &str) -> Result<()> {
    if !is_contained(path) {
        bail!("Refusing to write {path} outside of the work directory");
    }
    let target = dir.join(path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, content).with_context(|| format!("Failed to write {path}"))
}

/// Makes the files in `dir` what the `modifications` say.
fn apply(dir: &Path, modifications: &[FileModification]) -> Result<()> {
    for modification in modifications {
        match &modification.state {
            FileState::Write(content) => write_file(dir, &modification.path, content)?,
            FileState::Delete => {
                if !is_contained(&modification.path) {
                    bail!(
                        "Refusing to delete {} outside of the work directory",
                        modification.path
                    );
                }
                match std::fs::remove_file(dir.join(&modification.path)) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// Puts the files the `modifications` touch back to what `snapshot` has.
async fn restore(
    snapshot: &dyn ProjectRepositorySnapshot,
    dir: &Path,
    modifications: &[FileModification],
) -> Result<()> {
    let mut originals = Vec::new();
    for modification in modifications {
        let original = snapshot.read_file(&modification.path).await.ok();
        originals.push(FileModification {
            path: modification.path.clone(),
            state: match original {
                Some(content) => FileState::Write(content),
                None => FileState::Delete,
            },
        });
    }
    apply(dir, &originals)
}

/// Evaluates the flake in `dir`.
async fn evaluate(dir: &Path, access: &NixAccess) -> Result<Evaluation> {
    let mut command = Command::new("nix");
    command
        .args(["--extra-experimental-features", "nix-command flakes"])
        .args(["eval", "--impure", "--json", "--expr"])
        .arg(expression(dir))
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(config) = &access.config {
        command.env("NIX_CONFIG", config);
    }

    let output = tokio::time::timeout(EVAL_TIMEOUT, command.output())
        .await
        .context("Evaluating the flake took too long")?
        .context("Failed to run nix")?;

    if !output.status.success() {
        let stderr = redact(&String::from_utf8_lossy(&output.stderr), &access.secrets);
        bail!("nix eval failed ({}): {}", output.status, stderr.trim());
    }

    serde_json::from_slice(&output.stdout).context("Unexpected output of nix eval")
}

/// A nix string with `text` in it.
fn nix_string(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("${", "\\${")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

/// A path, as it goes into a flake reference.
fn flake_url(dir: &Path) -> String {
    let mut url = String::from("path:");
    for byte in dir.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(byte as char)
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// The nix expression that evaluates the flake in `dir` to an [`Evaluation`].
///
/// Every output is evaluated within `tryEval`, so that one that throws shows up as `null` and
/// doesn't take the others with it.
fn expression(dir: &Path) -> String {
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
  toolsOf = shell:
    if !(builtins.isAttrs shell) then null
    else builtins.listToAttrs (builtins.concatMap
      (input:
        let found = guardDeep (tool input);
        in if found == null then [ ] else found)
      (flatten (map (attribute: shell.${attribute} or [ ]) [
        "nativeBuildInputs"
        "buildInputs"
        "propagatedNativeBuildInputs"
        "propagatedBuildInputs"
      ])));
in {
  inherit system;
  devShellTools = builtins.listToAttrs (map
    (name: { inherit name; value = guardDeep (toolsOf flake.devShells.${system}.${name}); })
    (names (flake.devShells.${system} or { })));
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

    format!(
        "let flake = builtins.getFlake {}; in {}",
        nix_string(&flake_url(dir)),
        BODY.trim_start()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::repository::test_support::MockSnapshot;

    fn evaluation(outputs: &[(&str, Option<&str>)]) -> Evaluation {
        Evaluation {
            system: "x86_64-linux".to_string(),
            outputs: outputs
                .iter()
                .map(|(name, drv)| (name.to_string(), drv.map(str::to_string)))
                .collect(),
            dev_shell_tools: Default::default(),
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

    #[test]
    fn urls_and_strings_are_escaped() {
        assert_eq!(
            flake_url(Path::new("/tmp/work dir/a#b")),
            "path:/tmp/work%20dir/a%23b"
        );
        assert_eq!(nix_string(r#"a"b${c}\"#), r#""a\"b\${c}\\""#);
    }

    #[test]
    fn paths_outside_the_directory_are_refused() {
        assert!(is_contained("nix/default.nix"));
        assert!(!is_contained("../escape"));
        assert!(!is_contained("/etc/passwd"));
        assert!(!is_contained("a/../../b"));
    }

    fn nix_works() -> bool {
        std::process::Command::new("nix")
            .args([
                "--extra-experimental-features",
                "nix-command flakes",
                "eval",
                "--impure",
                "--expr",
                "builtins.currentSystem",
            ])
            .output()
            .is_ok_and(|output| output.status.success())
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
      nixosConfigurations.host-a.config.system.build.toplevel = build "host-a" "echo a > $out";
      lib.not-a-derivation = 1;
    };
}
"#;

    #[tokio::test]
    async fn evaluates_and_compares_a_real_flake() {
        if !nix_works() {
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
        materialize(&snapshot, dir.path()).await.unwrap();

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
                "devShells.agent",
                "devShells.default",
                "formatter",
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
    }
}
