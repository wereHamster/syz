use crate::core::engine::UpdateTarget;
use anyhow::Result;

pub trait PullRequestGenerator: Send + Sync {
    fn title(
        &self,
        package_group: &str,
        targets: &[UpdateTarget],
        is_major: bool,
    ) -> Result<String>;

    fn body(&self, package_group: &str, targets: &[UpdateTarget], is_major: bool)
        -> Result<String>;
}
