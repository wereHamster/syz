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

/// Sends the application the messages to audit every project.
///
/// This runs outside of the application loop on purpose: when the mailbox is full, the sends
/// wait for the loop to make room, which it could not do if the loop were sending them itself.
async fn audit_all_projects(handle: &Handle) -> anyhow::Result<()> {
    // Responses are cached until purged, so without this the audits would work with the
    // registry data of the first time they were fetched. The mailbox is processed in order,
    // so this is done before the audits are started.
    handle.send(pk(), Payload::Purge).await?;

    for project in handle.store().list_projects().await? {
        handle
            .send(
                pk(),
                Payload::UpdateVulnerableDependencies {
                    project_id: project.id.clone(),
                },
            )
            .await?;
        handle
            .send(
                pk(),
                Payload::PruneBranches {
                    project_id: project.id,
                },
            )
            .await?;
    }

    Ok(())
}

/// Asks the application to audit all projects once a day. Runs for the life of the process.
pub async fn run(handle: Handle) {
    let mut after = Utc::now();

    loop {
        let next = next_audit(after);
        tracing::info!("Next scheduled audit at {}", next);

        let wait = (next - Utc::now()).to_std().unwrap_or_default();
        tokio::time::sleep(wait).await;

        if let Err(e) = audit_all_projects(&handle).await {
            tracing::error!("Failed to schedule the audit of all projects: {:#}", e);
        }

        // Continue from the run just done, so that waking up slightly early cannot repeat it,
        // and from now if the wake up was late, so that missed days are skipped, not run
        // back to back.
        after = next.max(Utc::now());
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
