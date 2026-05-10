use crate::core::engine::UpdateTarget;
use anyhow::Result;

pub trait PullRequestGenerator: Send + Sync {
    fn generate_pull_request_title(
        &self,
        package_group: &str,
        targets: &[UpdateTarget],
        is_major: bool,
    ) -> Result<String>;

    fn generate_pull_request_body(
        &self,
        package_group: &str,
        targets: &[UpdateTarget],
        is_major: bool,
    ) -> Result<String>;
}

pub struct DefaultPullRequestGenerator;

impl PullRequestGenerator for DefaultPullRequestGenerator {
    fn generate_pull_request_title(
        &self,
        package_group: &str,
        targets: &[UpdateTarget],
        is_major: bool,
    ) -> Result<String> {
        let mut title = if targets.len() == 1 {
            let target = &targets[0];
            let clean_new = target.target_version.version.clone();
            format!("Update {} to {}", package_group, clean_new)
        } else {
            let first_new_req = &targets[0].target_version;
            let mut highest_new = &first_new_req.version;
            let mut lowest_new = &first_new_req.version;

            for t in targets {
                let current_ver_str = &t.target_version.version;
                let highest_ver_str = highest_new;
                let lowest_ver_str = lowest_new;

                if let (Ok(ver_new), Ok(ver_highest)) = (
                    semver::Version::parse(current_ver_str),
                    semver::Version::parse(highest_ver_str),
                ) {
                    if ver_new > ver_highest {
                        highest_new = &t.target_version.version;
                    }
                }

                if let (Ok(ver_new), Ok(ver_lowest)) = (
                    semver::Version::parse(current_ver_str),
                    semver::Version::parse(lowest_ver_str),
                ) {
                    if ver_new < ver_lowest {
                        lowest_new = &t.target_version.version;
                    }
                }
            }

            let clean_highest = highest_new;
            let clean_lowest = lowest_new;

            if clean_highest == clean_lowest {
                format!("Update {} to {}", package_group, clean_highest)
            } else {
                format!(
                    "Update {} to {} ~ {}",
                    package_group, clean_lowest, clean_highest
                )
            }
        };

        if is_major {
            title.push_str(" (major)");
        }

        Ok(title)
    }

    fn generate_pull_request_body(
        &self,
        package_group: &str,
        targets: &[UpdateTarget],
        _is_major: bool,
    ) -> Result<String> {
        let mut body = format!("Update {} dependencies.\n\n", package_group);

        for target in targets {
            body.push_str(&format!(
                "- `{}`: {} -> {}\n",
                target.name, target.current_version.version, target.target_version.version
            ));
        }

        Ok(body)
    }
}
