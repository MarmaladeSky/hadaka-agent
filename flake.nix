# Copyright 2026 David Akermann
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

{
  description = "A task-focused, security-first agent runtime designed to remain simple and reviewable.";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];

      forAllSystems = function:
        nixpkgs.lib.genAttrs systems (system: function {
          pkgs = import nixpkgs { inherit system; };
        });

      mkPackage = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "hadaka-agent";
        version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            pkgs.lib.cleanSourceFilter path type
            && !(builtins.elem (builtins.baseNameOf path) [ "target" ".idea" ".agents" ".codex" ]);
        };
        cargoLock.lockFile = ./Cargo.lock;

        nativeCheckInputs = [ pkgs.cacert ];
        preCheck = ''
          export SSL_CERT_FILE="${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        '';

        meta = {
          description = "A task-focused, security-first agent runtime designed to remain simple and reviewable.";
          homepage = "https://github.com/MarmaladeSky/hadaka-agent";
          license = pkgs.lib.licenses.asl20;
          mainProgram = "hadaka-agent";
        };
      };
    in
    {
      packages = forAllSystems ({ pkgs }: let
        package = mkPackage pkgs;
      in {
        hadaka-agent = package;
        default = package;
      });

      apps = forAllSystems ({ pkgs }: let
        package = mkPackage pkgs;
      in {
        default = {
          type = "app";
          program = "${package}/bin/hadaka-agent";
          meta.description = "A task-focused, security-first agent runtime.";
        };
      });

      devShells = forAllSystems ({ pkgs }: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.clippy
            pkgs.rustc
            pkgs.rustfmt
          ];
        };
      });

      formatter = forAllSystems ({ pkgs }: pkgs.nixpkgs-fmt);
    };
}
