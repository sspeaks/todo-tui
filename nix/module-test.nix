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
    };
    environment.systemPackages = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
  };
  testScript = ''
    server.wait_for_unit("todo-tui.service")
    server.wait_for_open_port(7743)
    # The daemon runs as todo-tui, its home 0700.
    server.succeed("test \"$(stat -c %U:%a /var/lib/todo-tui)\" = todo-tui:700")
    server.succeed("pgrep -u todo-tui -f 'todo daemon'")
    # Pair a client, then use the daemon from another home through the
    # listener, as a laptop would through `tailscale serve`.
    out = server.succeed("todo-tui-admin remote pair --name laptop --url http://127.0.0.1:7743 --no-qr")
    token = next(l.split(":", 1)[1].strip() for l in out.splitlines() if l.startswith("Token:"))
    server.succeed("mkdir -p /tmp/client && printf '%s' '" + token + "' > /tmp/client/t && chmod 600 /tmp/client/t")
    server.succeed("printf '[client]\\nremote = \"http://127.0.0.1:7743\"\\ncredential = \"file:/tmp/client/t\"\\n' > /tmp/client/config.toml")
    server.succeed("TODO_TUI_HOME=/tmp/client todo add 'from the laptop'")
    server.succeed("todo-tui-admin status | grep -q 'Inbox: 1'")
    server.fail("test -e /tmp/client/run/daemon.sock")
    # A reload (SIGHUP) and a restart keep it serving.
    server.succeed("systemctl reload todo-tui")
    server.succeed("systemctl restart todo-tui")
    server.wait_for_open_port(7743)
    server.succeed("TODO_TUI_HOME=/tmp/client todo status | grep -q 'Inbox: 1'")
  '';
}
