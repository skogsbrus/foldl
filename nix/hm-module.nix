# home-manager module: installs foldl, its shell hook and its config.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.foldl;
  toml = pkgs.formats.toml { };
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
    enableBashIntegration = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Add the foldl hook to bash.";
    };
    enableZshIntegration = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Add the foldl hook to zsh.";
    };
    settings = lib.mkOption {
      type = toml.type;
      default = { };
      example = {
        verbosity = "quiet";
        trusted_prefixes = [ "~/work" ];
      };
      description = "Written to ~/.config/foldl/config.toml.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];
    programs.bash.initExtra = lib.mkIf cfg.enableBashIntegration ''
      eval "$(${lib.getExe cfg.package} hook bash)"
    '';
    programs.zsh.initContent = lib.mkIf cfg.enableZshIntegration ''
      eval "$(${lib.getExe cfg.package} hook zsh)"
    '';
    xdg.configFile."foldl/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = toml.generate "foldl-config.toml" ({ version = 1; } // cfg.settings);
    };
  };
}
