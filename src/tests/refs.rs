use super::scratch_dir;
use crate::config::SyncState;
use crate::refs::{
    apply_ref_names, infer_name_from_url, load_refs_from, normalize_url, parse_managed_refs,
    read_project_refs, remove_managed_block, remove_ref, remove_refs, sanitize_description,
    save_refs_to, sync_project_refs, upsert_managed_block, upsert_ref, validate_ref_name,
    ReferenceEntry, ReferenceVault,
};
use std::collections::BTreeSet;
use std::fs;

fn entry(name: &str, url: &str, description: &str) -> ReferenceEntry {
    ReferenceEntry {
        name: name.to_string(),
        url: url.to_string(),
        description: description.to_string(),
    }
}

#[test]
fn validate_normalize_and_infer_url_names() {
    assert_eq!(validate_ref_name("react_18").unwrap(), "react_18");
    assert_eq!(validate_ref_name("next.js").unwrap(), "next.js");
    assert!(validate_ref_name("").is_err());
    assert!(validate_ref_name("-bad").is_err());
    assert!(validate_ref_name("bad name").is_err());
    assert!(validate_ref_name(&"a".repeat(65)).is_err());

    assert_eq!(
        normalize_url("github.com/facebook/react").unwrap(),
        "https://github.com/facebook/react"
    );
    assert_eq!(
        normalize_url("https://github.com/ratatui/ratatui.git").unwrap(),
        "https://github.com/ratatui/ratatui.git"
    );
    assert!(normalize_url("").is_err());
    assert!(normalize_url("ftp://example.com/repo").is_err());
    assert!(normalize_url("https://github.com/has space").is_err());
    assert!(normalize_url("https://localhostonly").is_err());
    assert!(normalize_url("http://localhost:8080/repo").is_ok());

    assert_eq!(
        infer_name_from_url("https://github.com/facebook/react"),
        "react"
    );
    assert_eq!(
        infer_name_from_url("https://github.com/facebook/react.git"),
        "react"
    );
    assert_eq!(
        infer_name_from_url("https://github.com/vercel/next.js/"),
        "next.js"
    );
    assert_eq!(
        infer_name_from_url("https://github.com/org/repo/tree/main?tab=readme#top"),
        "main"
    );
    assert_eq!(infer_name_from_url("https://example.com"), "ref");
    assert_eq!(sanitize_description("line1\r\nline2  "), "line1  line2");
}

#[test]
fn upsert_and_save_refs_toml_round_trip() {
    let dir = scratch_dir("refs-toml");
    let path = dir.join("references.toml");
    let mut vault = ReferenceVault::default();

    assert!(upsert_ref(
        &mut vault,
        entry("", "github.com/facebook/react", "UI components\nreference")
    )
    .unwrap());
    assert_eq!(vault.references[0].name, "react");
    assert_eq!(vault.references[0].url, "https://github.com/facebook/react");
    assert_eq!(vault.references[0].description, "UI components reference");

    assert!(!upsert_ref(
        &mut vault,
        entry("REACT", "https://github.com/facebook/react", "Updated desc")
    )
    .unwrap());
    assert_eq!(vault.references.len(), 1);
    assert_eq!(vault.references[0].description, "Updated desc");

    save_refs_to(&vault, &path).unwrap();
    let loaded = load_refs_from(&path).unwrap();
    assert_eq!(loaded, vault);

    assert!(remove_ref(&mut vault, "REACT"));
    assert!(!remove_ref(&mut vault, "missing"));
    assert!(vault.references.is_empty());
}

#[test]
fn managed_block_preserves_existing_and_removes_cleanly() {
    let react = entry("react", "https://github.com/facebook/react", "UI library");
    let tokio = entry(
        "tokio",
        "https://github.com/tokio-rs/tokio",
        "Async runtime",
    );

    // Append to existing content and preserve surrounding user text on update.
    let existing = "# Team Guide\n\nKeep tests deterministic.\n";
    let appended = upsert_managed_block(existing, &[&react]);
    assert!(appended.starts_with(
        "# Team Guide\n\nKeep tests deterministic.\n\n<!-- agentsquad:references:start -->"
    ));
    let parsed = parse_managed_refs(&appended);
    assert_eq!(
        parsed.get("react"),
        Some(&(
            "https://github.com/facebook/react".to_string(),
            "UI library".to_string()
        ))
    );

    let with_footer = format!("{appended}\n## Local Footer\nStay green.\n");
    let updated = upsert_managed_block(&with_footer, &[&react, &tokio]);
    assert!(updated.contains("# Team Guide"));
    assert!(updated.contains("Keep tests deterministic."));
    assert!(updated.contains("## Local Footer\nStay green."));
    assert_eq!(parse_managed_refs(&updated).len(), 2);

    // Removing managed section leaves both top and bottom user content intact.
    let cleaned = remove_managed_block(&updated).expect("user content remains");
    assert!(!cleaned.contains("agentsquad:references"));
    assert!(!cleaned.contains("https://github.com/facebook/react"));
    assert!(cleaned.contains("# Team Guide\n\nKeep tests deterministic."));
    assert!(cleaned.contains("## Local Footer\nStay green."));

    // Removing from a file that only contained the managed block returns None.
    let only_managed = upsert_managed_block("", &[&react]);
    assert_eq!(remove_managed_block(&only_managed), None);

    // CRLF endings are preserved cleanly.
    let crlf_existing = "# Notes\r\n\r\nLine two.\r\n";
    let crlf_added = upsert_managed_block(crlf_existing, &[&react]);
    assert!(crlf_added.contains("\r\n<!-- agentsquad:references:start -->\r\n"));
    let crlf_cleaned = remove_managed_block(&crlf_added).unwrap();
    assert_eq!(crlf_cleaned, "# Notes\r\n\r\nLine two.\r\n");
}

#[test]
fn apply_sync_diff_and_unapply_across_project_instruction_files() {
    let dir = scratch_dir("refs-project-sync");
    let proj_main = dir.join("proj-main");
    let proj_gemini = dir.join("proj-gemini");
    fs::create_dir_all(&proj_main).unwrap();
    fs::create_dir_all(&proj_gemini).unwrap();

    // Existing CLAUDE.md must never lose its content; AGENTS.md does not exist initially.
    fs::write(
        proj_main.join("CLAUDE.md"),
        "# Agent Instructions\n\nAlways run cargo test.\n",
    )
    .unwrap();
    fs::write(proj_gemini.join("GEMINI.md"), "# Gemini Rules\n").unwrap();

    let mut vault = ReferenceVault {
        references: vec![
            entry(
                "ratatui",
                "https://github.com/ratatui/ratatui",
                "TUI widgets and layout patterns",
            ),
            entry("crossterm", "https://github.com/crossterm-rs/crossterm", ""),
        ],
    };

    let (added, updated) =
        apply_ref_names(&proj_main, &vault, &["ratatui".into(), "crossterm".into()]).unwrap();
    assert_eq!((added, updated), (2, 0));

    let claude_text = fs::read_to_string(proj_main.join("CLAUDE.md")).unwrap();
    let agents_text = fs::read_to_string(proj_main.join("AGENTS.md")).unwrap();
    assert!(claude_text.starts_with("# Agent Instructions\n\nAlways run cargo test."));
    assert!(claude_text.contains(
        "**ratatui** (https://github.com/ratatui/ratatui): TUI widgets and layout patterns"
    ));
    assert!(
        claude_text.contains("- **crossterm** (https://github.com/crossterm-rs/crossterm)\n")
            || claude_text
                .contains("- **crossterm** (https://github.com/crossterm-rs/crossterm)\r\n")
    );
    assert!(agents_text.contains("**ratatui**"));
    assert!(!proj_main.join("GEMINI.md").exists());

    // GEMINI.md is updated only when already present in the project.
    apply_ref_names(&proj_gemini, &vault, &["ratatui".into()]).unwrap();
    let gemini_text = fs::read_to_string(proj_gemini.join("GEMINI.md")).unwrap();
    assert!(gemini_text.starts_with("# Gemini Rules"));
    assert!(gemini_text.contains("**ratatui**"));

    let state = read_project_refs(&proj_main, &vault);
    assert_eq!(
        state.vault_state.get("ratatui"),
        Some(&SyncState::Installed)
    );
    assert_eq!(
        state.vault_state.get("crossterm"),
        Some(&SyncState::Installed)
    );
    assert!(state.local_only.is_empty());

    // Editing a shared reference marks the project as Differs, and sync repairs it.
    vault.references[0].description = "Updated TUI patterns".to_string();
    let diff_state = read_project_refs(&proj_main, &vault);
    assert_eq!(
        diff_state.vault_state.get("ratatui"),
        Some(&SyncState::Differs)
    );
    let mut both = BTreeSet::new();
    both.insert("ratatui".to_string());
    both.insert("crossterm".to_string());
    assert_eq!(
        sync_project_refs(&proj_main, &vault, &both, &diff_state).unwrap(),
        (0, 1, 0)
    );
    assert!(fs::read_to_string(proj_main.join("CLAUDE.md"))
        .unwrap()
        .contains("Updated TUI patterns"));

    // Unapply one via remove_refs, keeping the other and original content intact.
    assert_eq!(
        remove_refs(&proj_main, &vault, &["ratatui".into()]).unwrap(),
        1
    );
    let after_partial = fs::read_to_string(proj_main.join("CLAUDE.md")).unwrap();
    assert!(!after_partial.contains("ratatui"));
    assert!(after_partial.contains("crossterm"));
    assert!(after_partial.contains("Always run cargo test."));

    // Unapply the last one: CLAUDE.md retains user content with no markers; AGENTS.md is deleted.
    let current = read_project_refs(&proj_main, &vault);
    assert_eq!(
        sync_project_refs(&proj_main, &vault, &BTreeSet::new(), &current).unwrap(),
        (0, 0, 1)
    );
    let final_claude = fs::read_to_string(proj_main.join("CLAUDE.md")).unwrap();
    assert_eq!(
        final_claude,
        "# Agent Instructions\n\nAlways run cargo test.\n"
    );
    assert!(!proj_main.join("AGENTS.md").exists());

    // Same for GEMINI.md: preserves "# Gemini Rules\n" with markers gone.
    remove_refs(&proj_gemini, &vault, &["ratatui".into()]).unwrap();
    assert_eq!(
        fs::read_to_string(proj_gemini.join("GEMINI.md")).unwrap(),
        "# Gemini Rules\n"
    );
}
