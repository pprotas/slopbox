{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      pkgs = nixpkgs.legacyPackages.aarch64-darwin;
    in
    {
      devShells.aarch64-darwin.default = pkgs.mkShell {
        packages = [
          pkgs.bash
          pkgs.cargo
          pkgs.rustc
        ];
        buildInputs = [ pkgs.zlib ];
        shellHook = ''
          if [ -n "''${SLOPBOX_NATIVE_SOCKET:-}" ]; then
            echo "shellHook received harness authority" >&2
            exit 90
          fi
          if (read -r canary < "$PWD/../outside/canary") 2>/dev/null; then
            echo "shellHook ran outside the tool sandbox" >&2
            exit 91
          fi
          export SLOPBOX_FIXTURE_NIX_ACTIVE=1
          printf '%s\n' "''${BASH_SOURCE[0]}" > activation-path
          printf 'hook\n' >> hook-calls
        '';
      };
    };
}
