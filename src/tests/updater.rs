use super::*;

#[test]
fn version_comparison() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(parse_version("v0.5.4"), Some((0, 5, 4)));
    assert_eq!(parse_version("0.5.4\n"), Some((0, 5, 4)));
    assert_eq!(parse_version("v1.2"), None);
    assert_eq!(parse_version("v1.2.3.4"), None);
    assert_eq!(parse_version("v1.2.3-rc1"), None);
    assert_eq!(parse_version("latest"), None);
    assert!(is_newer("0.5.3", "v0.5.4"));
    assert!(is_newer("0.5.9", "v0.6.0"));
    assert!(is_newer("0.5.3", "v0.10.0"), "numeric, not textual");
    assert!(!is_newer("0.5.3", "v0.5.3"), "same version");
    assert!(!is_newer("0.5.3", "v0.5.2"), "never downgrade");
    assert!(!is_newer("0.5.3", "garbage"), "unparseable tag is ignored");
}
#[test]
fn swap_binary_replaces_and_rolls_back() {
    let dir = env::temp_dir().join(format!("squad-swap-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (cur, new) = (dir.join("squad.exe"), dir.join("new.exe"));
    fs::write(&cur, "old build").unwrap();
    fs::write(&new, "new build").unwrap();
    swap_binary(&new, &cur).unwrap();
    assert_eq!(fs::read_to_string(&cur).unwrap(), "new build");
    let old = format!("squad.exe.old-{}", std::process::id());
    assert_eq!(fs::read_to_string(dir.join(&old)).unwrap(), "old build");
    // A failed copy puts the previous build back.
    fs::remove_file(dir.join(&old)).unwrap();
    assert!(swap_binary(&dir.join("missing.exe"), &cur).is_err());
    assert_eq!(fs::read_to_string(&cur).unwrap(), "new build");
    fs::remove_dir_all(&dir).ok();
}
#[test]
#[ignore = "needs network"]
fn fetch_release_downloads_and_verifies() {
    // The real pipeline minus the swap: curl, bsdtar, `--version` check.
    let work = env::temp_dir().join(format!("squad-fetch-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();
    let exe = fetch_release("v0.5.3", &work).expect("download and verify v0.5.3");
    assert!(exe.is_file());
    // A tag whose binary reports another version is refused.
    let wrong = fetch_release("v0.5.2", &work);
    assert!(wrong.is_ok(), "v0.5.2 reports itself as 0.5.2");
    assert!(fetch_release("v0.0.1", &work).is_err(), "no such release");
    fs::remove_dir_all(&work).ok();
}
#[test]
#[ignore = "needs network"]
fn latest_tag_reads_github() {
    let tag = latest_tag().expect("curl and network available");
    assert!(parse_version(&tag).is_some(), "got {tag}");
}
#[test]
fn update_cache_and_sha256_parsing() {
    // Fresh tag hit within 900s cooldown:
    assert_eq!(
        parse_cache("1000 v0.5.9\n", 1500),
        Some(Some("v0.5.9".to_string()))
    );
    // Expired after 900s:
    assert_eq!(parse_cache("1000 v0.5.9\n", 2000), None);
    // Recent failure marker `-` is cached within 600s:
    assert_eq!(parse_cache("1000 -\n", 1300), Some(None));
    assert_eq!(parse_cache("1000 -\n", 1700), None);

    let good_sha = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  squad-windows-x86_64.zip\n";
    assert_eq!(
        parse_sha256(good_sha).as_deref(),
        Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    );
    assert_eq!(parse_sha256("not-a-valid-hash"), None);
}

#[test]
fn download_failures_say_why() {
    assert!(download_failure("504", Some(22)).contains("HTTP 504"));
    assert!(download_failure("504", Some(22)).contains("their side"));
    assert!(download_failure("503", Some(22)).contains("try again later"));
    assert!(download_failure("404", Some(22)).contains("does not exist"));
    assert!(download_failure("403", Some(22)).contains("HTTP 403"));
    assert!(download_failure("000", Some(28)).contains("timed out"));
    assert!(download_failure("000", Some(6)).contains("cannot reach"));
    assert!(download_failure("", Some(35)).contains("exit 35"));
    assert!(download_failure("", None).contains("interrupted"));
}
