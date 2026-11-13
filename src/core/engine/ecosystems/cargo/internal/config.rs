use toml_edit::DocumentMut;

use crate::core::engine::repository::ProjectRepositorySnapshot;

/// The minimum publish age configured for crates.io in the repository's own
/// `.cargo/config.toml`.
///
/// Only the repository's config file is read, not the Cargo configuration of the machine
/// running syzd, so that the policy is defined by the project. Values follow Cargo's lookup:
/// `registries.crates-io.min-publish-age` wins over `registry.global-min-publish-age`.
///
/// Returns `None` when neither key is set, the value is `"0"`, or it can't be parsed.
pub async fn minimum_publish_age(repo: &dyn ProjectRepositorySnapshot) -> Option<chrono::Duration> {
    let config = read_config(repo).await?;
    crates_io_min_publish_age(&config)
        .and_then(parse_time_span)
        .filter(|age| !age.is_zero())
}

/// Environment variables that apply the repository's minimum publish age to a Cargo command,
/// overriding any value from the machine's own Cargo configuration.
pub fn min_publish_age_env(age: Option<chrono::Duration>) -> [(&'static str, String); 2] {
    let value = match age {
        Some(age) => format!("{} seconds", age.num_seconds()),
        None => "0".to_string(),
    };
    [
        ("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", value.clone()),
        ("CARGO_REGISTRIES_CRATES_IO_MIN_PUBLISH_AGE", value),
    ]
}

async fn read_config(repo: &dyn ProjectRepositorySnapshot) -> Option<DocumentMut> {
    // Cargo prefers the legacy extension-less file when both exist.
    for path in [".cargo/config", ".cargo/config.toml"] {
        if let Ok(content) = repo.read_file(path).await {
            return match content.parse::<DocumentMut>() {
                Ok(doc) => Some(doc),
                Err(_) => {
                    tracing::warn!("Ignoring invalid cargo config: {path}");
                    None
                }
            };
        }
    }
    None
}

fn crates_io_min_publish_age(config: &DocumentMut) -> Option<&str> {
    config
        .get("registries")
        .and_then(|r| r.get("crates-io"))
        .and_then(|r| r.get("min-publish-age"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            config
                .get("registry")
                .and_then(|r| r.get("global-min-publish-age"))
                .and_then(|v| v.as_str())
        })
}

/// Parses Cargo's time span format: `"0"` or `"<integer> <unit>"`.
fn parse_time_span(value: &str) -> Option<chrono::Duration> {
    let value = value.trim();
    if value == "0" {
        return Some(chrono::Duration::zero());
    }
    let (amount, unit) = value.split_once(' ')?;
    let amount: i64 = amount.parse().ok()?;
    let seconds_per_unit = match unit.trim() {
        "second" | "seconds" => 1,
        "minute" | "minutes" => 60,
        "hour" | "hours" => 60 * 60,
        "day" | "days" => 24 * 60 * 60,
        "week" | "weeks" => 7 * 24 * 60 * 60,
        // Cargo's definition: the average Gregorian month (30.436875 days).
        "month" | "months" => 2_629_746,
        _ => return None,
    };
    chrono::Duration::try_seconds(amount.checked_mul(seconds_per_unit)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::repository::test_support::MockSnapshot;

    #[test]
    fn parses_cargo_time_spans() {
        assert_eq!(parse_time_span("0"), Some(chrono::Duration::zero()));
        assert_eq!(parse_time_span("3 days"), Some(chrono::Duration::days(3)));
        assert_eq!(parse_time_span("1 week"), Some(chrono::Duration::weeks(1)));
        assert_eq!(
            parse_time_span("90 minutes"),
            Some(chrono::Duration::minutes(90))
        );
        assert_eq!(parse_time_span("3"), None);
        assert_eq!(parse_time_span("3 fortnights"), None);
        assert_eq!(parse_time_span("-"), None);
    }

    #[tokio::test]
    async fn reads_global_min_publish_age() {
        let repo = MockSnapshot::with_files(&[(
            ".cargo/config.toml",
            "[registry]\nglobal-min-publish-age = \"3 days\"\n",
        )]);
        assert_eq!(
            minimum_publish_age(&repo).await,
            Some(chrono::Duration::days(3))
        );
    }

    #[tokio::test]
    async fn crates_io_setting_overrides_global() {
        let repo = MockSnapshot::with_files(&[(
            ".cargo/config.toml",
            "[registry]\nglobal-min-publish-age = \"3 days\"\n\n[registries.crates-io]\nmin-publish-age = \"2 weeks\"\n",
        )]);
        assert_eq!(
            minimum_publish_age(&repo).await,
            Some(chrono::Duration::weeks(2))
        );
    }

    #[tokio::test]
    async fn zero_or_missing_means_no_minimum() {
        let zero = MockSnapshot::with_files(&[(
            ".cargo/config.toml",
            "[registry]\nglobal-min-publish-age = \"0\"\n",
        )]);
        assert_eq!(minimum_publish_age(&zero).await, None);

        let missing = MockSnapshot::with_files(&[("Cargo.toml", "[package]\nname = \"foo\"\n")]);
        assert_eq!(minimum_publish_age(&missing).await, None);
    }

    #[tokio::test]
    async fn extensionless_config_takes_precedence() {
        let repo = MockSnapshot::with_files(&[
            (
                ".cargo/config",
                "[registry]\nglobal-min-publish-age = \"1 day\"\n",
            ),
            (
                ".cargo/config.toml",
                "[registry]\nglobal-min-publish-age = \"5 days\"\n",
            ),
        ]);
        assert_eq!(
            minimum_publish_age(&repo).await,
            Some(chrono::Duration::days(1))
        );
    }
}
