{ pkgs ? import <nixpkgs> { } }: with pkgs; mkShell {
  buildInputs = [
    pkg-config
    openssl
    vlc
    wayland
    xorg.libX11
    libGL
    libxkbcommon
    wayland
    xorg.libX11
    xorg.libXcursor
    xorg.libXi
    xorg.libXrandr
  ];
  LD_LIBRARY_PATH = lib.makeLibraryPath [
    libGL
    libxkbcommon
    wayland
    xorg.libX11
    xorg.libXcursor
    xorg.libXi
    xorg.libXrandr
  ];
}
