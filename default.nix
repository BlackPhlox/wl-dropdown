{ lib, rustPlatform, pkg-config, makeWrapper, libxkbcommon, wayland, libGL }:

rustPlatform.buildRustPackage {
  pname = "wl-dropdown";
  version = "0.1.0";

  src = lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [ pkg-config makeWrapper ];
  buildInputs = [ libxkbcommon ];

  # The screenshot and pty tests need the bundled fonts and a shell, both of
  # which the sandbox has; nothing needs a compositor.
  doCheck = true;

  postInstall = ''
    install -Dm644 share/templates/wl-dropdown.conf \
      $out/share/wl-dropdown/templates/wl-dropdown.conf
  '';

  # The settings window loads Wayland, xkbcommon and GL at run time.
  postFixup = ''
    wrapProgram $out/bin/wl-dropdown \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath [ wayland libxkbcommon libGL ]}
  '';

  meta = {
    description = "Yakuake-style dropdown terminal for Wayland, on wlr-layer-shell";
    homepage = "https://github.com/BlackPhlox/wl-dropdown";
    license = lib.licenses.mit;
    mainProgram = "wl-dropdown";
    platforms = lib.platforms.linux;
  };
}
