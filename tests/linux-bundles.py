"""Exercise read-only application bundles without Nix or third-party Python packages."""

import json
import os
import shutil
import socket
import subprocess
import sys
import sysconfig
import tempfile
from pathlib import Path

assert not Path("/nix").exists()
with tempfile.TemporaryDirectory(
    prefix="slopbox-bundles-", dir=Path.home()
) as directory:
    root = Path(directory)
    home = root / "home"
    workspace = root / "workspace"
    installation = root / "different-installation"
    bundle = installation / "app"
    package = bundle / "lib/example_plugin"
    libraries = installation / "lib"
    for path in [
        home / ".config/slopbox",
        home / ".ssh",
        workspace,
        package,
        libraries,
    ]:
        path.mkdir(parents=True)
    binary = root / "slopbox"
    shutil.copyfile(Path(sys.argv[1]).resolve(), binary)
    binary.chmod(0o755)
    config = home / ".config/slopbox/config.toml"
    env = {"PATH": os.environ["PATH"], "HOME": str(home)}

    def run(*args, success=True):
        result = subprocess.run(
            [str(arg) for arg in args],
            env=env,
            cwd=workspace,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        log = result.stdout + result.stderr
        assert (result.returncode == 0) == success, log
        return log

    (root / "dependency.c").write_text("int answer(void) { return 42; }\n")
    (root / "plugin.c").write_text(
        "int answer(void); int plugin(void) { return answer(); }\n"
    )
    run(
        "cc",
        "-shared",
        "-fPIC",
        root / "dependency.c",
        "-o",
        libraries / "libbundle-dep.so",
    )
    run(
        "cc",
        "-shared",
        "-fPIC",
        root / "plugin.c",
        f"-L{libraries}",
        "-lbundle-dep",
        "-Wl,-rpath,$ORIGIN/../../../lib",
        "-o",
        package / "native.so",
    )
    context = bundle / "caller-libraries"
    context.mkdir()
    run("cc", "-shared", "-fPIC", root / "dependency.c", "-o", context / "libcaller.so")
    run(
        "cc",
        "-shared",
        "-fPIC",
        root / "plugin.c",
        f"-L{context}",
        "-lcaller",
        "-o",
        bundle / "caller-plugin.so",
    )
    (root / "caller.c").write_text("""#include <dlfcn.h>
#include <stdio.h>
int main(int argc, char **argv) {
    if (argc != 2) return 1;
    void *module = dlopen(argv[1], RTLD_NOW);
    if (!module) { puts(dlerror()); return 2; }
    int (*plugin)(void) = dlsym(module, "plugin");
    if (!plugin) return 3;
    printf("%d\\n", plugin());
    return 0;
}
""")
    run(
        "cc",
        root / "caller.c",
        "-ldl",
        "-Wl,--disable-new-dtags,-rpath,$ORIGIN/caller-libraries",
        "-o",
        bundle / "caller",
    )
    (libraries / "unrelated-data").write_text("not selected by dependency discovery")
    (package / "__init__.py").write_text(
        "from importlib.resources import files\ndef value(): return files(__package__).joinpath('answer.txt').read_text()\n"
    )
    (package / "answer.txt").write_text("bundle-data")
    (bundle / "asset-alias").symlink_to("lib/example_plugin/answer.txt")
    info = bundle / "lib/example_plugin-1.0.dist-info"
    info.mkdir()
    (info / "METADATA").write_text("Name: example_plugin\nVersion: 1.0\n")
    (info / "entry_points.txt").write_text(
        "[slopbox.fixture]\nexample = example_plugin:value\n"
    )
    helper = bundle / "helper"
    helper.write_text("#!/bin/sh\nprintf 'bundle-subprocess'\n")
    helper.chmod(0o755)
    alias = root / "current"
    alias.symlink_to(bundle, target_is_directory=True)
    (home / ".ssh/key").write_text("host-only")
    stdlib = Path(sysconfig.get_path("stdlib")).resolve()
    python_bundles = [stdlib]
    customization = Path("/etc") / stdlib.name
    if customization.is_dir():
        python_bundles.append(customization)
    base = f"""[policy]
network = "none"
credentials = "none"
harness = "none"
[runtime]
executables = [{json.dumps(sys.executable)}, "findmnt"]
dependency_roots = [{json.dumps(str(libraries))}]
"""

    def configure(bundles):
        config.write_text(
            base + "bundles = " + json.dumps([str(path) for path in bundles]) + "\n"
        )

    launch = [binary, "run", "--dev-env", "none", "--"]
    configure([*python_bundles, alias])
    status = run(binary, "status", "--verbose")
    assert f"runtime-bundle: {alias} (whole tree, read-only code and data)" in status
    assert not (home / ".local").exists(), "status created state"
    assert "0 failed check(s)" in run(binary, "doctor")
    assert not (home / ".local").exists(), "doctor created state"
    probe = workspace / "probe.py"
    probe.write_text(f"""
import ctypes, importlib.metadata, os, socket, subprocess
from pathlib import Path
import example_plugin
assert example_plugin.value() == 'bundle-data'
entry, = importlib.metadata.entry_points(group='slopbox.fixture')
assert entry.load()() == 'bundle-data'
bundle = Path({str(alias)!r})
assert ctypes.CDLL(str(bundle / 'lib/example_plugin/native.so')).plugin() == 42
assert subprocess.check_output([str(bundle / 'caller'), str(bundle / 'caller-plugin.so')], text=True) == '42\\n'
assert (bundle / 'asset-alias').read_text() == 'bundle-data'
assert subprocess.check_output([str(bundle / 'helper')], text=True) == 'bundle-subprocess'
for path in [bundle / 'new-file', bundle / 'asset-alias', bundle / 'lib/example_plugin/answer.txt']:
    try: path.write_text('changed')
    except OSError: pass
    else: raise AssertionError(f'writable runtime: {{path}}')
try: (bundle / 'helper').unlink()
except OSError: pass
else: raise AssertionError('writable runtime directory')
for path in [{str(home / ".ssh/key")!r}, {str(config)!r}, {str(libraries / "unrelated-data")!r}, '/usr/bin/cc', '/usr/bin/curl']:
    assert not Path(path).exists(), path
assert 'ro' in subprocess.check_output(['findmnt', '-n', '-o', 'VFS-OPTIONS', '-T', str(bundle)], text=True).strip().split(',')
try: socket.create_connection(('1.1.1.1', 80), timeout=1)
except OSError: pass
else: raise AssertionError('direct network available')
assert not os.environ.get('SSH_AUTH_SOCK')
print('bundle-isolation-passed')
""")
    command = ["env", f"PYTHONPATH={alias}/lib", "python3", probe]
    assert "bundle-isolation-passed" in run(*launch, *command)
    assert "bundle-isolation-passed" in run(
        *launch, "slopbox", "tool-run", "--network", "none", "--", *command
    )
    assert (package / "answer.txt").read_text() == "bundle-data"
    assert not list(package.glob("__pycache__/*")), "guest modified the host bundle"
    configure(python_bundles)
    assert "No module named 'example_plugin'" in run(*launch, *command, success=False)
    configure([*python_bundles, alias])

    other = root / "other-bundle"
    other.mkdir()
    (workspace / "secret").write_text("workspace-code")
    (bundle / "workspace").mkdir()
    (bundle / "workspace/secret").write_text("decoy")
    (bundle / "directory-alias").symlink_to(other)
    escape = bundle / "escape"
    escape.symlink_to("directory-alias/../workspace/secret")
    configure([*python_bundles, alias, other])
    assert "overlaps workspace" in run(
        *launch, "/bin/sh", "-c", "exit 0", success=False
    )
    escape.unlink()
    (bundle / "directory-alias").unlink()
    configure([*python_bundles, alias])
    for target, message in [
        (libraries / "unrelated-data", "escapes selected resources"),
        (home / ".ssh/key", "credentials"),
        (workspace, "overlaps workspace"),
    ]:
        escape.symlink_to(target)
        assert message in run(*launch, "/bin/sh", "-c", "exit 0", success=False)
        escape.unlink()
    escape.symlink_to("asset-alias")
    assert "bundle-isolation-passed" in run(*launch, *command)
    escape.unlink()
    os.link(package / "answer.txt", workspace / "hard-link")
    assert "mutable hard-link aliases" in run(
        *launch, "/bin/sh", "-c", "exit 0", success=False
    )
    (workspace / "hard-link").unlink()
    with socket.socket(socket.AF_UNIX) as listener:
        listener.bind(str(bundle / "service.sock"))
        assert "special file" in run(*launch, "/bin/sh", "-c", "exit 0", success=False)
    (bundle / "service.sock").unlink()
    os.mkfifo(bundle / "fifo")
    assert "special file" in run(*launch, "/bin/sh", "-c", "exit 0", success=False)
    (bundle / "fifo").unlink()
    for path in [
        home,
        root,
        workspace,
        home / ".ssh",
        "/usr",
        "/usr/lib",
        "/usr/share",
        "/tmp",
    ]:
        configure([path])
        error = run(*launch, "/bin/sh", "-c", "exit 0", success=False)
        assert "runtime" in error or "application bundle" in error, error
    configure([*python_bundles, alias])
    (workspace / ".slopbox.toml").write_text(
        f"[runtime]\nbundles = [{json.dumps(str(root))}]\n"
    )
    assert "unknown field" in run(binary, "status", success=False)
    print("PASS: application bundle plugins, data, subprocesses, aliases and isolation")
