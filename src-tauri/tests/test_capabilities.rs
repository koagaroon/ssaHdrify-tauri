//! Pin the fs:scope deny-list shape so a typo (missing trailing
//! `/**`, wrong scope variable, accidental removal of a key entry)
//! surfaces as a test failure rather than silently widening the
//! attack surface.
//!
//! The deny list is data, not runtime behavior, so this test reads
//! `capabilities/default.json` and asserts:
//!
//!   1. Every required entry is present (alarms on accidental delete
//!      / forgotten scope-variable repair).
//!   2. No forbidden entry is present (alarms on bundle-namespaced
//!      traps: `$APPDATA/Microsoft/Credentials/**` would resolve to
//!      a non-existent bundle-namespaced path; the cross-tool deny
//!      lives under `$DATA/Microsoft/Credentials/**` instead).
//!   3. Each entry's path string parses as expected (scope variable
//!      prefix, no unbalanced braces / glob shapes that wouldn't
//!      match anything).
//!
//! This is a static-shape test — it doesn't invoke `fs_scope().is_allowed`
//! because that requires a live Tauri app handle. Tauri's matcher
//! interprets the strings here; if the strings are wrong, the matcher
//! silently mis-applies. Pinning the strings catches the typo class
//! the original incident actually had.

use std::path::PathBuf;

fn capabilities_path() -> PathBuf {
    // CARGO_MANIFEST_DIR points at src-tauri/ when this test runs via
    // `cargo test`, so the capabilities directory is one level down.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("capabilities/default.json")
}

fn read_deny_paths() -> Vec<String> {
    let raw = std::fs::read_to_string(capabilities_path())
        .expect("capabilities/default.json should be readable");
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).expect("capabilities/default.json should parse as JSON");
    let permissions = parsed["permissions"]
        .as_array()
        .expect("permissions should be an array");
    let fs_scope = permissions
        .iter()
        .find(|p| p.is_object() && p["identifier"].as_str() == Some("fs:scope"))
        .expect("fs:scope permission block should be present");
    fs_scope["deny"]
        .as_array()
        .expect("fs:scope.deny should be an array")
        .iter()
        .map(|entry| {
            entry["path"]
                .as_str()
                .expect("each deny entry should have a path string")
                .to_string()
        })
        .collect()
}

fn read_permission_strings() -> Vec<String> {
    let raw = std::fs::read_to_string(capabilities_path())
        .expect("capabilities/default.json should be readable");
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).expect("capabilities/default.json should parse as JSON");
    parsed["permissions"]
        .as_array()
        .expect("permissions should be an array")
        .iter()
        .filter_map(|permission| permission.as_str().map(ToString::to_string))
        .collect()
}

#[test]
fn plugin_fs_grants_no_direct_file_command_surface() {
    let permissions = read_permission_strings();

    assert!(
        !permissions
            .iter()
            .any(|permission| permission == "fs:default"),
        "fs:default grants broad read APIs; frontend file checks should route through Rust"
    );
    for forbidden in [
        "fs:allow-exists",
        "fs:allow-read-file",
        "fs:allow-read-dir",
        "fs:allow-stat",
        "fs:allow-lstat",
        "fs:allow-write-file",
        "fs:allow-write-text-file",
        "fs:allow-copy-file",
        "fs:allow-rename",
    ] {
        assert!(
            !permissions.iter().any(|permission| permission == forbidden),
            "unexpected broad plugin-fs permission remained: {forbidden}"
        );
    }
}

#[test]
fn deny_list_contains_required_categories() {
    let deny = read_deny_paths();

    // Per design doc § fs:scope policy three categories.
    // Each entry below documents which category + which OS family it
    // protects so a future audit can re-verify by reading the list
    // in one place.
    let required = [
        // Category 1 — cross-tool credentials / tokens (POSIX $HOME)
        ("$HOME/.ssh", "SSH keys + known_hosts"),
        ("$HOME/.ssh/**", "SSH keys recursive"),
        ("$HOME/.aws/**", "AWS credentials"),
        ("$HOME/.gnupg/**", "GPG keyring"),
        ("$HOME/.azure/**", "Azure CLI state"),
        ("$HOME/.gcloud/**", "gcloud credentials"),
        ("$HOME/.docker/**", "Docker auth"),
        ("$HOME/.kube/**", "kubeconfig"),
        (
            "$HOME/.cargo/**",
            "Cargo creds + registry + git (single rule)",
        ),
        ("$HOME/.config/gh/**", "gh CLI auth"),
        ("$HOME/.config/git/credentials", "git http credentials"),
        ("$HOME/.config/op/**", "1Password CLI"),
        ("$HOME/.git-credentials", "legacy git credential helper"),
        ("$HOME/.npmrc", "npm auth tokens"),
        ("$HOME/.netrc", "netrc credentials"),
        ("$HOME/.pypirc", "PyPI tokens"),
        // Category 1 — cross-tool credentials (Windows $DATA / $LOCALDATA)
        (
            "$DATA/Microsoft/Credentials/**",
            "Windows Credential Manager",
        ),
        ("$DATA/Microsoft/Crypto/**", "Windows DPAPI keys"),
        ("$DATA/Microsoft/Protect/**", "DPAPI protect dir"),
        ("$DATA/Microsoft/Vault/**", "Windows Vault"),
        (
            "$LOCALDATA/Microsoft/Credentials/**",
            "local Credential Manager",
        ),
        ("$LOCALDATA/Microsoft/Vault/**", "local Vault"),
        ("$DATA/Mozilla/**", "Firefox / Thunderbird profile"),
        ("$DATA/Thunderbird/**", "Thunderbird mail store"),
        ("$DATA/Signal/**", "Signal Desktop"),
        ("$DATA/Telegram Desktop/**", "Telegram Desktop"),
        ("$DATA/PuTTY/**", "PuTTY sessions / saved keys"),
        ("$DATA/Code/User/**", "VS Code user state"),
        ("$DATA/JetBrains/**", "JetBrains user state"),
        ("$LOCALDATA/JetBrains/**", "JetBrains user state (local)"),
        ("$DATA/npm/**", "npm cache / state"),
        ("$DATA/yarn/**", "yarn cache / state"),
        ("$DATA/Docker/**", "Docker Desktop roaming"),
        ("$LOCALDATA/Docker/**", "Docker Desktop local"),
        ("$LOCALDATA/Lens/**", "Lens (k8s desktop)"),
        // Category 1 — recent-files leakage (jump lists + Office recents)
        (
            "$DATA/Microsoft/Windows/Recent/**",
            "Windows Recent / jump list",
        ),
        ("$DATA/Microsoft/Office/Recent/**", "Office recent files"),
        (
            "$DATA/Microsoft/Windows/Start Menu/**",
            "Start Menu autostart class",
        ),
        ("$DATA/Microsoft/Windows/SendTo/**", "SendTo shortcuts"),
        (
            "$LOCALDATA/Microsoft/Windows/Start Menu/**",
            "local Start Menu",
        ),
        (
            "$LOCALDATA/Microsoft/WindowsApps/**",
            "WindowsApps store shims",
        ),
        ("$LOCALDATA/Packages/**", "UWP package state"),
        // Category 2 — browser profiles
        ("$LOCALDATA/Google/Chrome/User Data/**", "Chrome profiles"),
        ("$LOCALDATA/Chromium/User Data/**", "Chromium profiles"),
        ("$LOCALDATA/Microsoft/Edge/User Data/**", "Edge profiles"),
        ("$LOCALDATA/BraveSoftware/**", "Brave profiles"),
        ("$LOCALDATA/Mozilla/**", "Firefox local profile"),
        // Category 3 — app's own state (legacy + unified)
        (
            "$APPDATA/**",
            "legacy bundle-namespaced state (Tauri internal)",
        ),
        ("$APPLOCALDATA/**", "legacy bundle-namespaced local state"),
        (
            "$DATA/ssahdrify/**",
            "unified app data dir (W11.4b destination)",
        ),
        // Shell history (POSIX + PowerShell + fish + vi + less)
        ("$HOME/.bash_history", "bash"),
        ("$HOME/.zsh_history", "zsh"),
        (
            "$LOCALDATA/Microsoft/Windows/PowerShell/PSReadLine/**",
            "PowerShell",
        ),
        ("$HOME/.local/share/fish/fish_history", "fish"),
        ("$HOME/.viminfo", "vim state"),
        ("$HOME/.lesshst", "less history"),
        // Linux keyrings
        ("$HOME/.local/share/keyrings/**", "GNOME keyring"),
        ("$HOME/.gnome2/keyrings/**", "legacy GNOME keyring"),
        ("$HOME/.kde/share/apps/kwallet/**", "KDE Wallet"),
        // Bare-directory denies that sit ALONGSIDE their `/**` recursive
        // forms above — pinned so neither half can be silently dropped.
        ("$HOME/.aws", "AWS credentials dir"),
        ("$HOME/.azure", "Azure CLI dir"),
        ("$HOME/.gcloud", "gcloud dir"),
        ("$HOME/.gnupg", "GPG keyring dir"),
        ("$HOME/.docker", "Docker auth dir"),
        ("$HOME/.kube", "kubeconfig dir"),
        // Editor state across the three non-Windows config conventions
        // (Linux XDG config, Linux data, macOS Application Support).
        (
            "$HOME/.config/Code/User/**",
            "VS Code user state (Linux XDG)",
        ),
        ("$HOME/.config/JetBrains/**", "JetBrains state (Linux XDG)"),
        (
            "$HOME/.local/share/JetBrains/**",
            "JetBrains state (Linux data)",
        ),
        (
            "$HOME/Library/Application Support/Code/User/**",
            "VS Code user state (macOS)",
        ),
        (
            "$HOME/Library/Application Support/JetBrains/**",
            "JetBrains state (macOS)",
        ),
    ];

    let missing: Vec<&(&str, &str)> = required
        .iter()
        .filter(|(path, _)| !deny.iter().any(|d| d == path))
        .collect();
    assert!(
        missing.is_empty(),
        "deny list is missing required entries (R17 W17.5 A-R17-49 pin):\n{}",
        missing
            .iter()
            .map(|(path, label)| format!("  - {path}  ({label})"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn deny_list_rejects_w12_1_w13_2_traps() {
    let deny = read_deny_paths();

    // Forbidden entries documented per past bundle-namespacing
    // incidents:
    // Tauri 2's $APPDATA / $APPLOCALDATA are bundle-namespaced (resolve
    // to `<roaming>/com.koagaroon.ssahdrify/`). Cross-tool deny rules
    // that name a SECOND segment (e.g. `$APPDATA/Microsoft/...`)
    // resolve to a non-existent doubly-namespaced path and DENY
    // NOTHING. The right form for cross-tool denies is `$DATA/...`.
    let forbidden = [
        (
            "$APPDATA/Microsoft/Credentials/**",
            "should be $DATA/... (W12.1)",
        ),
        ("$APPDATA/Mozilla/**", "should be $DATA/... (W12.1)"),
        ("$APPDATA/Signal/**", "should be $DATA/... (W12.1)"),
        (
            "$APPLOCALDATA/Microsoft/Credentials/**",
            "should be $LOCALDATA/... (W12.1)",
        ),
        (
            "$APPLOCALDATA/Google/Chrome/User Data/**",
            "should be $LOCALDATA/... (W12.1)",
        ),
        // per-file `.cargo/credentials*` /
        // `.cargo/config*` enumeration is superseded by the wildcard
        // `.cargo/**`. Adding the narrower rules back would suggest
        // a future contributor might enumerate by hand again and
        // miss `.cargo/registry/**` / `.cargo/git/**`.
        (
            "$HOME/.cargo/credentials",
            "covered by $HOME/.cargo/** (W17.5 A-R17-50)",
        ),
        (
            "$HOME/.cargo/credentials.toml",
            "covered by $HOME/.cargo/** (W17.5 A-R17-50)",
        ),
        (
            "$HOME/.cargo/config",
            "covered by $HOME/.cargo/** (W17.5 A-R17-50)",
        ),
        (
            "$HOME/.cargo/config.toml",
            "covered by $HOME/.cargo/** (W17.5 A-R17-50)",
        ),
    ];

    let present: Vec<&(&str, &str)> = forbidden
        .iter()
        .filter(|(path, _)| deny.iter().any(|d| d == path))
        .collect();
    assert!(
        present.is_empty(),
        "deny list contains entries flagged as wrong-shape (R17 W17.5):\n{}",
        present
            .iter()
            .map(|(path, reason)| format!("  - {path}  ({reason})"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn deny_list_entries_use_known_scope_variables() {
    let deny = read_deny_paths();

    // Every deny entry must start with one of these scope-variable
    // prefixes. Catches typos like `$DAT/...` (missing letter),
    // `${DATA}/...` (shell-style braces), or accidentally absolute
    // paths (`C:\Users\...`) that resolve to a literal path the
    // matcher won't expand.
    let allowed_prefixes = [
        "$HOME/",
        "$DATA/",
        "$LOCALDATA/",
        "$APPDATA/",
        "$APPLOCALDATA/",
    ];

    let bad: Vec<&String> = deny
        .iter()
        .filter(|p| !allowed_prefixes.iter().any(|pref| p.starts_with(pref)))
        .collect();
    assert!(
        bad.is_empty(),
        "deny list entries with unknown / mangled scope-variable prefix (R17 W17.5):\n{}",
        bad.iter()
            .map(|p| format!("  - {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn bundle_namespaced_denies_are_bare_recursive_only() {
    // $APPDATA / $APPLOCALDATA are bundle-namespaced (resolve to
    // `<roaming>/com.koagaroon.ssahdrify/`), so the ONLY meaningful form is
    // the bare `$APPDATA/**` — the app's own namespaced dir. A multi-segment
    // `$APPDATA/<anything>/**` resolves to a non-existent doubly-namespaced
    // path and DENIES NOTHING. The forbidden-traps test blocks only the
    // specific historical mistakes; this enforces the GENERAL rule so a
    // future `$APPDATA/Foo/**`-shaped trap fails too (pattern over
    // enumeration). `$APPLOCALDATA` does not start with `$APPDATA`, so the
    // two prefixes never cross-match.
    let deny = read_deny_paths();
    for p in &deny {
        for prefix in ["$APPDATA", "$APPLOCALDATA"] {
            if p.starts_with(prefix) {
                assert_eq!(
                    p,
                    &format!("{prefix}/**"),
                    "bundle-namespaced deny must be bare `{prefix}/**`; the multi-segment \
                     form `{p}` resolves to a non-existent doubly-namespaced path and \
                     denies nothing"
                );
            }
        }
    }
}
