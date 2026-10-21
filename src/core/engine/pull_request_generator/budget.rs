//! How much room each part of a pull request description gets.
//!
//! The description as a whole has a limit. It is split first by kind of section, then among
//! the sections of that kind, which come from all the ecosystems the pull request touches. A
//! section gets its share as a budget and has to stay within it, cutting its own content in a
//! way that suits that content.

/// What the description may add up to. GitHub rejects bodies over 65,536 characters.
pub const BODY_LIMIT: usize = 60_000;

/// The part of the description a section belongs to. The parts appear in this order, no matter
/// which ecosystem a section is for, so the summaries of all ecosystems are at the very top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    /// What the pull request updates.
    Summary,
    /// Alerts about the update: policy, advisories.
    Notices,
    /// What the update does to the project, as far as it can be told without reading the
    /// changes: the outputs it affects, and the tools of the dev shells it changes.
    Impact,
    /// The machines the flake describes, and what the update does to them.
    System,
    /// Release notes of the versions the update skips over.
    History,
}

impl SectionKind {
    pub const IN_ORDER: [SectionKind; 5] = [
        SectionKind::Summary,
        SectionKind::Notices,
        SectionKind::Impact,
        SectionKind::System,
        SectionKind::History,
    ];

    /// The most all sections of this kind may take together, so that the kinds after it
    /// aren't squeezed out. History takes whatever is left.
    fn cap(self) -> usize {
        match self {
            SectionKind::Summary => 5_000,
            SectionKind::Notices => 6_000,
            SectionKind::Impact => 4_000,
            SectionKind::System => 4_000,
            SectionKind::History => usize::MAX,
        }
    }

    /// The budget for all sections of this kind, with `remaining` left of the description.
    pub fn budget(self, remaining: usize) -> usize {
        remaining.min(self.cap())
    }
}

/// Fits as many whole `items` as the budget allows between `prefix` and `suffix`.
///
/// When items have to be dropped, `omitted` gives the note that takes their place. The note is
/// part of what has to fit, so it is never cut off. Returns `None` if not even the prefix, the
/// note and the suffix fit.
pub fn fit_items(
    prefix: &str,
    items: &[String],
    suffix: &str,
    budget: usize,
    omitted: impl Fn(usize) -> String,
) -> Option<String> {
    let fixed = prefix.len() + suffix.len();
    let mut kept = items.len();
    let mut items_len: usize = items.iter().map(String::len).sum();

    loop {
        let note_len = if kept < items.len() {
            omitted(items.len() - kept).len()
        } else {
            0
        };

        if fixed + items_len + note_len <= budget {
            break;
        }
        if kept == 0 {
            return None;
        }

        kept -= 1;
        items_len -= items[kept].len();
    }

    let mut out = String::with_capacity(fixed + items_len);
    out.push_str(prefix);
    items[..kept].iter().for_each(|item| out.push_str(item));
    if kept < items.len() {
        out.push_str(&omitted(items.len() - kept));
    }
    out.push_str(suffix);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("- item {i}\n")).collect()
    }

    fn omitted(n: usize) -> String {
        format!("- … and {n} more\n")
    }

    #[test]
    fn keeps_everything_that_fits() {
        let out = fit_items("head\n", &items(3), "\n", 1_000, omitted).unwrap();
        assert_eq!(out, "head\n- item 0\n- item 1\n- item 2\n\n");
    }

    #[test]
    fn drops_whole_trailing_items_and_says_how_many() {
        let out = fit_items("head\n", &items(12), "\n", 70, omitted).unwrap();

        assert!(out.len() <= 70, "{out}");
        assert!(out.starts_with("head\n- item 0\n"), "{out}");
        assert!(out.ends_with("more\n\n"), "{out}");
        let kept = out.matches("- item ").count();
        assert!(kept > 0 && kept < 12);
        assert!(out.contains(&format!("… and {} more", 12 - kept)), "{out}");
    }

    #[test]
    fn the_note_always_fits() {
        for budget in 0..400 {
            if let Some(out) = fit_items("head\n", &items(30), "\n", budget, omitted) {
                assert!(out.len() <= budget, "{budget}: {out}");
            }
        }
    }

    #[test]
    fn is_none_when_not_even_the_note_fits() {
        assert_eq!(fit_items("head\n", &items(3), "\n", 10, omitted), None);
    }

    #[test]
    fn kinds_are_capped_by_what_is_left() {
        assert_eq!(SectionKind::Summary.budget(100_000), 5_000);
        assert_eq!(SectionKind::Summary.budget(300), 300);
        assert_eq!(SectionKind::Impact.budget(100_000), 4_000);
        assert_eq!(SectionKind::System.budget(100_000), 4_000);
        assert_eq!(SectionKind::History.budget(42_000), 42_000);
    }
}
