# Bash 3.2, the version macOS ships as /bin/bash. Tests run the hook and
# the escaping round-trip through it, so pin it rather than rely on the
# host having it.
{
  stdenv,
  fetchurl,
  lib,
  bison,
}:
stdenv.mkDerivation {
  pname = "bash";
  version = "3.2.57";
  src = fetchurl {
    url = "mirror://gnu/bash/bash-3.2.57.tar.gz";
    hash = "sha256-P6na+F6/NQaPCQzlEoPd7rPHXrW8cLGkp8sFhov+BqQ=";
  };
  # Old K&R-style C; modern compilers reject it without these.
  env.NIX_CFLAGS_COMPILE = toString [
    "-std=gnu89"
    "-fcommon"
    "-Wno-implicit-function-declaration"
    "-Wno-implicit-int"
    "-Wno-int-conversion"
    "-Wno-incompatible-pointer-types"
    "-Wno-return-type"
    "-Wno-error"
    "-Wno-format-security"
    "-Wno-deprecated-non-prototype"
  ];
  configureFlags = [ "--without-bash-malloc" "--disable-nls" ];
  enableParallelBuilding = false;
  # The shipped y.tab.c predates the patched parse.y, so regenerate it.
  nativeBuildInputs = [ bison ];
  doCheck = false;
  postInstall = ''
    mv $out/bin/bash $out/bin/bash3
    rm -f $out/bin/bashbug
  '';
  hardeningDisable = [ "format" ];
  meta.license = lib.licenses.gpl2Plus;
}
