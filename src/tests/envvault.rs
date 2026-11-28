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

#[test]
fn apply_dotenv_keeps_env_out_of_git_once() {
    let entry = EnvEntry {
        key: "K".into(),
        value: "v".into(),
    };
    let repo = scratch_dir("envvault-exclude");
    fs::create_dir_all(repo.join(".git")).unwrap();
    apply_dotenv(&repo, std::slice::from_ref(&entry)).unwrap();
    apply_dotenv(&repo, std::slice::from_ref(&entry)).unwrap();
    let exclude = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.lines().filter(|l| *l == ".env").count(), 1);

    // Not a repository: nothing is created.
    let plain = scratch_dir("envvault-exclude-plain");
    apply_dotenv(&plain, std::slice::from_ref(&entry)).unwrap();
    assert!(!plain.join(".git").exists());
}

#[test]
fn propagate_env_updates_renames_and_deletes_only_holders() {
    let a = scratch_dir("envvault-prop-a");
    let b = scratch_dir("envvault-prop-b");
    fs::write(a.join(".env"), "KEEP=1\nAPI_KEY=old\n").unwrap();
    fs::write(b.join(".env"), "KEEP=2\n").unwrap();
    let dirs = [a.as_path(), b.as_path()];
    let entry = |k: &str, v: &str| EnvEntry {
        key: k.into(),
        value: v.into(),
    };

    // Value change touches only the holder; a repeat is a no-op.
    let new = entry("API_KEY", "new");
    assert_eq!(propagate_env(dirs, "API_KEY", Some(&new)), (1, vec![]));
    assert_eq!(propagate_env(dirs, "API_KEY", Some(&new)), (0, vec![]));
    assert_eq!(read_dotenv(&a).get("API_KEY").unwrap(), "new");
    assert!(!read_dotenv(&b).contains_key("API_KEY"));

    // Rename drops the old key and writes the new one.
    let renamed = entry("TOKEN", "new");
    assert_eq!(propagate_env(dirs, "API_KEY", Some(&renamed)), (1, vec![]));
    let env_a = read_dotenv(&a);
    assert!(!env_a.contains_key("API_KEY"));
    assert_eq!(env_a.get("TOKEN").unwrap(), "new");

    // Delete removes it and keeps unrelated lines.
    assert_eq!(propagate_env(dirs, "TOKEN", None), (1, vec![]));
    let env_a = read_dotenv(&a);
    assert!(!env_a.contains_key("TOKEN"));
    assert_eq!(env_a.get("KEEP").unwrap(), "1");
    let _ = fs::remove_dir_all(&a);
    let _ = fs::remove_dir_all(&b);
}
#[test]
fn llm_models_write_one_per_provider_into_dotenv() {
    use crate::models::*;
    let dir = scratch_dir("models-sync");
    let m = |name: &str, provider, model: &str, token: &str| ModelEntry {
        name: name.into(),
        provider,
        model: model.into(),
        endpoint: String::new(),
        token: token.into(),
    };
    let vault = ModelVault {
        models: vec![
            m("opus", Provider::Anthropic, "claude-opus-5-5", "sk-a"),
            m("sonnet", Provider::Anthropic, "claude-sonnet-5-5", ""),
            m("gpt", Provider::Openai, "gpt-x", "sk-o"),
        ],
    };
    fs::write(dir.join(".env"), "KEEP=1\nOPENAI_API_KEY=mine\n").unwrap();
    let pick = |names: &[&str]| names.iter().map(|s| s.to_string()).collect();

    // Pick opus: its model and token land; a hand-written OpenAI key survives.
    let env = read_dotenv(&dir);
    sync_project_models(&dir, &vault, &pick(&["opus"]), &env).unwrap();
    let env = read_dotenv(&dir);
    assert_eq!(env["ANTHROPIC_MODEL"], "claude-opus-5-5");
    assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "sk-a");
    assert_eq!(env["OPENAI_API_KEY"], "mine");
    assert_eq!(env["KEEP"], "1");
    assert_eq!(applied_names(&vault, &env), pick(&["opus"]));

    // Switch to sonnet: opus's token goes, since sonnet has none.
    sync_project_models(&dir, &vault, &pick(&["sonnet"]), &env).unwrap();
    let env = read_dotenv(&dir);
    assert_eq!(env["ANTHROPIC_MODEL"], "claude-sonnet-5-5");
    assert!(!env.contains_key("ANTHROPIC_AUTH_TOKEN"));

    // Unpick everything: the managed provider is cleared, the rest stays.
    sync_project_models(&dir, &vault, &pick(&[]), &env).unwrap();
    let env = read_dotenv(&dir);
    assert!(!env.contains_key("ANTHROPIC_MODEL"));
    assert_eq!(env["OPENAI_API_KEY"], "mine");
    assert!(is_model_key("GEMINI_API_KEY") && !is_model_key("KEEP"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn llm_model_test_pings_the_endpoint_and_reads_the_reply() {
    use crate::models::*;
    use std::io::{Read, Write};
    let m = |provider, endpoint: &str| ModelEntry {
        name: "t".into(),
        provider,
        model: "m\"1".into(),
        endpoint: endpoint.into(),
        token: "sek".into(),
    };
    let (url, h, body) = test_request(&m(Provider::Anthropic, ""));
    assert_eq!(url, "https://api.anthropic.com/v1/messages");
    assert!(h.contains(&"Authorization: Bearer sek".to_string()));
    assert!(body.contains(r#""model":"m\"1""#));
    let (url, _, _) = test_request(&m(Provider::Openai, "http://x/v1/"));
    assert_eq!(url, "http://x/v1/chat/completions");
    let (url, h, _) = test_request(&m(Provider::Gemini, ""));
    assert!(url.ends_with("/v1beta/models/m\"1:generateContent"));
    assert!(h.contains(&"x-goog-api-key: sek".to_string()));

    assert_eq!(
        parse_test_reply(r#"{"content":[{"type":"text","text":" OK "}]}"#).as_deref(),
        Some("OK")
    );
    assert_eq!(
        parse_test_reply(r#"{"choices":[{"message":{"content":"OK"}}]}"#).as_deref(),
        Some("OK")
    );
    assert_eq!(
        parse_test_reply(r#"{"candidates":[{"content":{"parts":[{"text":"OK"}]}}]}"#).as_deref(),
        Some("OK")
    );
    assert_eq!(
        parse_test_reply(r#"{"error":{"message":"bad key"}}"#).as_deref(),
        Some("bad key")
    );

    // Full round trip through curl against a one-shot loopback server.
    let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", srv.local_addr().unwrap());
    let t = std::thread::spawn(move || {
        let (mut s, _) = srv.accept().unwrap();
        let mut req = Vec::new();
        let mut buf = [0u8; 4096];
        while !String::from_utf8_lossy(&req).contains("Reply with OK") {
            let n = s.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            req.extend_from_slice(&buf[..n]);
        }
        let body = r#"{"error":{"message":"bad key"}}"#;
        let _ = write!(
            s,
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        String::from_utf8_lossy(&req).into_owned()
    });
    let got = test_model(&m(Provider::Openai, &base));
    let req = t.join().unwrap();
    assert!(req.contains("Authorization: Bearer sek"), "{req}");
    assert_eq!(got, Err("HTTP 401: bad key".into()));
}
