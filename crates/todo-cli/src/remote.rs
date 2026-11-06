//! `todo remote`: pair, list and revoke phone-app devices for the remote
//! listener (ADR-0008), and show how it is set up.

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt as _;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context as _, bail};
use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;
use todo_daemon::paths::Paths;
use todo_daemon::remote::{self, RemoteConfig};

/// The app's pairing link scheme.
const SCHEME: &str = "todo-tui://pair";

pub fn pair(paths: &Paths, name: &str, url: Option<String>, qr: bool) -> anyhow::Result<ExitCode> {
    let config = remote::read_config(&paths.config)?;
    let url = match url {
        Some(u) => normalise_url(&u)?,
        None => tailscale_url()
            .context("could not find this machine's Tailscale name; pass --url https://<host>")?,
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&paths.data)
        .with_context(|| format!("creating {}", paths.data.display()))?;
    let (device, token) = remote::pair(&paths.remote_tokens(), name, jiff::Timestamp::now())?;
    let link = pairing_link(&url, &token);
    println!(
        "Paired {:?} ({}).",
        device.name,
        crate::short_id(&device.id)
    );
    println!();
    if qr {
        println!("{}", render_qr(&link)?);
    }
    println!("Server: {url}");
    println!("Token:  {token}");
    println!("Link:   {link}");
    println!();
    println!("Scan the code in the app, or paste the server and token. The token is");
    println!(
        "shown only now; `todo remote revoke {}` unpairs it.",
        crate::short_id(&device.id)
    );
    if config.is_none() {
        println!();
        println!("Note: the remote listener is off. See `todo remote status`.");
    }
    Ok(ExitCode::SUCCESS)
}

pub fn list(paths: &Paths) -> anyhow::Result<ExitCode> {
    let devices = remote::list_devices(&paths.remote_tokens())?;
    if devices.is_empty() {
        println!("No paired devices. `todo remote pair --name <device>` pairs one.");
    }
    for d in devices {
        println!(
            "{}  {}  paired {}",
            crate::short_id(&d.id),
            d.name,
            d.created_at
        );
    }
    Ok(ExitCode::SUCCESS)
}

pub fn revoke(paths: &Paths, which: &str) -> anyhow::Result<ExitCode> {
    let d = remote::revoke(&paths.remote_tokens(), which)?;
    println!("Revoked {:?} ({}).", d.name, crate::short_id(&d.id));
    Ok(ExitCode::SUCCESS)
}

pub async fn status(paths: &Paths) -> anyhow::Result<ExitCode> {
    let devices = remote::list_devices(&paths.remote_tokens())?.len();
    match remote::read_config(&paths.config) {
        Ok(None) => {
            println!("Remote listener: off");
            println!();
            println!(
                "To use the phone app, add this to {}:",
                paths.config.display()
            );
            println!();
            println!("  [remote]");
            println!("  listen = \"127.0.0.1:7743\"");
            println!();
            println!("then `todo daemon stop` (the next client restarts it), and run");
            println!("  tailscale serve --bg --https=443 http://127.0.0.1:7743");
            println!("and `todo remote pair --name <device>`.");
        }
        Ok(Some(RemoteConfig { listen, .. })) => {
            let up = tokio::time::timeout(
                Duration::from_millis(500),
                tokio::net::TcpStream::connect(listen),
            )
            .await
            .is_ok_and(|r| r.is_ok());
            println!(
                "Remote listener: {listen} ({})",
                if up {
                    "listening"
                } else {
                    "not listening: restart the daemon with `todo daemon stop`, and check its log"
                }
            );
            match tailscale_url() {
                Ok(url) => println!("Tailscale name:  {url}"),
                Err(e) => println!("Tailscale name:  unknown ({e:#})"),
            }
        }
        Err(e) => println!("Remote listener: off, the config has a problem: {e:#}"),
    }
    println!("Paired devices:  {devices}");
    Ok(ExitCode::SUCCESS)
}

/// `https://<this machine>.<tailnet>.ts.net`, from `tailscale status --json`.
fn tailscale_url() -> anyhow::Result<String> {
    // The macOS app ships its CLI inside the bundle, off PATH; a `tailscale`
    // on PATH there (nixpkgs, Homebrew) only reaches an open-source
    // tailscaled, so the bundle's is tried when that one fails too.
    const MAC_APP: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
    let run = |bin: &str| -> anyhow::Result<Vec<u8>> {
        let out = std::process::Command::new(bin)
            .args(["status", "--json"])
            .output()
            .with_context(|| format!("running `{bin} status --json`"))?;
        if !out.status.success() {
            bail!(
                "`{bin} status` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    };
    let stdout = match run("tailscale") {
        Ok(out) => out,
        Err(e) if cfg!(target_os = "macos") && std::path::Path::new(MAC_APP).exists() => {
            run(MAC_APP).map_err(|mac| mac.context(format!("{e:#}")))?
        }
        Err(e) => return Err(e.context("is Tailscale installed and up?")),
    };
    let status: serde_json::Value = serde_json::from_slice(&stdout)?;
    let name = status["Self"]["DNSName"]
        .as_str()
        .map(|n| n.trim_end_matches('.'))
        .filter(|n| !n.is_empty())
        .context("`tailscale status` has no Self.DNSName")?;
    Ok(format!("https://{name}"))
}

/// An `http(s)://host[:port]` origin, without a trailing slash.
fn normalise_url(url: &str) -> anyhow::Result<String> {
    let url = url.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .context("the URL must start with https:// (or http://)")?;
    if rest.is_empty() || rest.contains(['/', '?', '#', ' ']) {
        bail!("the URL must be an origin like https://laptop.tailnet.ts.net");
    }
    Ok(url.to_owned())
}

fn pairing_link(url: &str, token: &str) -> String {
    format!("{SCHEME}?url={}&token={}", encode(url), encode(token))
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn render_qr(link: &str) -> anyhow::Result<String> {
    use std::io::IsTerminal as _;
    render_qr_for(link, std::io::stdout().is_terminal())
}

/// The code as text: dark modules as blocks in the text colour. On a
/// terminal, each line is set black on bright white, so the code reads the
/// right way round whatever the theme: phone scanners (iOS's included) do
/// not reliably read an inverted code.
fn render_qr_for(link: &str, terminal: bool) -> anyhow::Result<String> {
    let text = QrCode::new(link.as_bytes())?
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Dark)
        .light_color(Dense1x2::Light)
        .quiet_zone(true)
        .build();
    if !terminal {
        return Ok(text);
    }
    Ok(text
        .lines()
        .map(|line| format!("\x1b[30;107m{line}\x1b[0m"))
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_encoded() {
        assert_eq!(
            pairing_link("https://mac.tail1.ts.net", "todo_ab"),
            "todo-tui://pair?url=https%3A%2F%2Fmac.tail1.ts.net&token=todo_ab"
        );
    }

    #[test]
    fn urls_are_origins() {
        assert_eq!(
            normalise_url(" https://mac.tail1.ts.net/ ").unwrap(),
            "https://mac.tail1.ts.net"
        );
        assert_eq!(
            normalise_url("http://100.64.0.1:7743").unwrap(),
            "http://100.64.0.1:7743"
        );
        assert!(normalise_url("mac.tail1.ts.net").is_err());
        assert!(normalise_url("https://mac/path").is_err());
        assert!(normalise_url("https://").is_err());
    }

    #[test]
    fn qr_renders_dark_on_light() {
        let plain = render_qr_for("todo-tui://pair?url=x&token=y", false).unwrap();
        assert!(plain.lines().count() > 10);
        assert!(!plain.contains('\x1b'));
        // The quiet zone is light: spaces, not blocks.
        assert!(plain.lines().next().unwrap().chars().all(|c| c == ' '));
        let colored = render_qr_for("todo-tui://pair?url=x&token=y", true).unwrap();
        assert!(
            colored
                .lines()
                .all(|l| l.starts_with("\x1b[30;107m") && l.ends_with("\x1b[0m"))
        );
    }
}
