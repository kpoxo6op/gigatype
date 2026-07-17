{
  description = "GigaType private local Russian voice typing";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      packages = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "gigatype";
            version = "0.3.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.cmake pkgs.pkg-config ];
            buildInputs = [ pkgs.alsa-lib pkgs.openssl ];
            ORT_LIB_LOCATION = "${pkgs.onnxruntime}/lib";
            ORT_PREFER_DYNAMIC_LINK = "1";
            postInstall = ''
              install -Dm644 packaging/systemd/gigatype.service \
                $out/lib/systemd/user/gigatype.service
            '';
          };
        });
    };
}
