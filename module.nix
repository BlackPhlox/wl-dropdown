{ config, lib, pkgs, ... }:

let
  cfg = config.programs.wl-dropdown;
in
{
  options.programs.wl-dropdown = {
    enable = lib.mkEnableOption "wl-dropdown, a dropdown terminal for Wayland";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./default.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./default.nix { }";
      description = "The wl-dropdown package to use.";
    };

    settings = lib.mkOption {
      type = with lib.types; attrsOf (oneOf [ bool int float str ]);
      default = { };
      example = lib.literalExpression ''
        {
          widthPercent = 0.6;
          heightPercent = 0.3;
          fontFamily = "Hack";
          fontSize = 10.5;
          foreground = "#ebebeb";
          background = "#000000";
          backgroundOpacity = 0.92;
          animationMs = 180;
        }
      '';
      description = ''
        Baseline settings, written to
        {file}`$XDG_CONFIG_HOME/wl-dropdown/defaults.conf`.

        This is the *baseline*, not the live configuration. wl-dropdown
        resolves each value as user override, then theme, then this file, then
        its built-in default, and only ever writes {file}`wl-dropdown.conf`
        itself. So these values act as your declared starting point while the
        settings window stays usable, and its "Reset" returns the terminal to
        exactly what is declared here.

        Writing {file}`wl-dropdown.conf` from Nix instead would make it a
        read-only symlink into the store and the settings window could never
        save.

        Recognised keys, with their built-in defaults:

        - `widthPercent` (0.6) and `heightPercent` (0.3) — fractions of the
          usable area, clamped to 0.2–1.0 and 0.15–1.0
        - `fontFamily` ("Hack") and `fontSize` (10.5)
        - `foreground` ("#ebebeb") and `background` ("#000000")
        - `backgroundOpacity` (0.92)
        - `color0` … `color15` (libvterm's) — the ANSI palette, 8–15 bright
        - `colorScheme` ("") — a Konsole/Yakuake `.colorscheme`, by name or
          path, e.g. `"''${./ayu.colorscheme}"`
        - `cornerRadius` (8) — the two free, lower corners
        - `animationMs` (180) — roll-down duration
        - `shellProgram` ("") — empty uses the shell from passwd
        - `previousTabKeys` ("Shift+Left, Ctrl+Shift+Tab"), `nextTabKeys`
          ("Shift+Right, Ctrl+Tab"), `moveTabLeftKeys` ("Ctrl+Shift+Left")
          and `moveTabRightKeys` ("Ctrl+Shift+Right") — comma-separated key
          combinations; empty unbinds the action
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."wl-dropdown/defaults.conf" = lib.mkIf (cfg.settings != { }) {
      text = lib.generators.toINI { } { General = cfg.settings; };
    };
  };
}
