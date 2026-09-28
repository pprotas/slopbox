"""Native bundle resources and literal Mach-O dependency grants."""

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

slopbox, node = (Path(value).resolve() for value in sys.argv[1:3])
with tempfile.TemporaryDirectory(
    prefix="slopbox-bundles-", dir="/private/var/tmp"
) as temporary:
    root = Path(temporary)
    home, work, app, external = (
        root / name for name in ("home", "work", "app", "external")
    )
    for path in (
        home,
        work,
        app / "bin",
        app / "plugins",
        external / "bin",
        external / "v2/lib",
    ):
        path.mkdir(parents=True, exist_ok=True)
    (external / "current").symlink_to("v2/lib")
    (home / ".ssh").mkdir()
    (home / ".ssh/key").write_text("host credential canary")
    (external / "v2/lib/unrelated").write_text("dependency root canary")
    (app / "data.json").write_text('{"value": 42}')
    compiler_environment = {"PATH": "/usr/bin:/bin", "HOME": str(home)}

    def compile_source(name, source, arguments):
        path = root / f"{name}.c"
        path.write_text(source)
        subprocess.run(
            ["/usr/bin/clang", str(path), *map(str, arguments)],
            env=compiler_environment,
            check=True,
            capture_output=True,
            timeout=30,
        )

    library = external / "v2/lib/libvalue.2.1.dylib"
    compile_source(
        "value",
        "int value(void) { return 42; }",
        [
            "-dynamiclib",
            "-install_name",
            external / "current/libvalue.2.dylib",
            "-o",
            library,
        ],
    )
    (library.parent / "libvalue.2.dylib").symlink_to(library.name)
    plugin = app / "plugins/value.bundle"
    compile_source(
        "plugin",
        "extern int value(void); int plugin(void) { return value(); }",
        ["-bundle", library, "-o", plugin],
    )
    helper = app / "bin/helper"
    compile_source(
        "helper",
        "#include <dlfcn.h>\n#include <stdio.h>\n"
        "int main(int argc, char **argv) { void *h = dlopen(argv[1], RTLD_NOW); "
        'if (!h) { fprintf(stderr, "%s\\n", dlerror()); return 1; } '
        'int (*f)(void) = dlsym(h, "plugin"); printf("%d\\n", f()); return 0; }',
        ["-o", helper],
    )
    child = library.parent / "libchild.dylib"
    compile_source(
        "child",
        "int child(void) { return 7; }",
        ["-dynamiclib", "-install_name", "@rpath/libchild.dylib", "-o", child],
    )
    parent = library.parent / "libparent.dylib"
    compile_source(
        "parent",
        "extern int child(void); int parent(void) { return child(); }",
        ["-dynamiclib", child, "-install_name", "@rpath/libparent.dylib", "-o", parent],
    )
    linked = external / "bin/linked"
    compile_source(
        "linked",
        '#include <stdio.h>\nextern int parent(void); int main(void) { printf("%d\\n", parent()); return 0; }',
        [parent, "-Wl,-rpath,@executable_path/../current", "-o", linked],
    )
    entry = app / "bin/application"
    entry.write_text(
        "#!/usr/bin/env node\n"
        "const fs = require('node:fs'); const assert = require('node:assert/strict');\n"
        "const {execFileSync} = require('node:child_process');\n"
        f"const app = {json.dumps(str(app))}; const external = {json.dumps(str(external))};\n"
        "assert.equal(JSON.parse(fs.readFileSync(app + '/data.json')).value, 42);\n"
        "assert.equal(execFileSync(app + '/bin/helper', [app + '/plugins/value.bundle'], {encoding:'utf8'}).trim(), '42');\n"
        "assert.equal(execFileSync(external + '/bin/linked', {encoding:'utf8'}).trim(), '7');\n"
        f"for (const path of [{json.dumps(str(home / '.ssh/key'))}, external + '/v2/lib/unrelated']) {{\n"
        "  assert.throws(() => fs.readFileSync(path), {code:'EPERM'});\n"
        "}\n"
        "assert.throws(() => fs.readdirSync(external + '/v2/lib'), {code:'EPERM'});\n"
        "assert.throws(() => fs.writeFileSync(app + '/data.json', 'changed'), {code:'EPERM'});\n"
        "assert.throws(() => fs.writeFileSync(external + '/current/libvalue.2.dylib', 'changed'), {code:'EPERM'});\n"
        "fs.writeFileSync('result', 'bundle and library isolation passed');\n"
        "console.log('bundle and library isolation passed');\n"
    )
    entry.chmod(0o755)
    alias = root / "current-app"
    alias.symlink_to(app)
    config = root / "config/slopbox/config.toml"
    config.parent.mkdir(parents=True)
    config.write_text(
        '[policy]\nnetwork="none"\ncredentials="none"\nharness="none"\n'
        f"[runtime]\nexecutables={json.dumps([str(node), str(alias / 'bin/application'), str(linked)])}\n"
        f"bundles={json.dumps([str(alias)])}\ndependency_roots={json.dumps([str(external)])}\n"
    )
    environment = dict(
        os.environ,
        HOME=str(home),
        XDG_CONFIG_HOME=str(root / "config"),
        XDG_DATA_HOME=str(root / "data"),
    )

    def run():
        return subprocess.run(
            [str(slopbox), "run", "--workspace", str(work), "--", "application"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
            timeout=45,
        )

    result = run()
    assert result.returncode == 0, result.stderr
    assert "bundle and library isolation passed" in result.stdout, result.stdout
    assert (work / "result").read_text() == "bundle and library isolation passed"
    assert (app / "data.json").read_text() == '{"value": 42}'
    (app / "escape").symlink_to(home / ".ssh/key")
    result = run()
    assert result.returncode != 0 and "private state" in result.stderr, result.stderr
    (app / "escape").unlink()
    os.link(home / ".ssh/key", app / "hard-link")
    result = run()
    assert result.returncode != 0 and "hard-link" in result.stderr, result.stderr
    (app / "hard-link").unlink()
    (app / "setid").write_text("not runnable")
    (app / "setid").chmod(0o4755)
    assert (app / "setid").stat().st_mode & 0o4000
    result = run()
    assert result.returncode != 0 and "setuid/setgid" in result.stderr, result.stderr
    print(
        "native bundles: resources, native plugins, inherited runpaths, library aliases, isolation and rejection passed"
    )
