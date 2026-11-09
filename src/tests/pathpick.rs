use super::*;
use crate::pathpick::*;
use std::path::MAIN_SEPARATOR as S;

/// `root` holds work-a, work-b, Other (folders), workfile (a file), .hidden.
fn tree(tag: &str) -> PathBuf {
    let root = scratch_dir(tag);
    for d in ["work-a", "work-b", "Other", ".hidden"] {
        fs::create_dir(root.join(d)).unwrap();
    }
    fs::write(root.join("workfile"), "x").unwrap();
    root
}

#[test]
fn tab_completes_one_match_and_the_shared_prefix_of_many() {
    let root = tree("pp-complete");
    let r = root.display();
    let one = complete(&format!("{r}{S}oth")).unwrap();
    assert_eq!(one.text, format!("{r}{S}Other{S}"), "case follows the disk");
    assert!(one.picks.is_empty());

    let many = complete(&format!("{r}{S}WO")).unwrap();
    assert_eq!(
        many.text,
        format!("{r}{S}work-"),
        "folders only, never workfile"
    );
    assert_eq!(many.picks.len(), 2);
    assert_eq!(many.picks[0].0, format!("work-a{S}"));
    assert_eq!(many.picks[1].1, format!("{r}{S}work-b{S}"));

    assert!(complete(&format!("{r}{S}zzz")).is_none());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn completion_hides_dot_folders_until_asked_and_keeps_quotes() {
    let root = tree("pp-dot");
    let r = root.display();
    let all = complete(&format!("{r}{S}")).unwrap();
    assert_eq!(all.picks.len(), 3, "Other, work-a, work-b; not .hidden");
    assert_eq!(all.text, format!("{r}{S}"));
    let dot = complete(&format!("{r}{S}.h")).unwrap();
    assert_eq!(dot.text, format!("{r}{S}.hidden{S}"));
    let quoted = complete(&format!("\"{r}{S}oth")).unwrap();
    assert_eq!(quoted.text, format!("\"{r}{S}Other{S}"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn check_tells_folder_from_file_and_nothing() {
    let root = tree("pp-check");
    let r = root.display();
    assert_eq!(check(""), PathState::Blank);
    assert_eq!(
        check(&format!(" \"{r}\" ")),
        PathState::Folder,
        "quotes are cleaned"
    );
    assert_eq!(check(&format!("{r}{S}workfile")), PathState::NotFolder);
    assert_eq!(check(&format!("{r}{S}nope")), PathState::Missing);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn roots_rank_parent_folders_by_use() {
    let mk = |p: &str| Project {
        name: "x".into(),
        path: PathBuf::from(p),
        command: vec!["claude".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
    };
    let projects = [
        mk(r"C:\one\a"),
        mk(r"C:\two\b"),
        mk(r"c:\TWO\c"),
        mk(r"C:\x"),
    ];
    let got: Vec<String> = roots(&projects).into_iter().map(|(_, t)| t).collect();
    assert_eq!(
        got,
        [r"C:\two\", r"C:\one\", r"C:\"],
        "most used first, no double separator"
    );
}
