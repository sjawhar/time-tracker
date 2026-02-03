//! jj/git project identity extraction.

use std::path::Path;

/// Project identity from jj context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectIdentity {
    pub project_name: String,
    pub workspace_name: Option<String>,
}

/// Extract repo name from a git remote URL.
pub fn parse_remote_name(url: &str) -> Option<String> {
    let name = url
        .rsplit('/')
        .next()
        .or_else(|| url.rsplit(':').next())?
        .trim_end_matches(".git");

    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

impl ProjectIdentity {
    /// Build identity from jj command outputs.
    pub fn from_jj_output(remote_url: Option<&str>, workspace_count: usize, jj_root: &str) -> Self {
        let workspace_name = Path::new(jj_root)
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from);

        let remote_name = remote_url.and_then(parse_remote_name);

        let project_name = if workspace_count > 1 {
            remote_name.or_else(|| {
                Path::new(jj_root)
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(String::from)
            })
        } else {
            remote_name.or_else(|| workspace_name.clone())
        }
        .unwrap_or_else(|| "unknown".to_string());

        Self {
            project_name,
            workspace_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_git_remote_url() {
        assert_eq!(
            parse_remote_name("https://github.com/user/time-tracker.git"),
            Some("time-tracker".to_string())
        );
        assert_eq!(
            parse_remote_name("git@github.com:user/dotfiles.git"),
            Some("dotfiles".to_string())
        );
    }

    #[test]
    fn test_project_identity_multi_workspace() {
        let identity = ProjectIdentity::from_jj_output(
            Some("https://github.com/user/time-tracker.git"),
            2,
            "/home/sami/time-tracker/default",
        );

        assert_eq!(identity.project_name, "time-tracker");
        assert_eq!(identity.workspace_name.as_deref(), Some("default"));
    }
}
