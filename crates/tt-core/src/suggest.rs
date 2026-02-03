//! Tag suggestion logic based on event metadata.
//!
//! Provides path-based heuristics to suggest project tags from working directories.

use std::collections::HashMap;

/// A suggested tag with reasoning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// The suggested tag.
    pub tag: String,
    /// Human-readable explanation for why this tag was suggested.
    pub reason: String,
}

/// Suggest a tag based on working directory paths.
///
/// Analyzes the provided cwds to find the most likely project name.
/// Returns `None` if no meaningful project can be inferred.
pub fn suggest_from_metadata(cwds: &[&str]) -> Option<Suggestion> {
    if cwds.is_empty() {
        return None;
    }

    // Extract project names from each path
    let mut project_counts: HashMap<String, usize> = HashMap::new();
    for cwd in cwds {
        if let Some(project) = extract_project_from_path(cwd) {
            *project_counts.entry(project).or_insert(0) += 1;
        }
    }

    if project_counts.is_empty() {
        return None;
    }

    // Find the most common project
    let (best_project, best_count) = project_counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .unwrap();

    // Calculate percentage
    let percentage = (best_count * 100) / cwds.len();

    Some(Suggestion {
        tag: best_project.clone(),
        reason: format!(
            "Most common working directory ({percentage}% of events in {best_project})"
        ),
    })
}

/// Check if metadata is ambiguous and would benefit from LLM analysis.
///
/// Returns `true` if:
/// - All paths are generic (home dirs, tmp, etc.)
/// - No dominant directory (no project with >50% of events)
pub fn is_metadata_ambiguous(cwds: &[&str]) -> bool {
    if cwds.is_empty() {
        return true;
    }

    // Check if any path yields a project
    let mut project_counts: HashMap<String, usize> = HashMap::new();
    for cwd in cwds {
        if let Some(project) = extract_project_from_path(cwd) {
            *project_counts.entry(project).or_insert(0) += 1;
        }
    }

    // If no projects were extracted, metadata is ambiguous
    if project_counts.is_empty() {
        return true;
    }

    // If no project has >50% of events, metadata is ambiguous
    let total = cwds.len();
    let max_count = project_counts.values().max().unwrap_or(&0);
    let max_percentage = (max_count * 100) / total;

    max_percentage <= 50
}

/// Extract a project name from a working directory path.
///
/// Returns `None` for generic paths that don't indicate a project.
fn extract_project_from_path(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }

    // Split path into components
    let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if components.is_empty() {
        return None;
    }

    // Skip paths that are just user home directories
    // e.g., /home/user or /Users/user or ~
    if is_home_directory(&components) {
        return None;
    }

    // Skip generic system paths
    if is_generic_path(&components) {
        return None;
    }

    // Look for meaningful project directories
    // Prefer paths under known project containers: projects/, repos/, src/, code/, work/
    if let Some(project) = find_project_under_container(&components) {
        return Some(project);
    }

    // Otherwise, use the deepest meaningful component
    // Skip the home directory prefix and use the first meaningful subdirectory
    if let Some(project) = find_first_meaningful_component(&components) {
        return Some(project);
    }

    None
}

/// Check if path components represent a user home directory.
fn is_home_directory(components: &[&str]) -> bool {
    matches!(
        components,
        // /home/username, /Users/username (macOS), or /root
        ["home" | "Users", _] | ["root"]
    )
}

/// Check if path is a generic system path not indicating a project.
fn is_generic_path(components: &[&str]) -> bool {
    if components.is_empty() {
        return true;
    }

    let first = components[0];

    // Generic system paths
    matches!(
        first,
        "tmp" | "var" | "etc" | "usr" | "bin" | "lib" | "opt" | "proc" | "sys"
    )
}

/// Find project name under known container directories.
fn find_project_under_container(components: &[&str]) -> Option<String> {
    // Known container directory names
    const CONTAINERS: &[&str] = &[
        "projects",
        "repos",
        "src",
        "code",
        "work",
        "dev",
        "workspace",
        "workspaces",
        "git",
    ];

    for (i, component) in components.iter().enumerate() {
        let lower = component.to_lowercase();
        if CONTAINERS.contains(&lower.as_str()) {
            // Return the next component after the container
            if let Some(project) = components.get(i + 1) {
                // Don't return if the project name is itself generic
                if !is_generic_component(project) {
                    return Some((*project).to_string());
                }
            }
        }
    }

    None
}

/// Find the first meaningful component after home directory.
fn find_first_meaningful_component(components: &[&str]) -> Option<String> {
    // Skip home directory prefix
    let skip = if components.len() >= 2 {
        match (components.first(), components.get(1)) {
            (Some(&"home" | &"Users"), Some(_)) => 2,
            (Some(&"root"), _) => 1,
            _ => 0,
        }
    } else {
        0
    };

    // Find first non-generic component
    for component in components.iter().skip(skip) {
        if !is_generic_component(component) {
            return Some((*component).to_string());
        }
    }

    None
}

/// Check if a component name is generic and not a meaningful project name.
fn is_generic_component(component: &str) -> bool {
    let lower = component.to_lowercase();

    // Generic directory names
    matches!(
        lower.as_str(),
        "projects"
            | "repos"
            | "src"
            | "code"
            | "work"
            | "dev"
            | "workspace"
            | "workspaces"
            | "git"
            | "tmp"
            | "temp"
            | "scratch"
            | "downloads"
            | "documents"
            | "desktop"
            | "home"
            | "user"
            | "root"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== extract_project_from_path tests ==========

    #[test]
    fn test_extract_project_from_projects_dir() {
        assert_eq!(
            extract_project_from_path("/home/user/projects/acme-webapp"),
            Some("acme-webapp".to_string())
        );
    }

    #[test]
    fn test_extract_project_from_nested_projects_dir() {
        assert_eq!(
            extract_project_from_path("/home/user/projects/acme-webapp/src"),
            Some("acme-webapp".to_string())
        );
    }

    #[test]
    fn test_extract_project_from_repos_dir() {
        assert_eq!(
            extract_project_from_path("/home/sami/repos/time-tracker/default"),
            Some("time-tracker".to_string())
        );
    }

    #[test]
    fn test_extract_project_from_home_subdir() {
        // Without a known container, use first meaningful subdir
        assert_eq!(
            extract_project_from_path("/home/user/my-project"),
            Some("my-project".to_string())
        );
    }

    #[test]
    fn test_extract_project_from_macos_path() {
        assert_eq!(
            extract_project_from_path("/Users/john/code/awesome-app"),
            Some("awesome-app".to_string())
        );
    }

    #[test]
    fn test_extract_none_from_home_only() {
        assert_eq!(extract_project_from_path("/home/user"), None);
    }

    #[test]
    fn test_extract_none_from_tmp() {
        assert_eq!(extract_project_from_path("/tmp/scratch"), None);
    }

    #[test]
    fn test_extract_none_from_var() {
        assert_eq!(extract_project_from_path("/var/log"), None);
    }

    #[test]
    fn test_extract_none_from_empty() {
        assert_eq!(extract_project_from_path(""), None);
    }

    #[test]
    fn test_extract_none_from_root() {
        assert_eq!(extract_project_from_path("/"), None);
    }

    // ========== suggest_from_metadata tests ==========

    #[test]
    fn test_suggest_dominant_project() {
        let cwds = vec![
            "/home/user/projects/acme-webapp/src",
            "/home/user/projects/acme-webapp/tests",
            "/home/user/projects/acme-webapp",
            "/home/user/projects/other-project",
        ];

        let suggestion = suggest_from_metadata(&cwds).unwrap();
        assert_eq!(suggestion.tag, "acme-webapp");
        assert!(suggestion.reason.contains("75%")); // 3 out of 4
    }

    #[test]
    fn test_suggest_none_for_empty() {
        let cwds: Vec<&str> = vec![];
        assert_eq!(suggest_from_metadata(&cwds), None);
    }

    #[test]
    fn test_suggest_none_for_generic_paths() {
        let cwds = vec!["/home/user", "/tmp", "/var/log"];
        assert_eq!(suggest_from_metadata(&cwds), None);
    }

    // ========== is_metadata_ambiguous tests ==========

    #[test]
    fn test_ambiguous_when_empty() {
        let cwds: Vec<&str> = vec![];
        assert!(is_metadata_ambiguous(&cwds));
    }

    #[test]
    fn test_ambiguous_when_all_generic() {
        let cwds = vec!["/home/user", "/tmp", "/var/log"];
        assert!(is_metadata_ambiguous(&cwds));
    }

    #[test]
    fn test_ambiguous_when_no_dominant() {
        let cwds = vec![
            "/home/user/projects/project-a",
            "/home/user/projects/project-b",
            "/home/user/projects/project-c",
            "/home/user/projects/project-d",
        ];
        // Each project has 25%, none has >50%
        assert!(is_metadata_ambiguous(&cwds));
    }

    #[test]
    fn test_not_ambiguous_when_dominant() {
        let cwds = vec![
            "/home/user/projects/main-project",
            "/home/user/projects/main-project/src",
            "/home/user/projects/main-project/tests",
            "/home/user/projects/other",
        ];
        // main-project has 75%
        assert!(!is_metadata_ambiguous(&cwds));
    }

    #[test]
    fn test_ambiguous_at_50_percent() {
        let cwds = vec![
            "/home/user/projects/project-a",
            "/home/user/projects/project-a",
            "/home/user/projects/project-b",
            "/home/user/projects/project-b",
        ];
        // Each has exactly 50%, so it's ambiguous (need >50%)
        assert!(is_metadata_ambiguous(&cwds));
    }

    #[test]
    fn test_not_ambiguous_just_over_50() {
        let cwds = vec![
            "/home/user/projects/project-a",
            "/home/user/projects/project-a",
            "/home/user/projects/project-a",
            "/home/user/projects/project-b",
            "/home/user/projects/project-c",
        ];
        // project-a has 60%
        assert!(!is_metadata_ambiguous(&cwds));
    }
}
