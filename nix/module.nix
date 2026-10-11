self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.syzd;
  package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
in
{
  options.services.syzd = {
    enable = lib.mkEnableOption "syzd, the syz dependency tracking server";

    environmentFile = lib.mkOption {
      type = lib.types.path;
      example = "/run/secrets/syzd.env";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.syzd = rec {
      description = "syz dependency tracking server";

      wants = [ "network-online.target" ];
      after = wants;

      wantedBy = [ "multi-user.target" ];

      environment.HOME = "/var/lib/syz";

      serviceConfig = {
        ExecStart = lib.getExe' package "syzd";
        EnvironmentFile = cfg.environmentFile;
        Restart = "on-failure";

        DynamicUser = true;
        StateDirectory = "syz";
        WorkingDirectory = "/var/lib/syz";
      };
    };
  };
}
