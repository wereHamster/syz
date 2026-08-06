use std::collections::HashMap;

use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct FlakeLock {
    pub(super) nodes: HashMap<String, FlakeNode>,
    pub(super) root: String,
}

#[derive(Deserialize, Default)]
pub(super) struct FlakeNode {
    #[serde(default)]
    pub(super) inputs: HashMap<String, serde_json::Value>,
    pub(super) original: Option<FlakeRef>,
    pub(super) locked: Option<FlakeRef>,
}

#[derive(Deserialize)]
pub(super) struct FlakeRef {
    #[serde(rename = "type", default)]
    pub(super) ref_type: String,
    pub(super) owner: Option<String>,
    pub(super) repo: Option<String>,
    #[serde(rename = "ref")]
    pub(super) git_ref: Option<String>,
    pub(super) rev: Option<String>,
}

pub(super) struct GithubRootInput<'a> {
    pub(super) alias: &'a str,
    pub(super) owner: &'a str,
    pub(super) repo: &'a str,
    pub(super) git_ref: Option<&'a str>,
}

/// Direct (non-`follows`) root inputs whose `original` is a github ref, paired with their local alias.
pub(super) fn github_root_inputs(lock: &FlakeLock) -> Vec<GithubRootInput<'_>> {
    let mut result = Vec::new();
    let root_node = match lock.nodes.get(&lock.root) {
        Some(n) => n,
        None => return result,
    };

    for (alias, input_ref) in &root_node.inputs {
        // Only direct node references are handled; "follows" chains (array values) alias
        // another node's lock rather than owning one, so they're skipped.
        let node_name = match input_ref.as_str() {
            Some(n) => n,
            None => continue,
        };

        let node = match lock.nodes.get(node_name) {
            Some(n) => n,
            None => continue,
        };

        let original = match &node.original {
            Some(o) => o,
            None => continue,
        };

        if original.ref_type != "github" {
            continue;
        }

        let (owner, repo) = match (&original.owner, &original.repo) {
            (Some(o), Some(r)) => (o.as_str(), r.as_str()),
            _ => continue,
        };

        result.push(GithubRootInput {
            alias,
            owner,
            repo,
            git_ref: original.git_ref.as_deref(),
        });
    }

    result
}
