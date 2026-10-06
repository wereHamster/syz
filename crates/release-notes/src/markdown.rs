//! Pure markdown post-processing for release notes: section extraction from
//! changelog files, heading re-leveling, and sanitization for embedding in a
//! GitHub PR body.
//!
//! Semantics are ported from the legacy
//! `src/core/clients/github_release_notes.rs` implementation, with two
//! documented deltas:
//! - section extraction matches the version on **word boundaries** (legacy
//!   used substring matching, so `## 12.0.0` falsely matched `2.0.0`);
//! - regexes in [`sanitize`] are compiled once instead of per call.

use std::sync::LazyLock;

/// Extract the section of `changelog` belonging to `version`: the ATX heading
/// whose text contains the version as a word-boundary match (`## v2.0.0`,
/// `## 2.0.0 (2024-01-01)` match `2.0.0`; `## 12.0.0` does not), then all
/// lines until the next heading of the same or lower level.
///
/// Legacy semantics otherwise preserved, including manual `#`-counting
/// heading detection (`#`s followed by a space; not inside code fences —
/// the legacy extractor did not track fences and this preserves that).
pub fn extract_section(changelog: &str, version: &str) -> Option<String> {
    let base = crate::base_version(version);
    let mut extracted = String::new();
    let mut found = false;
    let mut target_level = 0;

    for line in changelog.lines() {
        let trimmed = line.trim();
        let Some((count, text)) = heading_of(trimmed) else {
            if found {
                extracted.push_str(line);
                extracted.push('\n');
            }
            continue;
        };

        if found {
            if count <= target_level {
                break;
            }
            extracted.push_str(line);
            extracted.push('\n');
        } else if contains_word(text, version) || contains_word(text, base) {
            found = true;
            target_level = count;
        }
    }

    if found {
        let section = extracted.trim().to_string();
        if section.is_empty() {
            None
        } else {
            Some(section)
        }
    } else {
        None
    }
}

/// Re-level headings so the shallowest starts at `target_level` (shifting up
/// by prepending `#`, shifting down by stripping leading `#`), code-block
/// aware (``` toggles).
///
/// Headings whose text contains any needle in `strip_heading_containing` are
/// removed — the legacy "skip the version heading" behavior, made explicit.
/// Legacy detail preserved: only a matching heading at the *start of the
/// input* is stripped (with any empty lines before it); a version heading
/// further down is left in place. When stripping leaves only an empty
/// remainder, returns the empty string (the legacy `skipped_first_heading`
/// branch).
pub fn shift_headings(md: &str, target_level: usize, strip_heading_containing: &[&str]) -> String {
    // Drop the version heading only when it is the first non-empty line
    // (legacy behavior), dropping any empty lines before it with it.
    let mut remaining: Vec<&str> = Vec::new();
    let mut stripped_title = false;
    let mut lines = md.lines().peekable();

    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((_, text)) = heading_of(trimmed) {
            if strip_heading_containing
                .iter()
                .any(|needle| text.contains(needle))
            {
                stripped_title = true;
            } else {
                remaining.push(line);
            }
        } else {
            remaining.push(line);
        }
        break;
    }
    remaining.extend(lines);

    // Find the shallowest heading level (outside code blocks).
    let mut min_level = usize::MAX;
    let mut in_code_block = false;
    for line in &remaining {
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if !in_code_block {
            if let Some((count, _)) = heading_of(line.trim()) {
                min_level = min_level.min(count);
            }
        }
    }

    let (shift_up_by, shift_down_by) = match min_level {
        usize::MAX => (0, 0),
        min if min < target_level => (target_level - min, 0),
        min => (0, min - target_level),
    };
    let shift_str = "#".repeat(shift_up_by);

    let mut out = String::with_capacity(md.len() + 100);
    let mut in_code_block = false;
    for line in &remaining {
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if !in_code_block {
            if let Some((count, _)) = heading_of(line.trim()) {
                if shift_up_by > 0 {
                    out.push_str(&shift_str);
                    out.push_str(line);
                } else if shift_down_by > 0 && count > shift_down_by {
                    out.push_str(&line[shift_down_by..]);
                } else {
                    out.push_str(line);
                }
                out.push('\n');
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    let final_out = out.trim().to_string();
    if final_out.is_empty() && stripped_title {
        return String::new();
    }

    final_out
}

static RE_MENTION: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"@([a-zA-Z0-9-]+)").unwrap());
static RE_ISSUE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"#([0-9]+)").unwrap());

/// Rewrite `github.com` links to `redirect.github.com`; escape `@mention` and
/// `#123` references with a zero-width space so GitHub does not auto-link
/// them inside PR bodies.
///
/// Semantics preserved exactly from the legacy implementation, including the
/// escaping inside code spans (documented legacy behavior: the test vector
/// expects `` `code #222` `` to become `` `code #&#8203;222` ``).
pub fn sanitize(md: &str) -> String {
    let mut out = md
        .replace("https://github.com/", "https://redirect.github.com/")
        .replace("http://github.com/", "https://redirect.github.com/");

    out = RE_MENTION
        .replace_all(&out, |caps: &regex::Captures| {
            let m = caps.get(0).unwrap();
            let (start, end) = (m.start(), m.end());

            let mut skip = false;
            if start > 0 {
                if let Some(prev) = out[..start].chars().next_back() {
                    if prev.is_ascii_alphanumeric() || prev == '_' {
                        skip = true;
                    }
                }
            }
            if let Some(next) = out[end..].chars().next() {
                if next == '/' {
                    skip = true;
                }
            }

            if skip {
                caps[0].to_string()
            } else {
                format!("@&#8203;{}", &caps[1])
            }
        })
        .to_string();

    out = RE_ISSUE
        .replace_all(&out, |caps: &regex::Captures| {
            let m = caps.get(0).unwrap();
            let prefix = &out[..m.start()];

            let mut skip = false;
            if let Some(prev) = prefix.chars().next_back() {
                if prev == '_' || prev == '&' {
                    skip = true;
                }
            }
            if prefix.ends_with("](")
                || prefix.ends_with("href=\"")
                || prefix.ends_with("href='")
                || prefix.ends_with("=\"")
                || prefix.ends_with("='")
            {
                skip = true;
            }

            if skip {
                caps[0].to_string()
            } else {
                format!("#&#8203;{}", &caps[1])
            }
        })
        .to_string();

    out
}

/// Count leading `#`s followed by a space, returning `(level, text)`.
/// Legacy heading detection: `#`s, then a space, then text; a run of `#`s
/// not followed by a space is not a heading.
fn heading_of(trimmed: &str) -> Option<(usize, &str)> {
    let count = trimmed.chars().take_while(|c| *c == '#').count();
    if count == 0 {
        return None;
    }
    let rest = &trimmed[count..];
    if rest.starts_with(' ') || rest.is_empty() {
        Some((count, rest.trim_start()))
    } else {
        None
    }
}

/// Version containment with *numeric* boundaries: the characters
/// immediately before/after the match must not extend the version number
/// itself (digit or `.`). `## v2.0.0` and `## 2.0.0 (2024-01-01)` match a
/// needle `2.0.0`; `## 12.0.0` does not (the `1` extends the number).
fn contains_word(text: &str, needle: &str) -> bool {
    let extends_version = |c: char| c.is_ascii_digit() || c == '.';
    let mut start = 0;
    while let Some(pos) = text[start..].find(needle) {
        let abs = start + pos;
        let end = abs + needle.len();

        let before = text[..abs].chars().next_back();
        let after = text[end..].chars().next();

        let bounded_before = before.map(|c| !extends_version(c)).unwrap_or(true);
        let bounded_after = after.map(|c| !extends_version(c)).unwrap_or(true);

        if bounded_before && bounded_after {
            return true;
        }
        start = abs + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_legacy_vector() {
        let input = "Fixed by @alice and @bob in #123. See also https://github.com/foo/bar. @apollo/server #456 [link](#789) <a href=\"#000\">#111</a> `code #222` `code @user` owner/repo#999";
        let expected = "Fixed by @&#8203;alice and @&#8203;bob in #&#8203;123. See also https://redirect.github.com/foo/bar. @apollo/server #&#8203;456 [link](#789) <a href=\"#000\">#&#8203;111</a> `code #&#8203;222` `code @&#8203;user` owner/repo#&#8203;999";
        assert_eq!(sanitize(input), expected);
    }

    #[test]
    fn extract_section_level_stop() {
        let changelog = "# Changelog\n\n## 1.0.0\n\n### Fixed\n\n- bug\n\n## 0.9.0\n\n- old\n";
        let section = extract_section(changelog, "1.0.0").unwrap();
        assert!(section.contains("### Fixed"));
        assert!(section.contains("- bug"));
        assert!(!section.contains("0.9.0"));
        assert!(!section.contains("# Changelog"));
    }

    #[test]
    fn extract_section_word_boundary() {
        let changelog = "## 12.0.0\n\n- new\n\n## 2.0.0\n\n- old\n";
        assert!(extract_section(changelog, "2.0.0").is_some());
        let section = extract_section(changelog, "2.0.0").unwrap();
        assert!(section.contains("- old"));
        assert!(!section.contains("- new"));
    }

    #[test]
    fn extract_section_v_prefix_and_parens() {
        assert!(extract_section("## v2.0.0\n\n- x\n", "2.0.0").is_some());
        assert!(extract_section("## 2.0.0 (2024-01-01)\n\n- x\n", "2.0.0").is_some());
    }

    #[test]
    fn extract_section_base_version_match() {
        // Requested "2.0.0+build"; changelog heading is "2.0.0".
        assert!(extract_section("## 2.0.0\n\n- x\n", "2.0.0+build").is_some());
    }

    #[test]
    fn extract_section_missing_version() {
        assert!(extract_section("# Changelog\n\n## 1.0.0\n\n- x\n", "9.9.9").is_none());
    }

    #[test]
    fn extract_section_empty_body_returns_none() {
        // Heading found but nothing under it.
        assert!(extract_section("## 1.0.0\n## 0.9.0\n- x\n", "1.0.0").is_none());
    }

    #[test]
    fn shift_headings_up() {
        let md = "### Fixed\n\n- bug";
        assert_eq!(shift_headings(md, 3, &[]), "### Fixed\n\n- bug");
        assert_eq!(shift_headings(md, 4, &[]), "#### Fixed\n\n- bug");
        assert_eq!(shift_headings(md, 2, &[]), "## Fixed\n\n- bug");
    }

    #[test]
    fn shift_headings_down() {
        let md = "## Header\n\n### Fixed\n\n- bug";
        let out = shift_headings(md, 3, &[]);
        assert!(out.starts_with("### Header"));
        assert!(out.contains("#### Fixed"));
    }

    #[test]
    fn shift_headings_strips_title() {
        let md = "## 2.0.0\n\n### Fixed\n\n- bug\n";
        let out = shift_headings(md, 3, &["2.0.0"]);
        // Heading target 3 nests under the PR section's `##` header, so the
        // remaining headings stay at `###`.
        assert!(out.starts_with("### Fixed"));
        assert!(!out.contains("2.0.0"));
    }

    #[test]
    fn shift_headings_strips_only_leading_title() {
        // Version heading NOT first: legacy leaves it in place.
        let md = "Some preamble\n\n## 2.0.0\n\n### Fixed\n";
        let out = shift_headings(md, 3, &["2.0.0"]);
        assert!(out.contains("## 2.0.0"));
        assert!(out.contains("Some preamble"));
    }

    #[test]
    fn shift_headings_code_blocks_untouched() {
        let md = "## 2.0.0\n\n### Fixed\n\n```rust\n## not a heading\n```\n";
        let out = shift_headings(md, 3, &["2.0.0"]);
        assert!(out.contains("## not a heading"));
    }

    #[test]
    fn shift_headings_empty_after_strip() {
        // Only the version heading exists; after stripping, empty.
        assert_eq!(shift_headings("## 2.0.0", 3, &["2.0.0"]), "");
    }
}
