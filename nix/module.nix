# NixOS module: the todo-tui daemon as a system service (ADR-0008), so the
# phone app and the laptops' `todo` reach it over the tailnet. The daemon
# keeps everything under one home (its config.toml, database, backups,
# logs and socket), runs as its own user, and serves its remote listener
# on loopback; `tailscale serve` puts HTTPS in front of it.
#
#   services.todo-tui = {
#     enable = true;
#     tailscaleServe = true;   # https://<machine>.<tailnet>.ts.net
#   };
#
# Then pair each device on the server: `sudo todo-tui-admin remote pair
# --name laptop` (the app scans its QR code; a laptop puts the token in
# its keychain and `[client] remote` in its config).
#
# Secrets (a Jira token) come from sops-nix, agenix or any root-owned file
# through systemd credentials, never the Nix store:
#
#   sops.secrets.jira-pat = { };            # root:root 0400, the default
#   services.todo-tui = {
#     credentials.jira = config.sops.secrets.jira-pat.path;
#     settings.instances = [{ ...; credential = "systemd:jira"; }];
#   };
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.todo-tui;
  toml = pkgs.formats.toml { };
  home = cfg.dataDir;
  settings = lib.recursiveUpdate {
    remote.listen = "127.0.0.1:${toString cfg.port}";
  } cfg.settings;
  configFile = toml.generate "todo-tui-config.toml" settings;
  # The CLI as the service's user, on the service's home: `remote pair`,
  # `status`, `export`, `backup now` and the rest, on this machine. It never
  # starts a daemon of its own (TODO_NO_SPAWN), which would run outside the
  # service; the daemon's own commands go through systemd.
  systemctl = lib.getExe' config.systemd.package "systemctl";
  admin = pkgs.writeShellScriptBin "todo-tui-admin" ''
    todo() {
      ${lib.getExe' pkgs.util-linux "runuser"} -u ${cfg.user} -- \
        env TODO_TUI_HOME=${home} TODO_NO_SPAWN=1 ${lib.getExe cfg.package} "$@"
    }
    case "''${1-}" in
      daemon)
        case "''${2-}" in
          stop) exec ${systemctl} stop todo-tui.service ;;
          *)
            echo "todo-tui-admin: the daemon is todo-tui.service: systemctl start|stop|restart|status todo-tui" >&2
            exit 2
            ;;
        esac
        ;;
      restore)
        # The service stopped for the swap, and started again after it.
        ${systemctl} stop todo-tui.service || exit
        todo "$@"
        status=$?
        ${systemctl} start todo-tui.service
        exit $status
        ;;
    esac
    todo "$@"
  '';
  socket = "${toString cfg.dataDir}/run/daemon.sock";
in
{
  options.services.todo-tui = {
    enable = lib.mkEnableOption "the todo-tui daemon, reachable by the phone app and remote clients";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "todo-tui.packages.\${system}.default";
      description = "The todo-tui package (its `todo` binary runs the daemon).";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "todo-tui";
      description = "The user the daemon runs as; created when it is the default.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "todo-tui";
      description = "The daemon's group.";
    };

    dataDir = lib.mkOption {
      type = lib.types.path;
      default = "/var/lib/todo-tui";
      description = ''
        The daemon's home (its TODO_TUI_HOME): config.toml, data/ (the
        database, backups, paired devices), logs/ and run/ (the socket).
      '';
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 7743;
      description = "The remote listener's port, on 127.0.0.1.";
    };

    settings = lib.mkOption {
      inherit (toml) type;
      default = { };
      example = lib.literalExpression ''
        {
          ui.archive_done_after_days = 14;
          backup.retain_days = 30;
          instances = [{
            id = "work-jira";
            kind = "jira-dc";
            base_url = "https://jira.example.com";
            credential = "env:TODO_JIRA_PAT";
          }];
        }
      '';
      description = ''
        config.toml (design 8.2), as Nix. `remote.listen` defaults to the
        loopback port above. Secrets go in `environmentFile`, named by a
        credential such as `env:TODO_JIRA_PAT`, never here: this ends up in
        the Nix store.
      '';
    };

    credentials = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      example = lib.literalExpression ''{ jira = config.sops.secrets.jira-pat.path; }'';
      description = ''
        Secrets the daemon reads as `systemd:NAME` in `settings`, by name:
        each a path to a file (a sops-nix or agenix secret, owned by root,
        mode 0400), passed with systemd's `LoadCredential=`, so the service
        reads its own copy and the file needs no change of owner. Read at
        start; a changed secret takes a restart (sops-nix's `restartUnits`).
      '';
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      example = "/run/secrets/todo-tui.env";
      description = ''
        A file of `NAME=value` lines for the daemon's environment, read at
        start: the secrets `settings` names with `env:NAME`. Prefer
        `credentials`, which keeps a secret out of the environment.
      '';
    };

    tailscaleServe = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Run `tailscale serve --https=<httpsPort>` in front of the listener,
        so it is https://<machine>.<tailnet>.ts.net on the tailnet only. Needs
        services.tailscale and HTTPS certificates on in the tailnet.
      '';
    };

    timeZone = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "America/Los_Angeles";
      description = ''
        The daemon's time zone (its TZ), when not the system's
        (`time.timeZone`, UTC when unset). The daemon resolves relative
        dates (`^tom`, `^fri`) and "today" (Today, overdue, L48) in it,
        for every device, the phone's captures included: make it yours.
      '';
    };

    httpsPort = lib.mkOption {
      type = lib.types.port;
      default = 443;
      description = "The tailnet HTTPS port `tailscale serve` takes.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = lib.all (n: builtins.match "[A-Za-z0-9_.-]+" n != null && n != "." && n != "..") (
          lib.attrNames cfg.credentials
        );
        message = "services.todo-tui.credentials: a name is letters, digits, _ - and .";
      }
      {
        assertion = !cfg.tailscaleServe || config.services.tailscale.enable;
        message = "services.todo-tui.tailscaleServe needs services.tailscale.enable";
      }
      {
        # Longer, and the daemon would put its socket under /tmp, which the
        # service's PrivateTmp hides from todo-tui-admin.
        assertion = lib.stringLength socket <= 100;
        message = "services.todo-tui.dataDir is too long: ${socket} must be at most 100 bytes";
      }
    ];

    users.users = lib.mkIf (cfg.user == "todo-tui") {
      todo-tui = {
        isSystemUser = true;
        inherit (cfg) group;
        home = cfg.dataDir;
        description = "todo-tui daemon";
      };
    };
    users.groups = lib.mkIf (cfg.group == "todo-tui") { todo-tui = { }; };

    environment.systemPackages = [ admin ];

    systemd.tmpfiles.settings."10-todo-tui" = {
      ${home}.d = {
        inherit (cfg) user group;
        mode = "0700";
      };
      # The daemon reads its config from its home; Nix owns this copy.
      "${home}/config.toml"."L+".argument = toString configFile;
    };

    systemd.services.todo-tui = {
      description = "todo-tui daemon";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      restartTriggers = [ configFile ];
      environment = {
        TODO_TUI_HOME = home;
        # The service starts the daemon, and a `todo daemon` that finds
        # another holding the lock fails rather than look like this one.
        TODO_NO_SPAWN = "1";
      }
      // lib.optionalAttrs (cfg.timeZone != null) {
        TZ = cfg.timeZone;
        TZDIR = "${pkgs.tzdata}/share/zoneinfo";
      };
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} daemon";
        # SIGHUP reloads config.toml (design 8.2).
        ExecReload = "${pkgs.coreutils}/bin/kill -HUP $MAINPID";
        User = cfg.user;
        Group = cfg.group;
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;
        LoadCredential = lib.mapAttrsToList (name: path: "${name}:${path}") cfg.credentials;
        # Always: a Shutdown RPC exits 0, and the phone needs it back.
        Restart = "always";
        RestartSec = 5;
        UMask = "0077";
        # Hardening: it needs its home, loopback and outbound HTTPS (Jira).
        ReadWritePaths = [ home ];
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        NoNewPrivileges = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictAddressFamilies = [
          "AF_UNIX"
          "AF_INET"
          "AF_INET6"
        ];
        RestrictNamespaces = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        RestrictRealtime = true;
        SystemCallArchitectures = "native";
        CapabilityBoundingSet = "";
      };
    };

    # `serve --bg` keeps its config in tailscaled across reboots: this sets
    # it once tailscale is logged in, and retries until it is. Turning
    # tailscaleServe off leaves it set (`tailscale serve --https=443 off`).
    systemd.services.todo-tui-tailscale-serve = lib.mkIf cfg.tailscaleServe {
      description = "tailscale serve in front of the todo-tui listener";
      wantedBy = [ "multi-user.target" ];
      after = [
        "tailscaled.service"
        "todo-tui.service"
        "network-online.target"
      ]
      ++ lib.optional (config.services.tailscale.authKeyFile != null) "tailscaled-autoconnect.service";
      wants = [
        "tailscaled.service"
        "network-online.target"
      ];
      # Retries forever, every 10s: logging in can come much later.
      startLimitIntervalSec = 0;
      path = [
        config.services.tailscale.package
        pkgs.jq
      ];
      script = ''
        # Logged in, with the machine's name (and so its certificate).
        until tailscale status --json | jq -e '.BackendState == "Running" and .Self.DNSName != ""' >/dev/null; do
          sleep 2
        done
        tailscale serve --bg --https=${toString cfg.httpsPort} http://127.0.0.1:${toString cfg.port} </dev/null
      '';
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        Restart = "on-failure";
        RestartSec = 10;
        # Waiting for a login, or for HTTPS to be turned on in the tailnet
        # (serve then prints a link and waits): a retry later.
        TimeoutStartSec = 120;
      };
    };
  };
}
