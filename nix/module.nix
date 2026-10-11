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
    environment.systemPackages = [ package ];

    users.users.syz = {
      isSystemUser = true;
      group = "syz";
    };
    users.groups.syz = { };

    systemd.tmpfiles.rules = [ "D! /var/cache/syz 0750 syz syz -" ];

    systemd.services.syzd = rec {
      description = "syz dependency tracking server";

      wants = [ "network-online.target" ];
      after = wants;

      wantedBy = [ "multi-user.target" ];

      environment.HOME = "/var/cache/syz";

      serviceConfig = {
        ExecStart = lib.getExe' package "syzd";
        EnvironmentFile = cfg.environmentFile;
        Restart = "on-failure";

        User = "syz";
        Group = "syz";
        StateDirectory = "syz";
        StateDirectoryMode = "0750";
        CacheDirectory = "syz";
        CacheDirectoryMode = "0750";
        WorkingDirectory = "/var/cache/syz";
      };
    };
  };
}
