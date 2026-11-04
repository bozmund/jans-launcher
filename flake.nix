{
  description = "jans-launcher - modpack updater and Minecraft launcher";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/83199d0d373dd3ac2b9a1996b1d0263f76ab7a4c";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      # egui (via winit/glutin) and LWJGL load these at runtime with dlopen.
      runtimeLibs = with pkgs; [
        libGL libxkbcommon wayland vulkan-loader
        xorg.libX11 xorg.libXcursor xorg.libXi xorg.libXrandr xorg.libXxf86vm
        alsa-lib libpulseaudio openal udev flite
      ];
    in {
      packages.${system}.default = pkgs.rustPlatform.buildRustPackage {
        pname = "jans-launcher";
        version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper pkgs.cmake ];
        buildInputs = [ pkgs.gtk3 ];
        doCheck = true;
        postInstall = ''
          wrapProgram $out/bin/jans-launcher \
            --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibs}
          install -Dm644 assets/icon.png $out/share/icons/hicolor/256x256/apps/jans-launcher.png
          install -Dm644 packaging/jans-launcher.desktop $out/share/applications/jans-launcher.desktop
        '';
      };
      devShells.${system}.default = pkgs.mkShell {
        nativeBuildInputs = [ pkgs.cargo pkgs.rustc pkgs.pkg-config pkgs.cmake ];
        buildInputs = [ pkgs.gtk3 ];
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
      };
    };
}
