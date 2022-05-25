with import <nixpkgs>{};

let
  pkgs = import (fetchTarball("channel:nixpkgs-unstable")) {};

in pkgs.mkShell {
  nativeBuildInputs = [ llvmPackages.clang pkg-config openssl ];
  buildInputs = [ libevdev openssl linuxHeaders ];

  BINDGEN_EXTRA_CLANG_ARGS = "-I${lib.getDev libevdev}/include/libevdev-1.0";
  LIBCLANG_PATH = "${lib.getLib llvmPackages.libclang}/lib";
}
