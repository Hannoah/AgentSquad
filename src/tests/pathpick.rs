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
    assert_eq!(
        complete(&format!("{r}{S}oth")).unwrap(),
        format!("{r}{S}Other{S}"),
        "case follows the disk"
    );
    assert_eq!(
        complete(&format!("{r}{S}WO")).unwrap(),
        format!("{r}{S}work-"),
        "folders only, never workfile"
    );
    assert!(complete(&format!("{r}{S}zzz")).is_none());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn suggest_lists_matching_subfolders_and_hides_dot_folders_until_asked() {
    let root = tree("pp-suggest");
    let r = root.display();
    let all = suggest(&format!("{r}{S}"));
    let labels: Vec<_> = all.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(
        labels,
        [
            format!("Other{S}"),
            format!("work-a{S}"),
            format!("work-b{S}")
        ]
    );
    assert_eq!(
        all[1].1,
        format!("{r}{S}work-a{S}"),
        "a pick fills the full path"
    );
    assert_eq!(suggest(&format!("{r}{S}WO")).len(), 2, "prefix, any case");
    assert_eq!(
        suggest(&format!("{r}{S}.h"))[0].1,
        format!("{r}{S}.hidden{S}")
    );
    assert_eq!(
        suggest(&format!("\"{r}{S}oth"))[0].1,
        format!("\"{r}{S}Other{S}"),
        "an opening quote is kept"
    );
    assert!(suggest(&format!("{r}{S}zzz")).is_empty());
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
#[cfg(unix)]
fn roots_rank_parent_folders_by_use_unix() {
    let mk = |p: &str| Project {
        name: "x".into(),
        path: PathBuf::from(p),
        command: vec!["claude".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
    };
    let projects = [mk("/one/a"), mk("/two/b"), mk("/two/c"), mk("/x")];
    let got: Vec<String> = roots(&projects).into_iter().map(|(_, t)| t).collect();
    assert_eq!(got, ["/two/", "/one/", "/"]);
}
#[test]
#[cfg(windows)] // Windows path syntax
fn roots_rank_parent_folders_by_use() {
    let mk = |p: &str| Project {
        name: "x".into(),
        path: PathBuf::from(p),
        command: vec!["claude".into()],
        kind: Kind::Agent,
        autostart: false,
        ledger: false,
        reports_to: None,
        pinned: false,
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
