use crate::core::engine::UpdateTarget;

pub fn generate_title(
    package_group: &str,
    targets: &[UpdateTarget],
    is_major: bool,
) -> String {
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

    title
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::{RequirementVersion, PackageInfo};

    fn make_target(name: &str, current: &str, target: &str) -> UpdateTarget {
        UpdateTarget {
            name: name.to_string(),
            current_version: RequirementVersion {
                requirement: current.to_string(),
                version: current.to_string(),
            },
            target_version: RequirementVersion {
                requirement: target.to_string(),
                version: target.to_string(),
            },
            latest_version: target.to_string(),
            package_info: PackageInfo { repo_url: None },
            minimum_release_age: None,
        }
    }

    #[test]
    fn test_generate_title_single_target() {
        let targets = vec![make_target("react", "17.0.0", "18.0.0")];
        let title = generate_title("React", &targets, false);
        assert_eq!(title, "Update React to 18.0.0");

        let title_major = generate_title("React", &targets, true);
        assert_eq!(title_major, "Update React to 18.0.0 (major)");
    }

    #[test]
    fn test_generate_title_multiple_targets_same_version() {
        let targets = vec![
            make_target("react", "17.0.0", "18.0.0"),
            make_target("react-dom", "17.0.0", "18.0.0"),
        ];
        let title = generate_title("React", &targets, false);
        assert_eq!(title, "Update React to 18.0.0");
    }

    #[test]
    fn test_generate_title_multiple_targets_different_versions() {
        let targets = vec![
            make_target("babel-core", "7.0.0", "7.1.0"),
            make_target("babel-cli", "7.0.0", "7.2.5"),
            make_target("babel-preset-env", "7.0.0", "7.0.5"),
        ];
        let title = generate_title("Babel", &targets, false);
        assert_eq!(title, "Update Babel to 7.0.5 ~ 7.2.5");
    }
}
