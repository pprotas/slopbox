use super::*;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;

#[test]
fn bundle_grants_include_only_selected_trees_and_validated_aliases() {
    let (_temporary, host) = super::super::tests::fixture();
    let app = host.home.join("app");
    let data = host.home.join("data");
    fs::create_dir(&app).unwrap();
    fs::create_dir(&data).unwrap();
    fs::write(data.join("value"), "resource").unwrap();
    symlink("../data", app.join("data")).unwrap();
    let alias = host.home.join("current");
    symlink(&app, &alias).unwrap();
    let boundary = Boundary::new(&host).unwrap();
    assert!(
        discover(
            std::slice::from_ref(&alias),
            &host,
            &boundary,
            &BTreeSet::new()
        )
        .is_err()
    );
    let roots = discover(
        &[alias.clone(), data.clone()],
        &host,
        &boundary,
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(
        roots.roots.into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([alias, app, data])
    );
}

#[test]
fn bundles_reject_private_roots_aliases_hard_links_and_sockets() {
    let (_temporary, host) = super::super::tests::fixture();
    let boundary = Boundary::new(&host).unwrap();
    for root in [
        &host.home,
        &host.workspace,
        &host.home.join(".ssh"),
        Path::new("/opt/homebrew"),
    ] {
        assert!(discover(&[root.to_owned()], &host, &boundary, &BTreeSet::new()).is_err());
    }
    let app = host.home.join("app");
    fs::create_dir(&app).unwrap();
    let secret = host.home.join(".ssh");
    fs::create_dir(&secret).unwrap();
    fs::write(secret.join("key"), "canary").unwrap();
    symlink(&secret, app.join("escape")).unwrap();
    assert!(
        discover(
            std::slice::from_ref(&app),
            &host,
            &boundary,
            &BTreeSet::new()
        )
        .is_err()
    );
    fs::remove_file(app.join("escape")).unwrap();
    fs::hard_link(secret.join("key"), app.join("hard-link")).unwrap();
    assert!(
        discover(
            std::slice::from_ref(&app),
            &host,
            &boundary,
            &BTreeSet::new()
        )
        .unwrap_err()
        .to_string()
        .contains("hard-link")
    );
    fs::remove_file(app.join("hard-link")).unwrap();
    let _socket = UnixListener::bind(app.join("socket")).unwrap();
    assert!(
        discover(&[app], &host, &boundary, &BTreeSet::new())
            .unwrap_err()
            .to_string()
            .contains("special file")
    );
}

#[test]
fn private_directory_case_aliases_are_rejected_by_identity() {
    let (_temporary, host) = super::super::tests::fixture();
    let private = host.home.join(".ssh");
    fs::create_dir(&private).unwrap();
    fs::write(private.join("key"), "canary").unwrap();
    let alias = host.home.join(".SSH");
    if !alias.exists() {
        return;
    } // Case-sensitive filesystem.
    assert_eq!(
        fs::metadata(&private).unwrap().ino(),
        fs::metadata(&alias).unwrap().ino()
    );
    let boundary = Boundary::new(&host).unwrap();
    assert!(boundary.check(&alias).is_err());
    assert!(boundary.check(&alias.join("key")).is_err());
    assert!(boundary.check(Path::new("/sYsTeM/VoLuMeS/Data")).is_err());
    assert!(
        discover(
            &[PathBuf::from("/APPLICATIONS")],
            &host,
            &boundary,
            &BTreeSet::new()
        )
        .unwrap_err()
        .to_string()
        .contains("broad host directory")
    );
    assert!(discover(&[alias], &host, &boundary, &BTreeSet::new()).is_err());
}

#[test]
fn bundles_reject_parent_traversal_across_aliases() {
    let (_temporary, host) = super::super::tests::fixture();
    let app = host.home.join("app");
    fs::create_dir_all(app.join("other/child")).unwrap();
    fs::write(app.join("data"), "one").unwrap();
    fs::write(app.join("other/data"), "two").unwrap();
    symlink("other/child", app.join("alias")).unwrap();
    symlink("alias/../data", app.join("confused")).unwrap();
    assert!(
        discover(
            &[app],
            &host,
            &Boundary::new(&host).unwrap(),
            &BTreeSet::new()
        )
        .unwrap_err()
        .to_string()
        .contains("alias before '..'")
    );
}
