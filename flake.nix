{
    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
        flake-utils.url = "github:numtide/flake-utils";
    };
    outputs = { self, nixpkgs, flake-utils }: let
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        package = pkgs: pkgs.rustPlatform.buildRustPackage {
            inherit (cargoToml.package) name version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            cargoLock.outputHashes = {
                "homie5-0.7.0" = "sha256-4uSlLk+pbvTtp7CIx6N39a9dFpWD93Vu5r1NsMuR90U=";
            };
            doCheck = false;
        };
        flakeForSystem = system: let
            pkgs = nixpkgs.legacyPackages.${system};
        in {
            devShell = with pkgs; mkShell {
                RUSTFLAGS = "-Clink-arg=-fuse-ld=mold";
                packages = [
                    git
                    mold-wrapped
                    rustup
                ];
            };
            packages.default = package pkgs;
        };
    in (flake-utils.lib.eachDefaultSystem (system: flakeForSystem system)) // rec {
        overlay = final: prev: { automower2mqtt = package final; };
        nixosModules.default = { config, lib, pkgs, utils, ... }:
        let
            cfg = config.services.automower2mqtt;
        in with lib; {
            options.services.automower2mqtt = {
                enable = mkEnableOption "Enable the automower2mqtt proxy service.";
                package = mkOption {
                    description = "The automower2mqtt package to use";
                    type = types.package;
                    default = pkgs.automower2mqtt;
                };
                flags = mkOption {
                    description = ''automower2mqtt CLI flags to pass into the service.'';
                    default = { };
                    type = types.listOf types.str;
                };
            };

            config = mkIf cfg.enable {
                nixpkgs.overlays = [ overlay ];
                systemd.services.automower2mqtt = {
                    description = "automower2mqtt proxy";
                    wants = [ "network.target" ];
                    wantedBy = [ "multi-user.target" ];
                    unitConfig.StartLimitIntervalSec = "0s";
                    serviceConfig = {
                        Restart = "always";
                        RestartSec = 5;
                        ExecStart = utils.escapeSystemdExecArgs ([
                            "${cfg.package}/bin/automower2mqtt"
                        ] ++ cfg.flags);
                    };
                };
            };
        };

    };
}
