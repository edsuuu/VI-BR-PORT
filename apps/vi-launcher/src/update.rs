//! Keeping the launcher and the build up to date. The fork's GitHub Pages
//! site serves `latest.json`, the manifest of the newest release; when Pages
//! cannot be reached the newest GitHub release is asked for the same manifest
//! (it is one of its assets). The manifest names, for each platform, the
//! bundle's address, size and SHA-256: a bundle that does not match is never
//! unpacked. Only HTTPS addresses on GitHub's own hosts are followed.
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::paths;

/// A dev build (VI_CHANNEL=dev when it was built): its name and version say so;
/// a stable one keeps the plain name.
pub const DEV: bool = match option_env!("VI_CHANNEL") {
    Some(c) => c.len() == 3 && c.as_bytes()[0] == b'd' && c.as_bytes()[1] == b'e' && c.as_bytes()[2] == b'v',
    None => false,
};

/// The version as the window shows it: "dev 0.1.0" for a dev build.
pub fn shown_version() -> String {
    if DEV { format!("dev {}", current_version()) } else { format!("v{}", current_version()) }
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The key of this platform's bundle in the manifest.
pub fn platform() -> &'static str {
    if cfg!(windows) { "windows-x64" } else { "linux-x64" }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub format: u32,
    pub version: String,
    #[serde(default)]
    pub published: Option<String>,
    #[serde(default)]
    pub notes_url: Option<String>,
    pub assets: std::collections::BTreeMap<String, Asset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// Where the manifest is asked for, in order, and which hosts any address
/// (the manifest's, a bundle's, a redirect's) may be on.
#[derive(Debug, Clone)]
pub struct Sources {
    pub pages: String,
    pub releases_api: String,
    pub hosts: Vec<String>,
    /// Only for the tests' local server.
    pub allow_http: bool,
}

impl Sources {
    pub fn fork() -> Self {
        Self {
            pages: "https://edsuuu.github.io/VI-BR-PORT/latest.json".into(),
            releases_api: "https://api.github.com/repos/edsuuu/VI-BR-PORT/releases/latest".into(),
            hosts: [
                "edsuuu.github.io",
                "api.github.com",
                "github.com",
                "objects.githubusercontent.com",
                "release-assets.githubusercontent.com",
            ]
            .map(String::from)
            .to_vec(),
            allow_http: false,
        }
    }
}

#[derive(Debug)]
pub enum UpdateError {
    Host(String),
    Network(String),
    Manifest(String),
    NoAsset(&'static str),
    Mismatch { expected: String, got: String },
    Unpack(String),
    /// The address answered 404.
    NotFound(String),
    /// Neither Pages nor the releases have a release yet.
    NoRelease,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host(u) => write!(f, "address not on the fork's hosts: {u}"),
            Self::Network(e) => write!(f, "network: {e}"),
            Self::Manifest(e) => write!(f, "manifest: {e}"),
            Self::NoAsset(p) => write!(f, "the release has no bundle for {p}"),
            Self::Mismatch { expected, got } => write!(f, "the download does not match the manifest (sha256 {got}, expected {expected})"),
            Self::Unpack(e) => write!(f, "unpacking: {e}"),
            Self::NotFound(u) => write!(f, "not found: {u}"),
            Self::NoRelease => write!(f, "no release published yet"),
        }
    }
}

/// A newer release and where its manifest came from.
#[derive(Debug, Clone)]
pub struct Found {
    pub manifest: Manifest,
    pub source: &'static str,
}

const MANIFEST_LIMIT: u64 = 1 << 20;
const BUNDLE_LIMIT: u64 = 2 << 30;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(60 * 30)))
        .timeout_connect(Some(Duration::from_secs(15)))
        .user_agent(concat!("vi-launcher/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn check_host(sources: &Sources, url: &str) -> Result<(), UpdateError> {
    let (scheme, rest) = url.split_once("://").ok_or_else(|| UpdateError::Host(url.into()))?;
    let secure = scheme.eq_ignore_ascii_case("https") || (sources.allow_http && scheme.eq_ignore_ascii_case("http"));
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let name = if host.starts_with('[') { host } else { host.split(':').next().unwrap_or("") };
    if secure && sources.hosts.iter().any(|h| h.eq_ignore_ascii_case(name)) {
        Ok(())
    } else {
        Err(UpdateError::Host(url.into()))
    }
}

/// GETs an address, following up to five redirects, each checked against
/// the hosts.
fn get(sources: &Sources, url: &str, accept: &str) -> Result<ureq::http::Response<ureq::Body>, UpdateError> {
    let agent = agent();
    let mut url = url.to_owned();
    for _ in 0..6 {
        check_host(sources, &url)?;
        let response = agent
            .get(&url)
            .header("Accept", accept)
            .call()
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        let status = response.status();
        if status.is_redirection() {
            let next = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| UpdateError::Network(format!("{status} without a location")))?;
            url = next.to_owned();
            continue;
        }
        if status.as_u16() == 404 {
            return Err(UpdateError::NotFound(url));
        }
        if !status.is_success() {
            return Err(UpdateError::Network(format!("{status} from {url}")));
        }
        return Ok(response);
    }
    Err(UpdateError::Network("too many redirects".into()))
}

fn read_manifest(sources: &Sources, url: &str) -> Result<Manifest, UpdateError> {
    let mut response = get(sources, url, "application/json")?;
    let text = response
        .body_mut()
        .with_config()
        .limit(MANIFEST_LIMIT)
        .read_to_string()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|e| UpdateError::Manifest(e.to_string()))?;
    if manifest.format != 1 {
        return Err(UpdateError::Manifest(format!("format {}", manifest.format)));
    }
    semver::Version::parse(&manifest.version).map_err(|e| UpdateError::Manifest(e.to_string()))?;
    for asset in manifest.assets.values() {
        check_host(sources, &asset.url)?;
        if asset.sha256.len() != 64 || !asset.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(UpdateError::Manifest("a sha256 that is not 64 hex digits".into()));
        }
    }
    Ok(manifest)
}

#[derive(Deserialize)]
struct Release {
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

/// The manifest of the newest release: from Pages, else from the newest
/// GitHub release's `latest.json` asset.
pub fn latest(sources: &Sources) -> Result<Found, UpdateError> {
    let pages_error = match read_manifest(sources, &sources.pages) {
        Ok(manifest) => return Ok(Found { manifest, source: "GitHub Pages" }),
        Err(e) => e,
    };
    let mut response = get(sources, &sources.releases_api, "application/vnd.github+json").map_err(|e| match (&pages_error, &e) {
        (UpdateError::NotFound(_), UpdateError::NotFound(_)) => UpdateError::NoRelease,
        _ => UpdateError::Network(format!("{pages_error}; releases: {e}")),
    })?;
    let text = response
        .body_mut()
        .with_config()
        .limit(MANIFEST_LIMIT)
        .read_to_string()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let release: Release = serde_json::from_str(&text).map_err(|e| UpdateError::Manifest(e.to_string()))?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == "latest.json")
        .ok_or_else(|| UpdateError::Manifest(format!("{pages_error}; the newest release has no latest.json")))?;
    let manifest = read_manifest(sources, &asset.browser_download_url)?;
    Ok(Found { manifest, source: "GitHub Releases" })
}

/// A release newer than this launcher, if there is one.
pub fn check(sources: &Sources) -> Result<Option<Found>, UpdateError> {
    let found = latest(sources)?;
    let ours = semver::Version::parse(current_version()).expect("the crate's version");
    let theirs = semver::Version::parse(&found.manifest.version).map_err(|e| UpdateError::Manifest(e.to_string()))?;
    Ok((theirs > ours).then_some(found))
}

/// Downloads this platform's bundle, checks it against the manifest and
/// unpacks it into a folder of its own; `progress` gets the bytes so far.
pub fn download(sources: &Sources, manifest: &Manifest, progress: &dyn Fn(u64, u64)) -> Result<PathBuf, UpdateError> {
    let asset = manifest.assets.get(platform()).ok_or(UpdateError::NoAsset(platform()))?;
    if asset.size > BUNDLE_LIMIT {
        return Err(UpdateError::Manifest(format!("a bundle of {} bytes", asset.size)));
    }
    let dir = paths::downloads_dir();
    std::fs::create_dir_all(&dir).map_err(|e| UpdateError::Unpack(e.to_string()))?;
    let archive = dir.join(format!("{}-{}.zip", manifest.version, platform()));
    let response = get(sources, &asset.url, "application/octet-stream")?;
    let mut reader = response.into_body().into_with_config().limit(asset.size + 1).reader();
    let got = {
        use sha2::{Digest, Sha256};
        let mut file = std::fs::File::create(&archive).map_err(|e| UpdateError::Unpack(e.to_string()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut total = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| UpdateError::Network(e.to_string()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            std::io::Write::write_all(&mut file, &buf[..n]).map_err(|e| UpdateError::Unpack(e.to_string()))?;
            total += n as u64;
            progress(total, asset.size);
        }
        if total != asset.size {
            let _ = std::fs::remove_file(&archive);
            return Err(UpdateError::Mismatch { expected: format!("{} bytes", asset.size), got: format!("{total} bytes") });
        }
        crate::options::hex(&hasher.finalize())
    };
    if !got.eq_ignore_ascii_case(&asset.sha256) {
        let _ = std::fs::remove_file(&archive);
        return Err(UpdateError::Mismatch { expected: asset.sha256.clone(), got });
    }
    let out = dir.join(format!("{}-{}", manifest.version, platform()));
    let _ = std::fs::remove_dir_all(&out);
    unpack(&archive, &out)?;
    Ok(out)
}

/// Unpacks a bundle, refusing any entry that would land outside `out`.
pub fn unpack(archive: &Path, out: &Path) -> Result<(), UpdateError> {
    let file = std::fs::File::open(archive).map_err(|e| UpdateError::Unpack(e.to_string()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| UpdateError::Unpack(e.to_string()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| UpdateError::Unpack(e.to_string()))?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(UpdateError::Unpack(format!("an entry outside the bundle: {:?}", entry.name())));
        };
        if relative.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(UpdateError::Unpack(format!("an entry outside the bundle: {:?}", entry.name())));
        }
        let target = out.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| UpdateError::Unpack(e.to_string()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| UpdateError::Unpack(e.to_string()))?;
        }
        let mut file = std::fs::File::create(&target).map_err(|e| UpdateError::Unpack(e.to_string()))?;
        std::io::copy(&mut entry, &mut file).map_err(|e| UpdateError::Unpack(e.to_string()))?;
    }
    Ok(())
}

/// The launcher's own file name in a bundle.
pub fn launcher_name() -> &'static str {
    if cfg!(windows) { "ED_Launcher.exe" } else { "ED_Launcher" }
}

/// Puts an unpacked bundle in place of the installed one: every file but the
/// launcher is copied over the install folder, then the launcher replaces its
/// own executable. The window then asks to be restarted.
pub fn install(unpacked: &Path, install_dir: &Path) -> Result<(), UpdateError> {
    let own = unpacked.join(launcher_name());
    copy_tree(unpacked, install_dir, &own)?;
    if own.is_file() {
        self_replace::self_replace(&own).map_err(|e| UpdateError::Unpack(e.to_string()))?;
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path, skip: &Path) -> Result<(), UpdateError> {
    std::fs::create_dir_all(to).map_err(|e| UpdateError::Unpack(e.to_string()))?;
    for entry in std::fs::read_dir(from).map_err(|e| UpdateError::Unpack(e.to_string()))? {
        let entry = entry.map_err(|e| UpdateError::Unpack(e.to_string()))?;
        let path = entry.path();
        if path == skip {
            continue;
        }
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target, skip)?;
        } else {
            std::fs::copy(&path, &target)
                .map_err(|e| UpdateError::Unpack(format!("{}: {e}", target.display())))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sources(port: u16) -> Sources {
        Sources {
            pages: format!("http://127.0.0.1:{port}/latest.json"),
            releases_api: format!("http://127.0.0.1:{port}/api"),
            hosts: vec!["127.0.0.1".into()],
            allow_http: true,
        }
    }

    #[test]
    fn hosts() {
        let s = Sources::fork();
        assert!(check_host(&s, "https://github.com/edsuuu/VI-BR-PORT/releases/download/v1/a.zip").is_ok());
        assert!(check_host(&s, "https://edsuuu.github.io/VI-BR-PORT/latest.json").is_ok());
        assert!(check_host(&s, "http://github.com/a").is_err());
        assert!(check_host(&s, "https://github.com.evil.example/a").is_err());
        assert!(check_host(&s, "https://user@evil.example/a").is_err());
        assert!(check_host(&s, "https://github.com@evil.example/a").is_err());
    }

    #[test]
    fn unpack_refuses_entries_outside() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            zip.start_file("../evil.txt", zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(b"x").unwrap();
            zip.finish().unwrap();
        }
        assert!(unpack(&archive, &dir.path().join("out")).is_err());
        assert!(!dir.path().join("evil.txt").exists());
    }

    /// A local server stands in for Pages: the manifest is read, the bundle
    /// downloaded, checked and unpacked; a bundle that does not match is not.
    #[test]
    fn pages_manifest_download_and_check() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let mut bundle = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bundle));
            zip.start_file("victory/victory.exe", zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(b"MZ build").unwrap();
            zip.finish().unwrap();
        }
        let sha = {
            use sha2::{Digest, Sha256};
            crate::options::hex(&Sha256::digest(&bundle))
        };
        let manifest = |sha: &str| {
            format!(
                r#"{{"format":1,"version":"99.0.0","assets":{{"{}":{{"url":"http://127.0.0.1:{port}/bundle.zip","sha256":"{sha}","size":{}}}}}}}"#,
                platform(),
                bundle.len()
            )
        };
        let good = manifest(&sha);
        let bad = manifest(&"0".repeat(64));
        let body = bundle.clone();
        let handle = std::thread::spawn(move || {
            for (i, request) in server.incoming_requests().take(4).enumerate() {
                let reply = match request.url() {
                    "/latest.json" if i < 2 => tiny_http::Response::from_data(good.clone().into_bytes()),
                    "/latest.json" => tiny_http::Response::from_data(bad.clone().into_bytes()),
                    _ => tiny_http::Response::from_data(body.clone()),
                };
                request.respond(reply).unwrap();
            }
        });
        let s = sources(port);
        let data = tempfile::tempdir().unwrap();
        // SAFETY: only this test reads the variable.
        unsafe { std::env::set_var("VICTORY_LAUNCHER_CONFIG", data.path().join("launcher.json")) };
        let found = check(&s).unwrap().expect("99.0.0 is newer");
        assert_eq!(found.source, "GitHub Pages");
        let out = download(&s, &found.manifest, &|_, _| {}).unwrap();
        assert_eq!(std::fs::read(out.join("victory").join("victory.exe")).unwrap(), b"MZ build");
        let bad = latest(&s).unwrap();
        assert!(matches!(download(&s, &bad.manifest, &|_, _| {}), Err(UpdateError::Mismatch { .. })));
        handle.join().unwrap();
    }
}
