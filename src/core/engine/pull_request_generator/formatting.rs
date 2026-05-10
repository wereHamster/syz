use chrono::{DateTime, Utc};

pub fn format_duration(duration: chrono::Duration) -> String {
    let seconds = duration.num_seconds().abs();

    if seconds < 60 {
        "less than a minute".to_string()
    } else if seconds < 3600 {
        let minutes = duration.num_minutes().abs();
        if minutes == 1 {
            "1 minute".to_string()
        } else {
            format!("{} minutes", minutes)
        }
    } else if seconds < 86400 {
        let hours = duration.num_hours().abs();
        if hours == 1 {
            "1 hour".to_string()
        } else {
            format!("{} hours", hours)
        }
    } else if seconds < 2592000 {
        let days = duration.num_days().abs();
        if days == 1 {
            "1 day".to_string()
        } else {
            format!("{} days", days)
        }
    } else if seconds < 31536000 {
        let months = duration.num_days().abs() / 30;
        if months == 1 {
            "1 month".to_string()
        } else {
            format!("{} months", months)
        }
    } else {
        let years = duration.num_days().abs() / 365;
        if years == 1 {
            "1 year".to_string()
        } else {
            format!("{} years", years)
        }
    }
}

pub fn format_time_ago(time: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(time);
    if duration.num_seconds() < 60 {
        "Just now".to_string()
    } else {
        format!("{} ago", format_duration(duration))
    }
}
