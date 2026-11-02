# NixOS module: installs foldl and its hook for every interactive shell.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.foldl;
in
{
  options.programs.foldl = {
    enable = lib.mkEnableOption "foldl, which runs code when a shell enters or leaves a folder";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "foldl.packages.\${system}.default";
      description = "The foldl package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    programs.bash.interactiveShellInit = ''
      eval "$(${lib.getExe cfg.package} hook bash)"
    '';
    programs.zsh.interactiveShellInit = ''
      eval "$(${lib.getExe cfg.package} hook zsh)"
    '';
  };
}
