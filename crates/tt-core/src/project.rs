//! Git project identity extraction.

use std::path::Path;

/// Project identity from git/jj context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectIdentity {
    pub project_name: String,
    pub workspace_name: Option<String>,
}

/// Extract repo name from a git remote URL.
///
/// Handles both HTTPS and SSH URL formats:
/// - `https://github.com/user/repo.git` -> "repo"
/// - `git@github.com:user/repo.git` -> "repo"
/// - `git@github.com:repo.git` -> "repo"
pub fn parse_remote_name(url: &str) -> Option<String> {
    // First try splitting by '/' for HTTPS URLs or SSH URLs with user path
    let after_slash = url.rsplit('/').next()?;

    // If the result still contains ':', it's an SSH URL without a '/' after the host
    // e.g., "git@github.com:repo.git" -> after_slash is "git@github.com:repo.git"
    let name = if after_slash.contains(':') {
        after_slash.rsplit(':').next()?
    } else {
        after_slash
    };

    let name = name.trim_end_matches(".git");

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
        // HTTPS format
        assert_eq!(
            parse_remote_name("https://github.com/user/time-tracker.git"),
            Some("time-tracker".to_string())
        );
        // SSH format with user path
        assert_eq!(
            parse_remote_name("git@github.com:user/dotfiles.git"),
            Some("dotfiles".to_string())
        );
        // SSH format without user path (edge case)
        assert_eq!(
            parse_remote_name("git@github.com:myrepo.git"),
            Some("myrepo".to_string())
        );
        // Without .git suffix
        assert_eq!(
            parse_remote_name("https://github.com/user/project"),
            Some("project".to_string())
        );
        // Empty and edge cases
        assert_eq!(parse_remote_name(""), None);
        assert_eq!(parse_remote_name(".git"), None);
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
