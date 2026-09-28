use super::*;
use std::ffi::OsString;
use std::os::unix::fs::{PermissionsExt, symlink};

pub(super) fn fixture() -> (tempfile::TempDir, Host) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let host = Host {
        config: home.join(".config"),
        data: home.join(".local/share"),
        workspace: root.join("workspace"),
        path: OsString::new(),
        brew: vec![],
        mise: home.join("mise"),
        rustup: home.join(".rustup"),
        rust_toolchain: None,
        developer: None,
        home,
    };
    (directory, host)
}

#[test]
fn selected_native_files_do_not_grant_installation_directories() {
    let (directory, host) = fixture();
    let install = directory.path().canonicalize().unwrap().join("install");
    fs::create_dir(&install).unwrap();
    fs::copy("/bin/echo", install.join("echo")).unwrap();
    let alias = host.home.join("alias");
    symlink(&install, &alias).unwrap();
    let runtime = prepare_for(
        &RuntimeSelection {
            executables: vec![alias.join("echo")],
            ..Default::default()
        },
        &host,
    )
    .unwrap();
    assert!(runtime.native.config.is_none());
    assert!(runtime.native.selected_files.contains(&alias.join("echo")));
    assert!(
        runtime
            .native
            .selected_files
            .contains(&install.join("echo"))
    );
    assert!(!runtime.native.selected_files.contains(&install));
    assert!(!runtime.native.selected_files.contains(&alias));
}

#[test]
fn selected_native_runtime_rejects_private_paths_and_hard_links() {
    let (_directory, host) = fixture();
    for root in [
        &host.workspace,
        &host.home.join(".ssh"),
        &host.home.join(".claude"),
        &host.home.join("Library"),
    ] {
        fs::create_dir_all(root).unwrap();
        let executable = root.join("echo");
        fs::copy("/bin/echo", &executable).unwrap();
        let selection = RuntimeSelection {
            executables: vec![executable],
            ..Default::default()
        };
        assert!(
            prepare_for(&selection, &host)
                .err()
                .unwrap()
                .to_string()
                .contains("overlaps")
        );
    }
    let executable = host.home.join("echo");
    fs::copy("/bin/echo", &executable).unwrap();
    fs::hard_link(&executable, host.workspace.join("hard-link")).unwrap();
    let selection = RuntimeSelection {
        executables: vec![executable],
        ..Default::default()
    };
    assert!(
        prepare_for(&selection, &host)
            .err()
            .unwrap()
            .to_string()
            .contains("hard-link")
    );
}

#[test]
fn selected_native_runtime_rejects_relative_paths_and_missing_resources() {
    let (_directory, host) = fixture();
    for executable in ["./echo", "../echo", "missing-command", "/bin/../bin/echo"] {
        let selection = RuntimeSelection {
            executables: vec![executable.into()],
            ..Default::default()
        };
        assert!(prepare_for(&selection, &host).is_err(), "{executable}");
    }
    let mut selection = RuntimeSelection {
        executables: vec!["echo".into()],
        ..Default::default()
    };
    selection.bundles.push(host.home.join("application"));
    assert!(prepare_for(&selection, &host).is_err());
    selection.dependency_roots = std::mem::take(&mut selection.bundles);
    assert!(prepare_for(&selection, &host).is_err());
}

#[test]
fn selected_native_scripts_require_supported_shebangs() {
    let (_directory, host) = fixture();
    let executable = host.home.join("script");
    let selection = RuntimeSelection {
        executables: vec![executable.clone()],
        ..Default::default()
    };
    for contents in [
        "echo no",
        "#!/usr/bin/env -S bash -e\n",
        "#!/usr/bin/python3\n",
    ] {
        fs::write(&executable, contents).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare_for(&selection, &host).is_err());
    }
    for contents in [
        "#!/bin/bash\nprintf accepted\n",
        "#!/usr/bin/env bash\nprintf accepted\n",
    ] {
        fs::write(&executable, contents).unwrap();
        assert!(prepare_for(&selection, &host).is_ok());
    }
}

#[test]
fn generic_home_rejects_aliases_and_leaves_guest_contents_alone() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let outside = directory.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&outside, &home).unwrap();
    assert!(super::super::prepare_generic_home(&home).is_err());
    assert_eq!(fs::metadata(&outside).unwrap().mode() & 0o777, 0o755);
    fs::remove_file(&home).unwrap();
    super::super::prepare_generic_home(&home).unwrap();
    std::os::unix::fs::symlink(&outside, home.join(".local")).unwrap();
    fs::write(home.join("saved"), "private state").unwrap();
    super::super::prepare_generic_home(&home).unwrap();
    assert_eq!(
        fs::read_to_string(home.join("saved")).unwrap(),
        "private state"
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(super::super::prepare_generic_home(&home).is_err());
    assert_eq!(fs::metadata(&home).unwrap().mode() & 0o777, 0o755);
}

#[test]
fn selected_native_runtime_rejects_setid_executables() {
    let (_directory, host) = fixture();
    let executable = host.home.join("echo");
    fs::copy("/bin/echo", &executable).unwrap();
    let selection = RuntimeSelection {
        executables: vec![executable.clone()],
        ..Default::default()
    };
    for mode in [0o4755, 0o2755] {
        fs::set_permissions(&executable, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            fs::metadata(&executable).unwrap().mode() & 0o6000,
            mode & 0o6000
        );
        assert!(
            prepare_for(&selection, &host)
                .err()
                .unwrap()
                .to_string()
                .contains("setuid/setgid")
        );
    }
}

#[test]
fn selected_native_runtime_rejects_non_system_dylibs() {
    let (_directory, host) = fixture();
    let mut binary = fs::read("/bin/echo").unwrap();
    let original = b"/usr/lib/libSystem.B.dylib";
    let replacement = b"/tmp/lib/libSystem.B.dylib";
    assert_eq!(original.len(), replacement.len());
    let offsets: Vec<_> = binary
        .windows(original.len())
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == original).then_some(index))
        .collect();
    assert!(!offsets.is_empty());
    for offset in offsets {
        binary[offset..offset + original.len()].copy_from_slice(replacement);
    }
    let executable = host.home.join("echo");
    fs::write(&executable, binary).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let selection = RuntimeSelection {
        executables: vec![executable],
        ..Default::default()
    };
    assert!(
        prepare_for(&selection, &host)
            .err()
            .unwrap()
            .to_string()
            .contains("non-system library")
    );
}
