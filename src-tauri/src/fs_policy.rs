//! App-owned filesystem scope for custom Rust commands.
//!
//! `tauri_plugin_fs::FsExt::fs_scope()` is the plugin's runtime scope. It
//! starts empty and is extended by events such as drag/drop; it is not the
//! same thing as the `fs:scope` ACL block in `capabilities/default.json`.
//! Custom app commands do not receive the fs plugin's command scope, so we
//! build an app-owned `tauri::fs::Scope` from that JSON and share it across
//! the read/write Rust commands.

use std::path::PathBuf;

use serde_json::Value;
use tauri::utils::config::FsScope;
use tauri::{Manager, Runtime};

const DEFAULT_CAPABILITY_JSON: &str = include_str!("../capabilities/default.json");
const FS_SCOPE_IDENTIFIER: &str = "fs:scope";

pub struct AppFsScope {
    pub scope: tauri::fs::Scope,
}

pub fn init_app_fs_scope<R, M>(manager: &M) -> Result<AppFsScope, String>
where
    R: Runtime,
    M: Manager<R>,
{
    let fs_scope = default_fs_scope()?;
    let scope = tauri::fs::Scope::new(manager, &fs_scope)
        .map_err(|e| format!("failed to initialize app filesystem scope: {e}"))?;
    Ok(AppFsScope { scope })
}

pub fn app_fs_scope<R, M>(manager: &M) -> Result<tauri::fs::Scope, String>
where
    R: Runtime,
    M: Manager<R>,
{
    manager
        .try_state::<AppFsScope>()
        .map(|state| state.scope.clone())
        .ok_or_else(|| "app filesystem scope was not initialized".to_string())
}

fn default_fs_scope() -> Result<FsScope, String> {
    fs_scope_from_capability_json(DEFAULT_CAPABILITY_JSON)
}

fn fs_scope_from_capability_json(raw: &str) -> Result<FsScope, String> {
    let root: Value = serde_json::from_str(raw)
        .map_err(|e| format!("capabilities/default.json is invalid JSON: {e}"))?;
    let permissions = root
        .get("permissions")
        .and_then(Value::as_array)
        .ok_or_else(|| "capabilities/default.json is missing permissions[]".to_string())?;
    let scopes: Vec<&Value> = permissions
        .iter()
        .filter(|permission| {
            permission.get("identifier").and_then(Value::as_str) == Some(FS_SCOPE_IDENTIFIER)
        })
        .collect();
    let scope = match scopes.as_slice() {
        [scope] => *scope,
        [] => return Err("capabilities/default.json is missing fs:scope permission".to_string()),
        _ => {
            return Err(
                "capabilities/default.json must contain exactly one fs:scope permission"
                    .to_string(),
            )
        }
    };

    Ok(FsScope::Scope {
        allow: parse_scope_paths(scope, "allow")?,
        deny: parse_scope_paths(scope, "deny")?,
        require_literal_leading_dot: parse_require_literal_leading_dot(scope)?,
    })
}

fn parse_scope_paths(scope: &Value, key: &str) -> Result<Vec<PathBuf>, String> {
    let entries = scope
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("fs:scope is missing {key}[]"))?;

    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            parse_scope_path(entry)
                .ok_or_else(|| format!("fs:scope {key}[{index}] is missing a path string"))
        })
        .collect()
}

fn parse_scope_path(entry: &Value) -> Option<PathBuf> {
    entry
        .as_str()
        .or_else(|| entry.get("path").and_then(Value::as_str))
        .map(PathBuf::from)
}

fn parse_require_literal_leading_dot(scope: &Value) -> Result<Option<bool>, String> {
    let camel = parse_optional_bool(scope, "requireLiteralLeadingDot")?;
    let kebab = parse_optional_bool(scope, "require-literal-leading-dot")?;
    match (camel, kebab) {
        (Some(a), Some(b)) if a != b => Err(
            "fs:scope has conflicting requireLiteralLeadingDot / require-literal-leading-dot values"
                .to_string(),
        ),
        (Some(value), _) | (_, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

fn parse_optional_bool(scope: &Value, key: &str) -> Result<Option<bool>, String> {
    match scope.get(key) {
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("fs:scope {key} must be a boolean")),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_scope_from_capability_json() {
        let FsScope::Scope { allow, deny, .. } = default_fs_scope().unwrap() else {
            panic!("default capability should define a full fs scope");
        };

        assert_eq!(allow, vec![PathBuf::from("**")]);
        assert!(deny.contains(&PathBuf::from("$HOME/.ssh/**")));
        assert!(deny.contains(&PathBuf::from("$APPDATA/**")));
    }

    #[test]
    fn parses_shorthand_paths_and_literal_dot_flag() {
        let raw = r#"
        {
          "permissions": [
            {
              "identifier": "fs:scope",
              "allow": ["**"],
              "deny": ["$HOME/.ssh/**"],
              "requireLiteralLeadingDot": true
            }
          ]
        }
        "#;

        let FsScope::Scope {
            allow,
            deny,
            require_literal_leading_dot,
        } = fs_scope_from_capability_json(raw).unwrap()
        else {
            panic!("capability should parse as a full fs scope");
        };

        assert_eq!(allow, vec![PathBuf::from("**")]);
        assert_eq!(deny, vec![PathBuf::from("$HOME/.ssh/**")]);
        assert_eq!(require_literal_leading_dot, Some(true));
    }

    #[test]
    fn rejects_duplicate_fs_scope_permissions() {
        let raw = r#"
        {
          "permissions": [
            { "identifier": "fs:scope", "allow": [], "deny": [] },
            { "identifier": "fs:scope", "allow": [], "deny": [] }
          ]
        }
        "#;

        let err = fs_scope_from_capability_json(raw).unwrap_err();
        assert!(err.contains("exactly one fs:scope"));
    }

    #[test]
    fn rejects_conflicting_literal_dot_aliases() {
        let raw = r#"
        {
          "permissions": [
            {
              "identifier": "fs:scope",
              "allow": [],
              "deny": [],
              "requireLiteralLeadingDot": true,
              "require-literal-leading-dot": false
            }
          ]
        }
        "#;

        let err = fs_scope_from_capability_json(raw).unwrap_err();
        assert!(err.contains("conflicting requireLiteralLeadingDot"));
    }

    #[cfg(windows)]
    #[test]
    fn allow_all_pattern_matches_normal_drive_root_outputs() {
        let pattern = tauri::fs::Pattern::new("**").unwrap();
        let match_options = glob::MatchOptions {
            require_literal_separator: true,
            require_literal_leading_dot: false,
            ..Default::default()
        };

        for raw in [
            r"E:\episode.embedded.ass",
            r"E:\_embed\episode.embedded.ass",
        ] {
            let path: PathBuf = std::path::Path::new(raw).components().collect();
            assert!(
                pattern.matches_path_with(&path, match_options),
                "`**` should allow normal drive-root output path {raw}"
            );
        }
    }
}
