//! Getting the file behind a pasted link (spike S4, D1): Giphy and Google
//! Photos have their own rules, direct media URLs are plain downloads, and
//! everything else goes to yt-dlp.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sg_media::tool::{Cancel, ToolError, run_lines};

/// A file fetched into the bundle's staging folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    pub path: PathBuf,
    pub label: String,
    pub uploader: Option<String>,
}

const USER_AGENT: &str = "Shotgun/0.1 (video editor; media import)";
/// Google serves the share page's data only to something that looks like a browser.
const BROWSER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0";

#[derive(Debug, PartialEq)]
pub(crate) enum Route {
    Giphy {
        id: String,
    },
    GooglePhotos,
    /// Direct download if the server says it is media, yt-dlp otherwise.
    Generic,
}

pub(crate) fn route(url: &str) -> Route {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let path = path.split(['?', '#']).next().unwrap_or("");
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if host == "giphy.com" || host.ends_with(".giphy.com") {
        let id = if host.starts_with("media") || segments.first() == Some(&"media") {
            // media.giphy.com/media/<id>/giphy.gif
            segments.len().checked_sub(2).map(|i| segments[i])
        } else {
            // giphy.com/gifs/<words>-<id>
            segments.last().and_then(|s| s.rsplit('-').next())
        };
        if let Some(id) = id.filter(|id| id.len() >= 8 && id.chars().all(|c| c.is_ascii_alphanumeric())) {
            return Route::Giphy { id: id.to_owned() };
        }
    }
    if host == "photos.app.goo.gl" || (host == "photos.google.com" && segments.first() == Some(&"share")) {
        return Route::GooglePhotos;
    }
    Route::Generic
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(15))).build().into()
}

fn http_error(url: &str, e: impl std::fmt::Display) -> ToolError {
    ToolError::other(format!("download of {url} failed: {e}"))
}

/// Streams a response body into `path`, reporting progress.
fn save_body(
    mut body: impl Read,
    total: Option<u64>,
    path: &Path,
    cancel: &Cancel,
    progress: &mut dyn FnMut(Option<f32>),
) -> Result<u64, ToolError> {
    let mut file = std::fs::File::create(path)?;
    let mut buf = vec![0u8; 1 << 16];
    let mut done = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let n = body.read(&mut buf)?;
        if n == 0 {
            return Ok(done);
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress(total.filter(|t| *t > 0).map(|t| (done as f64 / t as f64).min(1.0) as f32));
    }
}

fn file_name_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or("");
    // Only harmless characters survive into a file name.
    let clean: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).collect();
    if clean.trim_matches('.').is_empty() { "download".to_owned() } else { clean }
}

fn extension_for(content_type: &str) -> Option<&'static str> {
    Some(match content_type.split(';').next()?.trim() {
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "image/gif" => "gif",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/flac" => "flac",
        "audio/ogg" => "ogg",
        "audio/mp4" => "m4a",
        _ => return None,
    })
}

/// Plain GET of a media URL into `dir`. `Ok(None)` if the server answers
/// with something that is not media (a web page): the caller then tries
/// yt-dlp.
fn direct(
    url: &str,
    dir: &Path,
    name_hint: Option<&str>,
    user_agent: &str,
    cancel: &Cancel,
    progress: &mut dyn FnMut(Option<f32>),
) -> Result<Option<PathBuf>, ToolError> {
    let mut response = agent()
        .get(url)
        .header("User-Agent", user_agent)
        .header("Accept", "*/*")
        .call()
        .map_err(|e| http_error(url, e))?;
    let content_type =
        response.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    if !["video/", "image/", "audio/"].iter().any(|p| content_type.starts_with(p)) {
        return Ok(None);
    }
    let total = response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let mut name = name_hint.map_or_else(|| file_name_from_url(url), str::to_owned);
    if Path::new(&name).extension().is_none()
        && let Some(ext) = extension_for(&content_type)
    {
        name = format!("{name}.{ext}");
    }
    let path = dir.join(name);
    let got = save_body(response.body_mut().as_reader(), total, &path, cancel, progress)?;
    if total.is_some_and(|t| t != got) {
        return Err(http_error(url, format!("connection closed after {got} of {} bytes", total.unwrap_or(0))));
    }
    Ok(Some(path))
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn giphy(
    id: &str,
    dir: &Path,
    cancel: &Cancel,
    progress: &mut dyn FnMut(Option<f32>),
) -> Result<Vec<Fetched>, ToolError> {
    // The original upload; yt-dlp would pick the worst rendition (S4, 1.2).
    for rendition in ["source.gif", "giphy.gif"] {
        let url = format!("https://media.giphy.com/media/{id}/{rendition}");
        match direct(&url, dir, Some(&format!("giphy-{id}.gif")), USER_AGENT, cancel, progress) {
            Ok(Some(path)) => return Ok(vec![Fetched { path, label: format!("giphy {id}"), uploader: None }]),
            Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
            _ => {}
        }
    }
    Err(ToolError::other("Giphy did not return this GIF"))
}

/// One item of a Google Photos share page.
#[derive(Debug, PartialEq)]
pub(crate) struct PhotoItem {
    pub url: String,
    pub video: bool,
}

/// Finds the media items in the page source. Relies on the page's
/// undocumented structure (S4, 1.3):
/// `["AF1Qip...",["https://lh3.googleusercontent.com/pw/<token>",<w>,<h>,...`
pub(crate) fn google_photo_items(html: &str) -> Vec<PhotoItem> {
    const KEY: &str = "[\"AF1Qip";
    const URL: &str = "\",[\"https://lh3.googleusercontent.com/pw/";
    let mut items: Vec<(String, PhotoItem)> = Vec::new();
    let starts: Vec<usize> = html.match_indices(KEY).map(|(i, _)| i).collect();
    for (n, &start) in starts.iter().enumerate() {
        let rest = &html[start + 2..];
        let Some(key_end) = rest.find('"') else { continue };
        let key = &rest[..key_end];
        let after_key = &rest[key_end..];
        if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || !after_key.starts_with(URL) {
            continue;
        }
        let url_start = &after_key[4..];
        let Some(url_end) = url_start.find('"') else { continue };
        let url = &url_start[..url_end];
        if items.iter().any(|(k, _)| k == key) {
            continue;
        }
        // The record runs to the next item; a video carries this marker in it.
        let record_end = starts.get(n + 1).map_or(html.len(), |&next| next).min(start + 2000);
        let record = html.get(start..record_end).unwrap_or("");
        items.push((key.to_owned(), PhotoItem { url: url.to_owned(), video: record.contains("\"76647426\"") }));
    }
    items.into_iter().map(|(_, item)| item).collect()
}

const GOOGLE_PHOTOS_HELP: &str =
    "Google Photos link could not be read. Download the file in the browser and drop it here.";

fn google_photos(
    url: &str,
    dir: &Path,
    cancel: &Cancel,
    progress: &mut dyn FnMut(Option<f32>),
) -> Result<Vec<Fetched>, ToolError> {
    let page = agent()
        .get(url)
        .header("User-Agent", BROWSER_AGENT)
        .call()
        .and_then(|mut r| r.body_mut().with_config().limit(32 << 20).read_to_string())
        .map_err(|e| ToolError::other(format!("{GOOGLE_PHOTOS_HELP} ({e})")))?;
    let items = google_photo_items(&page);
    if items.is_empty() {
        return Err(ToolError::other(GOOGLE_PHOTOS_HELP));
    }
    let mut fetched = Vec::new();
    let count = items.len();
    for (i, item) in items.into_iter().enumerate() {
        // "=d" is the original photo. "=dv" for videos is unverified (01, open points).
        let suffix = if item.video { "=dv" } else { "=d" };
        let name = format!("google-photos-{}", i + 1);
        let mut scaled = |f: Option<f32>| progress(f.map(|f| (i as f32 + f) / count as f32));
        match direct(&format!("{}{suffix}", item.url), dir, Some(&name), BROWSER_AGENT, cancel, &mut scaled) {
            Ok(Some(path)) => fetched.push(Fetched { label: format!("Google Photos {}", i + 1), path, uploader: None }),
            Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
            Ok(None) | Err(_) => log::warn!("Google Photos item {} could not be downloaded", i + 1),
        }
    }
    if fetched.is_empty() { Err(ToolError::other(GOOGLE_PHOTOS_HELP)) } else { Ok(fetched) }
}

fn yt_dlp(
    url: &str,
    sound_only: bool,
    dir: &Path,
    cancel: &Cancel,
    progress: &mut dyn FnMut(Option<f32>),
) -> Result<Vec<Fetched>, ToolError> {
    let mut cmd = Command::new("yt-dlp");
    cmd.args(["--no-playlist", "--newline", "--no-simulate", "--write-info-json"]);
    // No HDR pipeline yet (05-color): take the SDR rendition.
    cmd.args(["-S", "hdr:SDR"]);
    if sound_only {
        // The best sound stream as the site has it; nothing is re-encoded.
        cmd.args(["-f", "bestaudio/best"]);
    }
    cmd.args([
        "--progress-template",
        "download:SGPROG %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s",
    ]);
    cmd.args(["--print", "after_move:SGFILE %(filepath)s"]);
    cmd.arg("-o").arg(dir.join("%(id)s.%(ext)s")).arg("--").arg(url);
    let mut files = Vec::new();
    run_lines(cmd, cancel, |line| {
        if let Some(rest) = line.strip_prefix("SGPROG ") {
            let mut parts = rest.split(' ').map(|p| p.parse::<f64>().ok());
            let done = parts.next().flatten();
            let total = parts.next().flatten().or_else(|| parts.next().flatten());
            progress(done.zip(total).filter(|(_, t)| *t > 0.0).map(|(d, t)| (d / t).min(1.0) as f32));
        } else if let Some(path) = line.strip_prefix("SGFILE ") {
            files.push(PathBuf::from(path));
        }
    })
    .map_err(|e| match e {
        ToolError::Failed { stderr, .. } => {
            // yt-dlp prefixes the useful line with "ERROR:".
            let reason = stderr.lines().rev().find(|l| l.contains("ERROR")).unwrap_or(&stderr).trim().to_owned();
            ToolError::other(format!("This link could not be downloaded. {reason}"))
        }
        other => other,
    })?;
    #[derive(serde::Deserialize)]
    struct Info {
        title: Option<String>,
        uploader: Option<String>,
    }
    let fetched: Vec<Fetched> = files
        .into_iter()
        .map(|path| {
            let info = std::fs::read(path.with_extension("info.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<Info>(&b).ok());
            let (title, uploader) = info.map_or((None, None), |i| (i.title, i.uploader));
            Fetched { label: title.unwrap_or_else(|| stem(&path)), path, uploader }
        })
        .collect();
    if fetched.is_empty() { Err(ToolError::other("yt-dlp finished without producing a file")) } else { Ok(fetched) }
}

/// Downloads what `url` points to into `dir` (which exists and is empty).
pub fn fetch(
    url: &str,
    sound_only: bool,
    dir: &Path,
    cancel: &Cancel,
    progress: &mut dyn FnMut(Option<f32>),
) -> Result<Vec<Fetched>, ToolError> {
    match route(url) {
        Route::Giphy { id } => giphy(&id, dir, cancel, progress),
        Route::GooglePhotos => google_photos(url, dir, cancel, progress),
        Route::Generic => match direct(url, dir, None, USER_AGENT, cancel, progress) {
            Ok(Some(path)) => Ok(vec![Fetched { label: stem(&path), path, uploader: None }]),
            Err(ToolError::Cancelled) => Err(ToolError::Cancelled),
            // Not media, or the server dislikes plain clients: yt-dlp knows more sites.
            Ok(None) | Err(_) => yt_dlp(url, sound_only, dir, cancel, progress),
        },
    }
}

/// True for text that should be imported as a link.
pub fn looks_like_url(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with("https://") || t.starts_with("http://")) && !t.contains(char::is_whitespace) && t.len() > 10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        let giphy = |id: &str| Route::Giphy { id: id.to_owned() };
        assert_eq!(route("https://giphy.com/gifs/cat-funny-JIX9t2j0ZTN9S"), giphy("JIX9t2j0ZTN9S"));
        assert_eq!(route("https://media2.giphy.com/media/JIX9t2j0ZTN9S/giphy.gif?cid=1"), giphy("JIX9t2j0ZTN9S"));
        assert_eq!(route("https://media.giphy.com/media/v1.Y2lkPTc5/JIX9t2j0ZTN9S/200.webp"), giphy("JIX9t2j0ZTN9S"));
        assert_eq!(route("https://giphy.com/explore/cats"), Route::Generic);
        assert_eq!(route("https://photos.app.goo.gl/CSV7NDstShTUwUZq5"), Route::GooglePhotos);
        assert_eq!(route("https://photos.google.com/share/AF1QipX?key=abc"), Route::GooglePhotos);
        assert_eq!(route("https://www.youtube.com/watch?v=abc"), Route::Generic);
        assert_eq!(route("https://example.com/a.mp4"), Route::Generic);
    }

    #[test]
    fn google_photos_page_parsing() {
        let html = concat!(
            r#"x,["AF1QipAAA_1",["https://lh3.googleusercontent.com/pw/TOKEN-one",4000,3000,null],1700,"#,
            r#""z"],["AF1QipBBB-2",["https://lh3.googleusercontent.com/pw/TOKEN_two",1920,1080,null],1701,{"76647426":[1]}],"#,
            r#"["AF1QipAAA_1",["https://lh3.googleusercontent.com/pw/TOKEN-one",4000,3000]],["AF1QipCCC","other"]"#
        );
        assert_eq!(
            google_photo_items(html),
            [
                PhotoItem { url: "https://lh3.googleusercontent.com/pw/TOKEN-one".into(), video: false },
                PhotoItem { url: "https://lh3.googleusercontent.com/pw/TOKEN_two".into(), video: true },
            ]
        );
        assert!(google_photo_items("<html>nothing</html>").is_empty());
    }

    #[test]
    fn names_and_urls() {
        assert_eq!(file_name_from_url("https://x.org/a/My%20Clip.mp4?token=1"), "My20Clip.mp4");
        assert_eq!(file_name_from_url("https://x.org/"), "download");
        assert_eq!(file_name_from_url("https://x.org/../../etc"), "etc");
        assert_eq!(extension_for("image/gif; charset=binary"), Some("gif"));
        assert!(looks_like_url(" https://youtu.be/abc123 "));
        assert!(!looks_like_url("https://a b"));
        assert!(!looks_like_url("just some text"));
    }
}

#[cfg(test)]
mod saved_page {
    /// The page saved by spike S4, when present on this machine: the parser
    /// must find the same items as the spike's script did.
    #[test]
    fn real_google_photos_page() {
        let dir = std::path::Path::new("/disk/shotgun-dev/media/s4/gphotos");
        let (Ok(html), Ok(json)) = (
            std::fs::read_to_string(dir.join("LsiQNDAp6K8DZAap8.html")),
            std::fs::read_to_string(dir.join("LsiQNDAp6K8DZAap8.json")),
        ) else {
            eprintln!("saved page not found: skipping");
            return;
        };
        let items = super::google_photo_items(&html);
        let expected = json.matches("\"url\"").count();
        assert!(expected > 0);
        assert_eq!(items.len(), expected);
        assert!(items.iter().all(|i| json.contains(&i.url)));
        assert_eq!(items.iter().filter(|i| i.video).count(), json.matches("\"video\": true").count());
    }
}
