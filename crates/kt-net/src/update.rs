//! Update checks (DESIGN §12): a static manifest in the public repo, read from
//! apps.jurisgames.com first and from the same file on raw.githubusercontent.com if that
//! fails. Never the rate-limited GitHub API.
//!
//! The manifest is untrusted input: size-capped, strictly parsed, and its links are only
//! passed on when they point at our own store listing or release page (the notice can't be
//! turned into a link to anywhere else, §1).

use serde::Deserialize;

/// Where the manifest lives, in the order tried.
pub const MANIFEST_URLS: [&str; 2] = ["https://apps.jurisgames.com/update.json", "https://raw.githubusercontent.com/kleiders3010/jurisgames/main/update.json"];

const MAX_BYTES: usize = 64 * 1024;

/// Links an update notice may open.
const ALLOWED_LINKS: [&str; 3] = [
    "https://play.google.com/store/apps/details?id=com.jurisgames.",
    "https://github.com/kleiders3010/jurisgames/releases",
    "https://apps.jurisgames.com/",
];

#[derive(Deserialize)]
struct Manifest {
    format: u32,
    apps: std::collections::HashMap<String, AppEntry>,
}

#[derive(Deserialize)]
struct AppEntry {
    version_name: String,
    version_code: u64,
    #[serde(default)]
    android: Links,
    #[serde(default)]
    desktop: Links,
}

#[derive(Deserialize, Default)]
struct Links {
    play: Option<String>,
    apk: Option<String>,
    windows: Option<String>,
    linux: Option<String>,
}

/// A newer version than the one running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub version_name: String,
    pub version_code: u64,
    /// Where "Update" goes on this platform (store listing or release page).
    pub link: String,
}

/// Which link an update notice should open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Play,
    Apk,
    Windows,
    Linux,
}

fn allowed(url: &str) -> bool {
    ALLOWED_LINKS.iter().any(|p| url.starts_with(p)) && !url.contains(['"', '\'', '<', '>', ' ', '\\'])
}

/// Read a manifest (untrusted bytes): the update for `app` if it's newer than `current`.
pub fn parse(bytes: &[u8], app: &str, current: u64, channel: Channel) -> Option<Update> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let m: Manifest = serde_json::from_slice(bytes).ok()?;
    if m.format != 1 {
        return None;
    }
    let e = m.apps.get(app)?;
    if e.version_code <= current || e.version_name.len() > 64 {
        return None;
    }
    let link = match channel {
        Channel::Play => e.android.play.clone(),
        Channel::Apk => e.android.apk.clone(),
        Channel::Windows => e.desktop.windows.clone(),
        Channel::Linux => e.desktop.linux.clone(),
    }?;
    allowed(&link).then(|| Update { version_name: e.version_name.clone(), version_code: e.version_code, link })
}

/// Check `urls` in order; the first manifest that reads wins (a newer version or not).
pub async fn check(urls: &[&str], app: &str, current: u64, channel: Channel) -> Option<Update> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = crate::http::client(std::time::Duration::from_secs(15))?;
    for u in urls {
        let Ok(resp) = client.get(*u).send().await else { continue };
        if !resp.status().is_success() || resp.content_length().is_some_and(|l| l as usize > MAX_BYTES) {
            continue;
        }
        let Ok(b) = resp.bytes().await else { continue };
        if serde_json::from_slice::<Manifest>(&b).is_err() {
            continue; // unreadable here: try the next source
        }
        return parse(&b, app, current, channel);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: &str = r#"{"format":1,"apps":{"tasks":{"version_name":"0.2.0","version_code":2,
        "android":{"play":"https://play.google.com/store/apps/details?id=com.jurisgames.tasks","apk":"https://github.com/kleiders3010/jurisgames/releases"},
        "desktop":{"windows":"https://github.com/kleiders3010/jurisgames/releases","linux":"https://evil.example/x"}}}}"#;

    #[test]
    fn newer_versions_with_our_own_links_only() {
        let u = parse(M.as_bytes(), "tasks", 1, Channel::Play).unwrap();
        assert_eq!((u.version_name.as_str(), u.version_code), ("0.2.0", 2));
        assert!(u.link.starts_with("https://play.google.com/"));
        assert!(parse(M.as_bytes(), "tasks", 2, Channel::Play).is_none(), "not newer");
        assert!(parse(M.as_bytes(), "tasks", 1, Channel::Linux).is_none(), "a link elsewhere is dropped");
        assert!(parse(M.as_bytes(), "other", 1, Channel::Play).is_none());
        assert!(parse(b"not json", "tasks", 1, Channel::Play).is_none());
        assert!(parse(&vec![b' '; MAX_BYTES + 1], "tasks", 1, Channel::Play).is_none());
        assert!(parse(M.replace("\"format\":1", "\"format\":2").as_bytes(), "tasks", 1, Channel::Play).is_none());
    }

    /// The real published manifest (network; run with `--ignored`).
    #[tokio::test]
    #[ignore]
    async fn live_manifest() {
        let u = check(&MANIFEST_URLS, "tasks", 0, Channel::Play).await.expect("published manifest readable");
        assert!(u.link.starts_with("https://play.google.com/"));
        assert!(check(&MANIFEST_URLS[1..], "tasks", 0, Channel::Apk).await.is_some(), "GitHub fallback serves it too");
    }

    #[tokio::test]
    async fn falls_back_when_the_first_source_fails() {
        // First source: nothing listening; second: a local server with the manifest.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{M}", M.len());
                let _ = s.write_all(resp.as_bytes()).await;
            }
        });
        let dead = "http://127.0.0.1:9/update.json";
        let live = format!("http://{addr}/update.json");
        let u = check(&[dead, &live], "tasks", 1, Channel::Apk).await.unwrap();
        assert_eq!(u.link, "https://github.com/kleiders3010/jurisgames/releases");
    }
}
