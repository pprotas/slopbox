use super::*;
use crate::backend::{BrokerConnections, ExecutionPlan, Workspace};
use crate::harness::PreparedHarness;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

impl Fixture {
    fn profile(&self, tool: bool) -> String {
        let base = self.host.home.parent().unwrap();
        let config = Config {
            node: env::current_exe().unwrap().canonicalize().unwrap(),
            pi_cli: base.join("pi/dist/cli.js"),
            tool_timeout_seconds: 10,
        };
        let tools = DeveloperTools::from_host(&self.host, &config.node).unwrap();
        let runtime = RuntimePlan {
            native: NativeRuntime {
                config: Some(config),
                tools,
                selected_files: Vec::new(),
                system_data: Vec::new(),
            },
            path: OsString::new(),
        };
        crate::backend::macos::profile::render(
            &ExecutionPlan {
                workspace: Workspace {
                    source: &self.host.workspace,
                    target: &self.host.workspace,
                    writable: true,
                },
                private_home: &base.join("harness-home"),
                tool_home: &base.join("tool-home"),
                tool_cache: &base.join("cache"),
                session_dir: &base.join("session"),
                dev_environment: None,
                runtime: &runtime,
                harness: &PreparedHarness::default(),
                brokers: &BrokerConnections::default(),
                environment: &[],
                private_terminal: false,
                clipboard: false,
            },
            tool,
            &base.join("socket"),
            &base.join("worker"),
        )
        .unwrap()
    }
}

#[test]
fn opt_links_are_literal_read_only_grants_in_the_tool_role() {
    let fixture = Fixture::new();
    let brew = &fixture.host.brew[0];
    fs::create_dir_all(brew.join("Cellar/fixture/1")).unwrap();
    fs::create_dir(brew.join("opt")).unwrap();
    symlink("../Cellar/fixture/1", brew.join("opt/fixture")).unwrap();
    let profile = fixture.profile(true);
    assert!(profile.contains(&format!(
        "(allow file-read* (literal \"{}\"))",
        brew.join("opt/fixture").display()
    )));
    assert!(profile.contains(&format!(
        "(allow file-read-metadata (literal \"{}\"))",
        brew.join("opt").display()
    )));
    assert!(!profile.contains(&format!("(subpath \"{}\")", brew.join("opt").display())));
    assert!(!profile.contains(&format!(
        "(subpath \"{}\")",
        brew.join("opt/fixture").display()
    )));
    assert!(
        !profile
            .lines()
            .any(|line| line.contains("file-write") && line.contains(brew.to_str().unwrap()))
    );
    assert!(!fixture.profile(false).contains(brew.to_str().unwrap()));
}

#[test]
#[ignore = "host-only: production Seatbelt profile and installed Apple clang"]
fn opt_dependency_loading_and_denials() {
    let fixture = Fixture::new();
    let base = fixture.host.home.parent().unwrap();
    let brew = &fixture.host.brew[0];
    let keg = brew.join("Cellar/fixture/1");
    let opt = brew.join("opt");
    for directory in [
        keg.join("bin"),
        keg.join("lib"),
        opt.clone(),
        brew.join("etc"),
        brew.join("var"),
        fixture.host.home.join(".ssh"),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(fixture.host.workspace.join("canary"), "workspace").unwrap();
    fs::write(keg.join("lib/data"), "public").unwrap();
    fs::write(fixture.host.home.join(".ssh/key"), "private").unwrap();
    fs::write(brew.join("etc/config"), "configuration").unwrap();
    fs::write(brew.join("var/state"), "state").unwrap();
    fs::write(opt.join("unpackaged"), "not an installation").unwrap();
    for name in ["fixture", "retarget-directory", "retarget-file"] {
        symlink("../Cellar/fixture/1", opt.join(name)).unwrap();
    }
    symlink(fixture.host.home.join(".ssh/key"), opt.join("private-file")).unwrap();
    symlink(fixture.host.home.join(".ssh/key"), keg.join("lib/escape")).unwrap();
    let source = fixture.host.workspace.join("library.c");
    fs::write(&source, "int fixture_value(void) { return 42; }\n").unwrap();
    let library = keg.join("lib/libfixture.dylib");
    let output = Command::new("/usr/bin/xcrun")
        .args(["--sdk", "macosx", "clang", "-dynamiclib"])
        .arg(&source)
        .arg("-install_name")
        .arg(opt.join("fixture/lib/libfixture.dylib"))
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let source = fixture.host.workspace.join("consumer.c");
    fs::write(&source, "#include <stdio.h>\nextern int fixture_value(void);\nint main(void) { printf(\"%d\\n\", fixture_value()); return 0; }\n").unwrap();
    let consumer = keg.join("bin/consumer");
    let output = Command::new("/usr/bin/xcrun")
        .args(["--sdk", "macosx", "clang"])
        .arg(source)
        .arg(&library)
        .arg("-o")
        .arg(&consumer)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let profile = fixture.profile(true);
    let output = Command::new("/usr/bin/sandbox-exec")
        .env_clear()
        .current_dir(&fixture.host.workspace)
        .args(["-p", &profile])
        .arg(consumer)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"42\n");

    let helper = "backend::macos::runtime::tests::homebrew::opt_access_fixture";
    let mut child = Command::new("/usr/bin/sandbox-exec")
        .env_clear()
        .env("SLOPBOX_TEST_BREW_ROOT", base)
        .current_dir(&fixture.host.workspace)
        .args(["-p", &profile])
        .arg(env::current_exe().unwrap())
        .args([
            "--exact",
            helper,
            "--ignored",
            "--nocapture",
            "--format=terse",
            "--test-threads=1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(
            stdout.read_line(&mut line).unwrap(),
            0,
            "fixture exited before ready"
        );
        if line.contains("homebrew-ready") {
            break;
        }
    }
    // Change already-granted links after sandbox application, not before compile.
    for (name, target) in [
        ("retarget-directory", fixture.host.home.join(".ssh")),
        ("retarget-file", fixture.host.home.join(".ssh/key")),
    ] {
        fs::remove_file(opt.join(name)).unwrap();
        symlink(target, opt.join(name)).unwrap();
    }
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"continue\n")
        .unwrap();
    let status = child.wait().unwrap();
    let mut output = String::new();
    stdout.read_to_string(&mut output).unwrap();
    assert!(status.success(), "{output}");

    let output = Command::new("/usr/bin/sandbox-exec")
        .env_clear()
        .env("SLOPBOX_TEST_BREW_ROOT", base)
        .env("SLOPBOX_TEST_BREW_HARNESS", "1")
        .current_dir(&fixture.host.workspace)
        .args(["-p", &fixture.profile(false)])
        .arg(env::current_exe().unwrap())
        .args([
            "--exact",
            helper,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(keg.join("lib/data")).unwrap(), b"public");
    assert_eq!(
        fs::read(fixture.host.home.join(".ssh/key")).unwrap(),
        b"private"
    );
}

#[test]
#[ignore = "subprocess fixture for opt_dependency_loading_and_denials"]
fn opt_access_fixture() {
    let base = PathBuf::from(env::var_os("SLOPBOX_TEST_BREW_ROOT").expect("fixture root required"));
    assert_eq!(
        fs::read(base.join("workspace/canary")).unwrap(),
        b"workspace"
    );
    let public = base.join("brew/opt/fixture/lib/data");
    if env::var_os("SLOPBOX_TEST_BREW_HARNESS").is_some() {
        assert_eq!(
            fs::read(public).unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(
            fs::read(base.join("brew/Cellar/fixture/1/lib/data"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EPERM)
        );
        return;
    }
    assert_eq!(fs::read(&public).unwrap(), b"public");
    assert_eq!(
        fs::write(&public, b"bad").unwrap_err().raw_os_error(),
        Some(libc::EPERM)
    );
    for path in [
        "home/.ssh/key",
        "brew/opt/private-file",
        "brew/opt/unpackaged",
        "brew/Cellar/fixture/1/lib/escape",
        "brew/etc/config",
        "brew/var/state",
    ] {
        assert_eq!(
            fs::read(base.join(path)).unwrap_err().raw_os_error(),
            Some(libc::EPERM),
            "{path}"
        );
    }
    println!("homebrew-ready");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    assert_eq!(input, "continue\n");
    for path in ["brew/opt/retarget-directory/key", "brew/opt/retarget-file"] {
        assert_eq!(
            fs::read(base.join(path)).unwrap_err().raw_os_error(),
            Some(libc::EPERM),
            "{path}"
        );
    }
}
