# A NixOS VM test of the module (Linux with KVM; the nix CI job): the
# daemon starts as its own user, a paired client reaches it through the
# remote listener, and the CLI on another home uses it as `[client] remote`.
self:
{ pkgs, ... }:
pkgs.testers.runNixOSTest {
  name = "todo-tui-module";
  nodes.server = {
    imports = [ self.nixosModules.default ];
    services.todo-tui = {
      enable = true;
      settings.backup.retain_days = 7;
      timeZone = "Pacific/Kiritimati";
      # As sops-nix leaves a secret: a root-owned 0400 file under /run.
      credentials.jira = "/run/secrets/jira-pat";
      settings.instances = [
        {
          id = "work-jira";
          kind = "jira-dc";
          base_url = "https://jira.example.com";
          credential = "systemd:jira";
        }
      ];
    };
    systemd.tmpfiles.rules = [ "d /run/secrets 0751 root root -" ];
    system.activationScripts.fakeSops.text = ''
      mkdir -p /run/secrets && printf 'pat\n' > /run/secrets/jira-pat && chmod 400 /run/secrets/jira-pat
    '';
    environment.systemPackages = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
  };
  testScript = ''
    server.wait_for_unit("todo-tui.service")
    server.wait_for_open_port(7743)
    # The daemon runs as todo-tui, its home 0700.
    server.succeed("test \"$(stat -c %U:%a /var/lib/todo-tui)\" = todo-tui:700")
    server.succeed("pgrep -u todo-tui -f 'todo daemon'")
    # It never starts one of its own: with the service stopped, it says so.
    server.succeed("systemctl stop todo-tui")
    server.fail("todo-tui-admin status")
    server.fail("pgrep -u todo-tui -f 'todo daemon'")
    server.succeed("systemctl start todo-tui")
    server.wait_for_open_port(7743)
    # The root-owned secret reached the daemon as a systemd credential: its
    # sync gets as far as the network (no Jira here), not a credential error.
    server.succeed("test \"$(stat -c %U:%a /run/secrets/jira-pat)\" = root:400")
    # (`sync status` exits 1 while an instance is not ok; the driver runs
    # with pipefail.)
    status = server.wait_until_succeeds("(todo-tui-admin sync status || true) | grep 'work-jira: offline'")
    assert "credential" not in status and "refused" not in status, status
    # Pair a client, then use the daemon from another home through the
    # listener, as a laptop would through `tailscale serve`.
    out = server.succeed("todo-tui-admin remote pair --name laptop --url http://127.0.0.1:7743 --no-qr")
    token = next(l.split(":", 1)[1].strip() for l in out.splitlines() if l.startswith("Token:"))
    server.succeed("mkdir -p /tmp/client && printf '%s' '" + token + "' > /tmp/client/t && chmod 600 /tmp/client/t")
    server.succeed("printf '[client]\\nremote = \"http://127.0.0.1:7743\"\\ncredential = \"file:/tmp/client/t\"\\n' > /tmp/client/config.toml")
    server.succeed("TODO_TUI_HOME=/tmp/client todo add 'from the laptop'")
    # Relative dates in the daemon's zone (UTC+14, a day ahead of UTC most
    # of the time): today there is the date `TZ=Pacific/Kiritimati date` says.
    out = server.succeed("TODO_TUI_HOME=/tmp/client todo add 'zoned ^today'")
    today = server.succeed("TZ=Pacific/Kiritimati date +%F").strip()
    assert "^" + today in out, out
    server.succeed("todo-tui-admin status | grep -q 'Inbox: 2'")
    server.fail("test -e /tmp/client/run/daemon.sock")
    # A reload (SIGHUP) and a restart keep it serving.
    server.succeed("systemctl reload todo-tui")
    server.succeed("systemctl restart todo-tui")
    server.wait_for_open_port(7743)
    server.succeed("TODO_TUI_HOME=/tmp/client todo status | grep -q 'Inbox: 2'")
    # A Shutdown over the socket (an upgrade restart, say) exits 0, and the
    # service comes back.
    pid = server.succeed("systemctl show -p MainPID --value todo-tui").strip()
    server.succeed("runuser -u todo-tui -- env TODO_TUI_HOME=/var/lib/todo-tui todo daemon stop")
    server.wait_until_succeeds("p=$(systemctl show -p MainPID --value todo-tui); [ \"$p\" != 0 ] && [ \"$p\" != " + pid + " ]")
    server.wait_for_open_port(7743)
    server.succeed("todo-tui-admin daemon stop")
    server.fail("systemctl is-active todo-tui")
  '';
}
