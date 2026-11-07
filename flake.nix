{
  description = "wl-dropdown — Yakuake-style dropdown terminal for Wayland, on wlr-layer-shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
  let
    forAllSystems = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
  in
  {
    homeManagerModules.default = import ./module.nix;

    packages = forAllSystems (system: {
      default = (import nixpkgs { inherit system; }).callPackage ./default.nix { };
    });

    devShells = forAllSystems (system:
      let pkgs = import nixpkgs { inherit system; };
      in {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${system}.default ];
          packages = with pkgs; [ cargo rustc clippy rustfmt sway grim ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [ wayland libxkbcommon libGL ]);
        };
      });
  };
}
