use super::*;
use crate::skills::*;
use std::collections::BTreeSet;

#[test]
fn validate_skill_name_normalizes_and_rejects_bad() {
    assert_eq!(validate_skill_name("  API-Test_1 ").unwrap(), "api-test_1");
    assert!(validate_skill_name("").is_err());
    assert!(validate_skill_name("-lead").is_err());
    assert!(validate_skill_name("bad name").is_err());
    assert!(validate_skill_name("../escape").is_err());
    assert!(validate_skill_name("a/b").is_err());
}

#[test]
fn skill_vault_roundtrip_and_multi_agent_sync() {
    let dir = scratch_dir("skills-sync");
    let vault_path = dir.join("skills.toml");
    let src_dir = dir.join("src-skill");
    let proj = dir.join("proj");
    fs::create_dir_all(src_dir.join("scripts")).unwrap();
    fs::create_dir_all(proj.join(".agents/skills/local-only")).unwrap();
    fs::write(
        src_dir.join("SKILL.md"),
        "---\nname: repo-lint\ndescription: Run repo linter checks.\n---\n\n# Repo Lint\nRun checks.\n",
    )
    .unwrap();
    fs::write(src_dir.join("scripts/check.sh"), "#!/bin/sh\necho ok\n").unwrap();
    fs::write(
        proj.join(".agents/skills/local-only/SKILL.md"),
        "---\nname: local-only\ndescription: Hand-written project skill.\n---\n",
    )
    .unwrap();

    let mut vault = SkillVault::default();
    assert!(upsert_skill(
        &mut vault,
        SkillEntry {
            name: "inline-guide".into(),
            description: "Guide for testing".into(),
            source_path: None,
            instructions: "Always run cargo test first.".into(),
        }
    )
    .unwrap());
    assert!(upsert_skill(
        &mut vault,
        SkillEntry {
            name: "repo-lint".into(),
            description: String::new(), // inferred from SKILL.md frontmatter
            source_path: Some(src_dir.display().to_string()),
            instructions: String::new(),
        }
    )
    .unwrap());
    assert_eq!(vault.skills[1].description, "Run repo linter checks.");

    save_skills_to(&vault, &vault_path).unwrap();
    let loaded = load_skills_from(&vault_path).unwrap();
    assert_eq!(loaded, vault);

    let (added, updated) =
        apply_skill_names(&proj, &loaded, &["inline-guide".into(), "repo-lint".into()]).unwrap();
    assert_eq!((added, updated), (2, 0));

    // Both universal (.agents/skills for agy/codex/muse/claude) and Claude (.claude/skills) exist.
    for root in SKILL_ROOTS {
        let inline_md = fs::read_to_string(proj.join(root).join("inline-guide/SKILL.md")).unwrap();
        assert!(inline_md.contains("name: inline-guide"), "{inline_md}");
        assert!(
            inline_md.contains("description: \"Guide for testing\""),
            "{inline_md}"
        );
        assert!(
            inline_md.contains("Always run cargo test first."),
            "{inline_md}"
        );

        assert!(proj.join(root).join("repo-lint/SKILL.md").is_file());
        assert_eq!(
            fs::read_to_string(proj.join(root).join("repo-lint/scripts/check.sh")).unwrap(),
            "#!/bin/sh\necho ok\n"
        );
    }

    let snap = read_project_skills(&proj, &loaded);
    assert_eq!(
        snap.vault_state.get("inline-guide"),
        Some(&SyncState::Installed)
    );
    assert_eq!(
        snap.vault_state.get("repo-lint"),
        Some(&SyncState::Installed)
    );
    assert_eq!(snap.local_only, vec!["local-only".to_string()]);

    // Removing one root marks the skill as Differs; syncing repairs it and leaves local-only intact.
    fs::remove_dir_all(proj.join(".claude/skills/repo-lint")).unwrap();
    let snap2 = read_project_skills(&proj, &loaded);
    assert_eq!(
        snap2.vault_state.get("repo-lint"),
        Some(&SyncState::Differs)
    );

    let mut keep = BTreeSet::new();
    keep.insert("repo-lint".to_string());
    let (a, u, r) = sync_project_skills(&proj, &loaded, &keep, &snap2).unwrap();
    assert_eq!((a, u, r), (0, 1, 1));
    assert!(proj.join(".claude/skills/repo-lint/SKILL.md").is_file());
    assert!(!proj.join(".agents/skills/inline-guide").exists());
    assert!(!proj.join(".claude/skills/inline-guide").exists());
    assert!(
        proj.join(".agents/skills/local-only/SKILL.md").is_file(),
        "local-only skill must never be removed"
    );

    // Self-copy guard: source_path pointing at the installed destination itself must not wipe it.
    let self_src = proj.join(".agents/skills/repo-lint");
    let mut self_vault = SkillVault::default();
    upsert_skill(
        &mut self_vault,
        SkillEntry {
            name: "repo-lint".into(),
            description: "Run repo linter checks.".into(),
            source_path: Some(self_src.display().to_string()),
            instructions: String::new(),
        },
    )
    .unwrap();
    apply_skill_names(&proj, &self_vault, &["repo-lint".into()]).unwrap();
    assert!(proj
        .join(".agents/skills/repo-lint/scripts/check.sh")
        .is_file());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn classify_source_or_body_distinguishes_paths_from_inline() {
    let dir = scratch_dir("skills-classify");
    let empty_folder = dir.join("empty-skill");
    fs::create_dir_all(&empty_folder).unwrap();
    assert!(classify_source_or_body(empty_folder.to_str().unwrap()).is_err());
    assert!(classify_source_or_body("~/definitely-nonexistent-skill-path").is_err());

    let (src, body) = classify_source_or_body("Run unit tests before finishing.").unwrap();
    assert!(src.is_none());
    assert_eq!(body, "Run unit tests before finishing.");
    let _ = fs::remove_dir_all(&dir);
}
