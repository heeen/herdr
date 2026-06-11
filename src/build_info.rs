//! Build identity helpers.

pub const BASE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The full user-facing version (base + channel/build-id suffix), computed by build.rs
/// so it is available as a compile-time constant. This is exactly what
/// `herdr --version` reports; version-parity checks must compare against this, never
/// against the bare `CARGO_PKG_VERSION` (suffixed builds would reject themselves).
pub const FULL_VERSION: &str = env!("HERDR_FULL_VERSION");

pub fn channel() -> &'static str {
    non_empty(option_env!("HERDR_BUILD_CHANNEL")).unwrap_or("stable")
}

pub fn build_id() -> Option<&'static str> {
    non_empty(option_env!("HERDR_BUILD_ID"))
}

pub fn version() -> String {
    FULL_VERSION.to_string()
}

pub fn is_preview() -> bool {
    channel() == "preview"
}

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn stable_version_defaults_to_cargo_version() {
        assert!(!super::version().is_empty());
    }

    #[test]
    fn full_version_starts_with_base_version() {
        assert!(super::FULL_VERSION.starts_with(super::BASE_VERSION));
        assert_eq!(super::version(), super::FULL_VERSION);
    }
}
