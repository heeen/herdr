pub(crate) const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) fn display_version() -> String {
    display_version_with(option_env!("HERDR_BUILD_COMMIT"))
}

fn display_version_with(build_commit: Option<&str>) -> String {
    match build_commit
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(commit) => format!("{PACKAGE_VERSION} ({commit})"),
        None => PACKAGE_VERSION.to_string(),
    }
}

pub(crate) fn version_line_matches_current(line: &str) -> bool {
    version_line_matches_package(line, PACKAGE_VERSION)
}

fn version_line_matches_package(line: &str, package_version: &str) -> bool {
    let trimmed = line.trim();
    let exact = format!("herdr {package_version}");
    trimmed == exact
        || trimmed
            .strip_prefix(&(exact + " ("))
            .is_some_and(|suffix| suffix.ends_with(')'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_version_includes_build_commit_when_present() {
        assert_eq!(
            display_version_with(Some("abc123")),
            format!("{PACKAGE_VERSION} (abc123)")
        );
    }

    #[test]
    fn display_version_omits_empty_build_commit() {
        assert_eq!(display_version_with(Some(" ")), PACKAGE_VERSION);
        assert_eq!(display_version_with(None), PACKAGE_VERSION);
    }

    #[test]
    fn current_version_line_allows_commit_suffix() {
        assert!(version_line_matches_package("herdr 0.6.4", "0.6.4"));
        assert!(version_line_matches_package(
            "herdr 0.6.4 (abc123-dirty)",
            "0.6.4"
        ));
        assert!(!version_line_matches_package(
            "herdr 0.6.3 (abc123)",
            "0.6.4"
        ));
        assert!(!version_line_matches_package(
            "other 0.6.4 (abc123)",
            "0.6.4"
        ));
    }
}
