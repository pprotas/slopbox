{
  pkgs,
  e2e,
  devShell,
  nixpkgsSource,
}:
pkgs.testers.runNixOSTest {
  name = "slopbox-e2e";
  nodes.machine = {
    virtualisation = {
      memorySize = 4096;
      cores = 2;
      additionalPaths = [
        (builtins.dirOf (builtins.dirOf e2e))
        nixpkgsSource
        devShell
        # The VM reevaluates the shell; raw drvPath breaks read-only flake checks.
        devShell.inputDerivation
      ];
    };
    nix.settings = {
      experimental-features = [
        "nix-command"
        "flakes"
      ];
      substituters = [ ];
    };
    users.users.tester.isNormalUser = true;
  };
  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("test -S /nix/var/nix/daemon-socket/socket")
    try:
        machine.succeed(${builtins.toJSON "su --login tester --command '${e2e} > /home/tester/e2e.log 2>&1'"}, timeout=300)
    finally:
        e2e_output = machine.succeed("cat /home/tester/e2e.log")
        print(e2e_output)
    assert "e2e: contained project closure" in e2e_output
    assert "e2e: all checks passed" in e2e_output
  '';
}
