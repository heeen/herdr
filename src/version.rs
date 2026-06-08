pub(crate) const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

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
