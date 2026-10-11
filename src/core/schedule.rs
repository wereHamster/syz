use chrono::{DateTime, Duration, Utc};

use super::application::Handle;
use super::database::pk;
use super::message::Payload;

/// The time of day (UTC) at which every project is audited for vulnerable dependencies.
const AUDIT_HOUR: u32 = 4;
const AUDIT_MINUTE: u32 = 27;

/// The first scheduled audit strictly after `after`.
fn next_audit(after: DateTime<Utc>) -> DateTime<Utc> {
    let today = after
        .date_naive()
        .and_hms_opt(AUDIT_HOUR, AUDIT_MINUTE, 0)
        .expect("the audit time is a valid time of day")
        .and_utc();

    if today > after {
        today
    } else {
        today + Duration::days(1)
    }
}

/// Asks the application to audit all projects once a day. Runs for the life of the process.
pub async fn run(handle: Handle) {
    let mut after = Utc::now();

    loop {
        let next = next_audit(after);
        tracing::info!("Next scheduled audit at {}", next);

        let wait = (next - Utc::now()).to_std().unwrap_or_default();
        tokio::time::sleep(wait).await;

        if let Err(e) = handle
            .send(pk(), Payload::UpdateAllProjectsVulnerableDependencies)
            .await
        {
            tracing::error!("Failed to schedule the audit of all projects: {}", e);
        }

        // Continue from the run just done, so that waking up slightly early cannot repeat it.
        after = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn before_the_audit_time_the_next_audit_is_today() {
        assert_eq!(
            next_audit(at("2026-10-11T03:00:00Z")),
            at("2026-10-11T04:27:00Z")
        );
    }

    #[test]
    fn after_the_audit_time_the_next_audit_is_tomorrow() {
        assert_eq!(
            next_audit(at("2026-10-11T05:00:00Z")),
            at("2026-10-12T04:27:00Z")
        );
    }

    #[test]
    fn at_the_audit_time_the_next_audit_is_tomorrow() {
        assert_eq!(
            next_audit(at("2026-10-11T04:27:00Z")),
            at("2026-10-12T04:27:00Z")
        );
    }

    #[test]
    fn rolls_over_the_end_of_the_month() {
        assert_eq!(
            next_audit(at("2026-10-31T23:59:00Z")),
            at("2026-11-01T04:27:00Z")
        );
    }
}
