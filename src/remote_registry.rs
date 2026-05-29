use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RemoteRegistrySnapshot {
    #[serde(default)]
    pub remotes: Vec<RemoteDefinitionSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteDefinitionSnapshot {
    pub id: String,
    pub name: String,
    pub target: RemoteTargetSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default)]
    pub keybindings: RemoteKeybindingsSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteTargetSnapshot {
    Ssh {
        target: String,
    },
    Local {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteKeybindingsSnapshot {
    #[default]
    Local,
    Server,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteRegistryError {
    InvalidTarget,
    InvalidName,
    DuplicateName,
    DuplicateTarget,
    NotFound,
}

impl RemoteRegistryError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidTarget => "invalid_remote_target",
            Self::InvalidName => "invalid_remote_name",
            Self::DuplicateName => "duplicate_remote_name",
            Self::DuplicateTarget => "duplicate_remote_target",
            Self::NotFound => "remote_not_found",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::InvalidTarget => "remote target is invalid",
            Self::InvalidName => "remote name is invalid",
            Self::DuplicateName => "remote name already exists",
            Self::DuplicateTarget => "remote target already exists",
            Self::NotFound => "remote not found",
        }
    }
}

impl RemoteRegistrySnapshot {
    #[cfg(test)]
    pub fn add(
        &mut self,
        name: Option<String>,
        target: String,
        keybindings: RemoteKeybindingsSnapshot,
    ) -> Result<RemoteDefinitionSnapshot, RemoteRegistryError> {
        self.add_excluding_targets(name, target, keybindings, &[])
    }

    pub fn add_excluding_targets(
        &mut self,
        name: Option<String>,
        target: String,
        keybindings: RemoteKeybindingsSnapshot,
        excluded_targets: &[RemoteTargetSnapshot],
    ) -> Result<RemoteDefinitionSnapshot, RemoteRegistryError> {
        let target = RemoteTargetSnapshot::parse(&target)?;
        let name = normalize_name(name.unwrap_or_else(|| target.default_display_name()))?;
        if self.remotes.iter().any(|remote| remote.name == name) {
            return Err(RemoteRegistryError::DuplicateName);
        }

        let target_key = target.canonical_key();
        if excluded_targets
            .iter()
            .any(|excluded| excluded.canonical_key() == target_key)
        {
            return Err(RemoteRegistryError::DuplicateTarget);
        }

        if self
            .remotes
            .iter()
            .any(|remote| remote.target.canonical_key() == target_key)
        {
            return Err(RemoteRegistryError::DuplicateTarget);
        }

        let remote = RemoteDefinitionSnapshot {
            id: self.next_id(),
            name,
            target,
            session: None,
            keybindings,
        };
        self.remotes.push(remote.clone());
        Ok(remote)
    }

    pub fn remove(&mut self, remote_id: &str) -> Result<String, RemoteRegistryError> {
        let index = self
            .remotes
            .iter()
            .position(|remote| remote.id == remote_id)
            .ok_or(RemoteRegistryError::NotFound)?;
        Ok(self.remotes.remove(index).id)
    }

    pub fn rename(
        &mut self,
        remote_id: &str,
        name: String,
    ) -> Result<RemoteDefinitionSnapshot, RemoteRegistryError> {
        let name = normalize_name(name)?;
        if self
            .remotes
            .iter()
            .any(|remote| remote.id != remote_id && remote.name == name)
        {
            return Err(RemoteRegistryError::DuplicateName);
        }

        let remote = self
            .remotes
            .iter_mut()
            .find(|remote| remote.id == remote_id)
            .ok_or(RemoteRegistryError::NotFound)?;
        remote.name = name;
        Ok(remote.clone())
    }

    fn next_id(&self) -> String {
        let mut index = 1;
        loop {
            let id = format!("remote-{index}");
            if self.remotes.iter().all(|remote| remote.id != id) {
                return id;
            }
            index += 1;
        }
    }
}

impl RemoteTargetSnapshot {
    pub fn parse(input: &str) -> Result<Self, RemoteRegistryError> {
        let target = input.trim();
        if target.is_empty() {
            return Err(RemoteRegistryError::InvalidTarget);
        }

        if target == "localhost" {
            return Ok(Self::Local { session: None });
        }

        if let Some(session) = target.strip_prefix("local:") {
            let session = session.trim();
            if session.is_empty() {
                return Err(RemoteRegistryError::InvalidTarget);
            }
            let session = (session != "default").then(|| session.to_string());
            return Ok(Self::Local { session });
        }

        Ok(Self::Ssh {
            target: target.to_string(),
        })
    }

    pub fn canonical_key(&self) -> String {
        match self {
            Self::Ssh { target } => format!("ssh:{target}"),
            Self::Local { session } => {
                format!("local:{}", session.as_deref().unwrap_or("default"))
            }
        }
    }

    fn default_display_name(&self) -> String {
        match self {
            Self::Local { session } => session.clone().unwrap_or_else(|| "local".to_string()),
            Self::Ssh { target } => ssh_display_name(target),
        }
    }
}

fn normalize_name(name: String) -> Result<String, RemoteRegistryError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(RemoteRegistryError::InvalidName);
    }
    Ok(name.to_string())
}

fn ssh_display_name(target: &str) -> String {
    let without_scheme = target.strip_prefix("ssh://").unwrap_or(target);
    let without_user = without_scheme
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(without_scheme);
    let host = without_user
        .split([':', '/'])
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(target);
    host.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_default_targets_to_the_same_canonical_key() {
        let localhost = RemoteTargetSnapshot::parse("localhost").unwrap();
        let local_default = RemoteTargetSnapshot::parse("local:default").unwrap();

        assert_eq!(localhost, RemoteTargetSnapshot::Local { session: None });
        assert_eq!(local_default, RemoteTargetSnapshot::Local { session: None });
        assert_eq!(localhost.canonical_key(), "local:default");
        assert_eq!(local_default.canonical_key(), "local:default");
    }

    #[test]
    fn parses_named_local_session_targets() {
        let target = RemoteTargetSnapshot::parse("local:dev").unwrap();

        assert_eq!(
            target,
            RemoteTargetSnapshot::Local {
                session: Some("dev".into())
            }
        );
        assert_eq!(target.canonical_key(), "local:dev");
    }

    #[test]
    fn derives_default_display_name_from_ssh_url_host() {
        let mut registry = RemoteRegistrySnapshot::default();

        let remote = registry
            .add(
                None,
                "ssh://you@example.test:2222".into(),
                RemoteKeybindingsSnapshot::Local,
            )
            .unwrap();

        assert_eq!(remote.name, "example.test");
    }

    #[test]
    fn rejects_duplicate_local_default_targets() {
        let mut registry = RemoteRegistrySnapshot::default();

        registry
            .add(
                Some("local".into()),
                "localhost".into(),
                RemoteKeybindingsSnapshot::Local,
            )
            .unwrap();
        let duplicate = registry
            .add(
                Some("default".into()),
                "local:default".into(),
                RemoteKeybindingsSnapshot::Local,
            )
            .unwrap_err();

        assert_eq!(duplicate, RemoteRegistryError::DuplicateTarget);
    }

    #[test]
    fn rejects_targets_excluded_by_the_caller() {
        let mut registry = RemoteRegistrySnapshot::default();
        let excluded = vec![RemoteTargetSnapshot::Local { session: None }];

        let duplicate = registry
            .add_excluding_targets(
                Some("local".into()),
                "localhost".into(),
                RemoteKeybindingsSnapshot::Local,
                &excluded,
            )
            .unwrap_err();

        assert_eq!(duplicate, RemoteRegistryError::DuplicateTarget);
        assert!(registry.remotes.is_empty());
    }
}
