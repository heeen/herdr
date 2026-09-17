use std::collections::BTreeMap;

use serde::Deserialize;

use crate::platform::SelectionTarget;

const CLIPBOARD: &[SelectionTarget] = &[SelectionTarget::Clipboard];
const PRIMARY: &[SelectionTarget] = &[SelectionTarget::Primary];
const BOTH: &[SelectionTarget] = &[SelectionTarget::Clipboard, SelectionTarget::Primary];

/// Where a pane program's clipboard writes go, overriding the selection it addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardAgentRoute {
    Clipboard,
    Primary,
    Both,
}

impl ClipboardAgentRoute {
    fn targets(self) -> &'static [SelectionTarget] {
        match self {
            Self::Clipboard => CLIPBOARD,
            Self::Primary => PRIMARY,
            Self::Both => BOTH,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ClipboardConfig {
    /// Per-agent overrides keyed by strict canonical agent id.
    #[serde(deserialize_with = "deserialize_agent_routes")]
    pub agents: BTreeMap<String, ClipboardAgentRoute>,
}

impl ClipboardConfig {
    /// Selections to write a pane's clipboard write to. Without an override for the agent
    /// running in that pane, the selection the program addressed is kept.
    pub fn targets_for(
        &self,
        requested: SelectionTarget,
        agent: Option<&str>,
    ) -> &'static [SelectionTarget] {
        agent
            .and_then(crate::detect::parse_agent_label)
            .and_then(|agent| self.agents.get(crate::detect::agent_label(agent)))
            .map_or_else(
                || match requested {
                    SelectionTarget::Clipboard => CLIPBOARD,
                    SelectionTarget::Primary => PRIMARY,
                },
                |route| route.targets(),
            )
    }
}

fn deserialize_agent_routes<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, ClipboardAgentRoute>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let agents = BTreeMap::<String, ClipboardAgentRoute>::deserialize(deserializer)?;
    for id in agents.keys() {
        if crate::detect::parse_canonical_agent_label(id).is_none() {
            return Err(serde::de::Error::custom(format!(
                "unknown canonical agent id `{id}` in clipboard agents"
            )));
        }
    }
    Ok(agents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> ClipboardConfig {
        toml::from_str(toml).unwrap()
    }

    const EXPECT_CLIPBOARD: [SelectionTarget; 1] = [SelectionTarget::Clipboard];
    const EXPECT_PRIMARY: [SelectionTarget; 1] = [SelectionTarget::Primary];
    const EXPECT_BOTH: [SelectionTarget; 2] =
        [SelectionTarget::Clipboard, SelectionTarget::Primary];

    #[test]
    fn writes_keep_their_selection_without_an_override() {
        let empty = ClipboardConfig::default();
        for agent in [None, Some("claude"), Some("not-an-agent")] {
            assert_eq!(
                empty.targets_for(SelectionTarget::Clipboard, agent),
                EXPECT_CLIPBOARD
            );
            assert_eq!(
                empty.targets_for(SelectionTarget::Primary, agent),
                EXPECT_PRIMARY
            );
        }
    }

    #[test]
    fn an_agent_override_replaces_the_requested_selection() {
        let both = config("agents = { claude = \"both\" }");
        assert_eq!(
            both.targets_for(SelectionTarget::Clipboard, Some("claude")),
            EXPECT_BOTH
        );
        assert_eq!(
            both.targets_for(SelectionTarget::Primary, Some("claude")),
            EXPECT_BOTH
        );
        // An alias resolves to the same canonical id the table is keyed by.
        assert_eq!(
            both.targets_for(SelectionTarget::Clipboard, Some("claude-code")),
            EXPECT_BOTH
        );
        // Other agents, and panes without one, are untouched.
        assert_eq!(
            both.targets_for(SelectionTarget::Clipboard, Some("codex")),
            EXPECT_CLIPBOARD
        );
        assert_eq!(
            both.targets_for(SelectionTarget::Primary, None),
            EXPECT_PRIMARY
        );

        let primary = config("agents = { codex = \"primary\" }");
        assert_eq!(
            primary.targets_for(SelectionTarget::Clipboard, Some("codex")),
            EXPECT_PRIMARY
        );
        let clipboard = config("agents = { codex = \"clipboard\" }");
        assert_eq!(
            clipboard.targets_for(SelectionTarget::Primary, Some("codex")),
            EXPECT_CLIPBOARD
        );
    }

    #[test]
    fn unknown_agent_ids_and_routes_are_rejected() {
        assert!(toml::from_str::<ClipboardConfig>("agents = { claude-code = \"both\" }").is_err());
        assert!(toml::from_str::<ClipboardConfig>("agents = { nosuch = \"both\" }").is_err());
        assert!(toml::from_str::<ClipboardConfig>("agents = { claude = \"selection\" }").is_err());
    }
}
