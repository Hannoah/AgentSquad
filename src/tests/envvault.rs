use super::*;
use crate::envvault::*;

#[test]
fn validate_env_key_accepts_posix_names_and_rejects_bad() {
    assert_eq!(
        validate_env_key("  OPENAI_API_KEY  ").unwrap(),
        "OPENAI_API_KEY"
    );
    assert_eq!(validate_env_key("_LOCAL_1").unwrap(), "_LOCAL_1");
    assert!(validate_env_key("").is_err());
    assert!(validate_env_key("1BAD").is_err());
    assert!(validate_env_key("BAD KEY").is_err());
    assert!(validate_env_key("BAD=KEY").is_err());
}

#[test]
fn vault_roundtrip_upsert_remove_and_mask() {
    let dir = scratch_dir("envvault-rt");
    let path = dir.join("env.toml");
    let mut vault = EnvVault::default();
    assert!(upsert_env(&mut vault, "OPENAI_API_KEY", "sk-1234567890").unwrap());
    assert!(!upsert_env(&mut vault, "OPENAI_API_KEY", "sk-updated99").unwrap());
    assert!(upsert_env(&mut vault, "SHORT", "abcd").unwrap());
    save_env_to(&vault, &path).unwrap();

    let loaded = load_env_from(&path).unwrap();
    assert_eq!(loaded, vault);
    assert_eq!(mask_value("abcd"), "••••");
    assert_eq!(mask_value("sk-updated99"), "sk••••99");

    assert!(remove_env(&mut vault, "SHORT"));
    assert!(!remove_env(&mut vault, "MISSING"));
    assert_eq!(vault.vars.len(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn apply_dotenv_creates_updates_and_preserves_comments() {
    let dir = scratch_dir("envvault-dotenv");
    let dotenv = dir.join(".env");
    fs::write(
        &dotenv,
        "# Header comment\nPORT=3000\nexport API_KEY=\"old-key\"\nKEEP=hello world\n",
    )
    .unwrap();

    let entries = vec![
        EnvEntry {
            key: "API_KEY".into(),
            value: "new value # with hash".into(),
        },
        EnvEntry {
            key: "ANTHROPIC_API_KEY".into(),
            value: "sk-ant-abc123".into(),
        },
    ];
    let (added, updated) = apply_dotenv(&dir, &entries).unwrap();
    assert_eq!((added, updated), (1, 1));

    let text = fs::read_to_string(&dotenv).unwrap();
    assert!(text.contains("# Header comment"), "{text}");
    assert!(text.contains("PORT=3000"), "{text}");
    assert!(text.contains("KEEP=hello world"), "{text}");
    assert!(
        text.contains("export API_KEY=\"new value # with hash\""),
        "preserved export prefix and quoted special chars:\n{text}"
    );
    assert!(text.contains("ANTHROPIC_API_KEY=sk-ant-abc123"), "{text}");

    let parsed = read_dotenv(&dir);
    assert_eq!(
        parsed.get("API_KEY").map(String::as_str),
        Some("new value # with hash")
    );
    assert_eq!(
        parsed.get("ANTHROPIC_API_KEY").map(String::as_str),
        Some("sk-ant-abc123")
    );
    assert_eq!(parsed.get("PORT").map(String::as_str), Some("3000"));
    let _ = fs::remove_dir_all(&dir);
}
