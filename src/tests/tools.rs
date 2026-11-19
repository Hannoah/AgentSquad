use super::*;
use crate::json;
use crate::tools::*;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn validate_tool_name_and_command() {
    assert_eq!(validate_tool_name("  github-mcp ").unwrap(), "github-mcp");
    assert!(validate_tool_name("").is_err());
    assert!(validate_tool_name("bad.name").is_err());
    assert!(validate_tool_name("bad name").is_err());
    assert!(validate_tool_command(ToolKind::Mcp, "npx -y pkg").is_ok());
    assert!(validate_tool_command(ToolKind::Mcp, "https://example.com/mcp").is_ok());
    assert!(validate_tool_command(ToolKind::Mcp, "   ").is_err());
    assert!(validate_tool_command(ToolKind::Cli, "gh pr list").is_ok());
}

#[test]
fn tool_vault_syncs_mcp_across_all_agents_and_cli_to_squad_tools() {
    let dir = scratch_dir("tools-sync");
    let vault_path = dir.join("tools.toml");
    let proj = dir.join("proj");
    fs::create_dir_all(proj.join(".git/info")).unwrap();
    fs::create_dir_all(proj.join(".codex")).unwrap();
    fs::write(
        proj.join(".mcp.json"),
        "{\n  \"otherTop\": true,\n  \"mcpServers\": {\n    \"local-mcp\": {\n      \"command\": \"echo\",\n      \"args\": [\"hi\"]\n    }\n  }\n}\n",
    )
    .unwrap();
    fs::write(
        proj.join(".codex/config.toml"),
        "model = \"o3\"\n\n[mcp_servers.local-mcp]\ncommand = \"echo\"\nargs = [\"hi\"]\n",
    )
    .unwrap();

    let mut env_map = BTreeMap::new();
    env_map.insert("TOKEN_ENV".to_string(), "1".to_string());

    let mut vault = ToolVault::default();
    assert!(upsert_tool(
        &mut vault,
        ToolEntry {
            name: "github".into(),
            kind: ToolKind::Mcp,
            command: "npx -y @modelcontextprotocol/server-github".into(),
            description: "GitHub MCP".into(),
            env: env_map,
        }
    )
    .unwrap());
    assert!(upsert_tool(
        &mut vault,
        ToolEntry {
            name: "docs".into(),
            kind: ToolKind::Mcp,
            command: "https://docs.example.com/mcp".into(),
            description: "Remote docs".into(),
            env: BTreeMap::new(),
        }
    )
    .unwrap());
    assert!(upsert_tool(
        &mut vault,
        ToolEntry {
            name: "gh-cli".into(),
            kind: ToolKind::Cli,
            command: "gh pr list".into(),
            description: "Inspect open pull requests".into(),
            env: BTreeMap::new(),
        }
    )
    .unwrap());

    save_tools_to(&vault, &vault_path).unwrap();
    let loaded = load_tools_from(&vault_path).unwrap();
    assert_eq!(loaded, vault);

    let (added, updated) = apply_tool_names(
        &proj,
        &loaded,
        &["github".into(), "docs".into(), "gh-cli".into()],
    )
    .unwrap();
    assert_eq!((added, updated), (3, 0));

    // 1. .mcp.json (Claude Code + Muse Code) preserves local-mcp & otherTop and adds github + docs.
    let claude_json = json::parse(&fs::read_to_string(proj.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        claude_json.get("otherTop"),
        Some(&json::JsonValue::Bool(true))
    );
    let claude_srv = claude_json.get("mcpServers").unwrap();
    assert!(claude_srv.get("local-mcp").is_some());
    assert_eq!(
        claude_srv.get("github").and_then(|v| v.get("command")),
        Some(&json::JsonValue::String("npx".into()))
    );
    assert_eq!(
        claude_srv.get("docs").and_then(|v| v.get("type")),
        Some(&json::JsonValue::String("http".into()))
    );

    // 2. .agents/mcp_config.json (AGY) uses mcpServers without type:"http".
    let agy_json =
        json::parse(&fs::read_to_string(proj.join(".agents/mcp_config.json")).unwrap()).unwrap();
    let agy_srv = agy_json.get("mcpServers").unwrap();
    assert_eq!(
        agy_srv.get("github").and_then(|v| v.get("command")),
        Some(&json::JsonValue::String("npx".into()))
    );
    assert_eq!(
        agy_srv.get("docs").and_then(|v| v.get("url")),
        Some(&json::JsonValue::String(
            "https://docs.example.com/mcp".into()
        ))
    );
    assert!(agy_srv.get("docs").and_then(|v| v.get("type")).is_none());

    // 3. .codex/config.toml (Codex CLI) preserves model & local-mcp and adds [mcp_servers.github/docs].
    let codex_text = fs::read_to_string(proj.join(".codex/config.toml")).unwrap();
    assert!(codex_text.contains("model = \"o3\""), "{codex_text}");
    assert!(
        codex_text.contains("[mcp_servers.local-mcp]"),
        "{codex_text}"
    );
    assert!(codex_text.contains("[mcp_servers.github]"), "{codex_text}");
    assert!(codex_text.contains("[mcp_servers.docs]"), "{codex_text}");

    // 4. .squad/TOOLS.md + .git/info/exclude for CLI tools.
    let tools_md = fs::read_to_string(proj.join(".squad/TOOLS.md")).unwrap();
    assert!(tools_md.contains("### gh-cli"), "{tools_md}");
    assert!(tools_md.contains("- Command: `gh pr list`"), "{tools_md}");
    assert!(fs::read_to_string(proj.join(".git/info/exclude"))
        .unwrap()
        .contains(".squad/"));

    let snap = read_project_tools(&proj, &loaded);
    assert_eq!(snap.vault_state.get("github"), Some(&SyncState::Installed));
    assert_eq!(snap.vault_state.get("docs"), Some(&SyncState::Installed));
    assert_eq!(snap.vault_state.get("gh-cli"), Some(&SyncState::Installed));
    assert!(snap
        .local_only
        .contains(&("local-mcp".to_string(), ToolKind::Mcp)));

    // Unchecking all vault tools removes only vault entries, keeping local-mcp and model intact,
    // and removes empty .agents/mcp_config.json + .squad/TOOLS.md.
    let (a, u, r) = sync_project_tools(&proj, &loaded, &BTreeSet::new(), &snap).unwrap();
    assert_eq!((a, u, r), (0, 0, 3));
    let claude_after = json::parse(&fs::read_to_string(proj.join(".mcp.json")).unwrap()).unwrap();
    assert!(claude_after
        .get("mcpServers")
        .unwrap()
        .get("local-mcp")
        .is_some());
    assert!(claude_after
        .get("mcpServers")
        .unwrap()
        .get("github")
        .is_none());
    assert!(!proj.join(".agents/mcp_config.json").exists());
    assert!(!proj.join(".squad/TOOLS.md").exists());
    let codex_after = fs::read_to_string(proj.join(".codex/config.toml")).unwrap();
    assert!(codex_after.contains("model = \"o3\""), "{codex_after}");
    assert!(codex_after.contains("local-mcp"), "{codex_after}");
    assert!(!codex_after.contains("github"), "{codex_after}");

    let _ = fs::remove_dir_all(&dir);
}
