{ pkgs ? import <nixos-unstable> {}}:
with pkgs;

pkgs.mkShell {
  nativeBuildInputs = [ llvmPackages.clang pkg-config openssl ];
  buildInputs = [ libevdev openssl pkgconfig linuxHeaders clippy cargo ]; 

  BINDGEN_EXTRA_CLANG_ARGS = "-I${lib.getDev libevdev}/include/libevdev-1.0";
  LIBCLANG_PATH = "${lib.getLib llvmPackages.libclang}/lib";
}
