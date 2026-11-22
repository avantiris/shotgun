//! Changing a part of a video by a sentence (docs/spec/10-ai.md, feedback
//! F17.4): the part a clip shows is handed whole to a generative model,
//! which gives back a video with the change made ("dark, epic storm
//! clouds in the sky"). Such models do not run inside Shotgun. Which ones
//! there are is written in a file on this machine, outside every project:
//! services on the network (fal.ai, Runway, Replicate: anything those
//! three offer, by its model's name and fields) or any program on this
//! machine (`kind = "command"`), which is also how a local model or a
//! service nobody thought of here is used.
//!
//! Nothing leaves the machine but what [`change`] is called for: the part
//! of the picture, without sound, and the sentence. Network and file I/O:
//! job threads only.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sg_media::{Cancel, ToolError};

/// How a service is spoken to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Fal,
    Runway,
    Replicate,
    /// A program on this machine.
    Command,
}

/// One entry of the file: a model at a service, or a program.
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    /// What it is called in the program.
    pub name: String,
    pub kind: Kind,
    /// The model at the service, as the service names it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The key is in this environment variable instead of in the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key_env: Option<String>,
    /// What a second of video costs there, in dollars: for the estimate
    /// shown before anything is sent. Written by the user; not looked up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_per_second: Option<f64>,
    /// The shortest and longest part the model takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_seconds: Option<f64>,
    /// The names of the request's fields for the video and the sentence,
    /// where the model has other ones than the service's usual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    video_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_field: Option<String>,
    /// What is sent as the sentence, with `{sentence}` where the user's
    /// goes (some models want the clip named in it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    /// Where in the answer the video's address is, as `a.b.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<String>,
    /// Further fields of the request, as the model takes them.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    input: Map<String, Value>,
    /// Another address than the service's own (a mirror; the tests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    /// `kind = "command"`: the program and its arguments, with `{in}`,
    /// `{out}` and `{prompt}` where the files' paths and the sentence go.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
}

/// (Written by hand: the key is not for logs.)
impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Service({:?}, {:?}, {:?})", self.name, self.kind, self.model)
    }
}

#[derive(Deserialize, Serialize)]
struct File {
    #[serde(default)]
    service: Vec<Service>,
}

/// What the file looks like, shown where there is none yet.
pub const EXAMPLE: &str = r#"[[service]]
name = "fal: Wan 2.7 edit"
kind = "fal"                       # fal, runway, replicate or command
model = "fal-ai/wan/v2.7/edit-video"
key = "your key from fal.ai"
price_per_second = 0.15            # for the estimate only
min_seconds = 2
max_seconds = 10
[service.input]                    # anything else the model takes
resolution = "1080p"

[[service]]
name = "Runway: Aleph"
kind = "runway"
model = "aleph2"
key = "your key from runwayml.com"
price_per_second = 0.28

[[service]]
name = "A program of mine"
kind = "command"
command = ["my-model", "--in", "{in}", "--out", "{out}", "--prompt", "{prompt}"]
"#;

/// Where the file is: `SHOTGUN_AI`, or `shotgun/ai.toml` in the configuration folder.
pub fn file() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SHOTGUN_AI") {
        return Some(PathBuf::from(path));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("shotgun/ai.toml"))
}

/// The services written in `path`; none if there is no such file.
pub fn services(path: &Path) -> Result<Vec<Service>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{} could not be read: {e}", path.display())),
    };
    let file: File = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for service in &file.service {
        service.check().map_err(|e| format!("{}: \"{}\": {e}", path.display(), service.name))?;
    }
    Ok(file.service)
}

/// Writes the services into `path` (what the window for it does): the
/// whole file anew, so remarks written into it by hand are gone. Only
/// its owner may read it: it holds keys.
pub fn save(path: &Path, services: &[Service]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let failed = |e: &dyn std::fmt::Display| format!("{} could not be written: {e}", path.display());
    let text = toml::to_string(&File { service: services.to_vec() }).map_err(|e| failed(&e))?;
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(|e| failed(&e))?;
    }
    // Written beside and renamed: a crash never leaves half a file.
    let beside = path.with_extension("toml.new");
    std::fs::write(&beside, text).map_err(|e| failed(&e))?;
    std::fs::set_permissions(&beside, std::fs::Permissions::from_mode(0o600)).map_err(|e| failed(&e))?;
    std::fs::rename(&beside, path).map_err(|e| failed(&e))
}

/// Models to begin from, as their services described them on 2026-11-22
/// (names, limits and prices from their pages; none tried with a key).
/// Each still needs the user's key.
pub fn presets() -> Vec<Service> {
    let one = |name: &str, kind: Kind, model: &str, price: f64, least: Option<f64>, most: Option<f64>| Service {
        name: name.to_owned(),
        kind,
        model: model.to_owned(),
        price_per_second: Some(price),
        min_seconds: least,
        max_seconds: most,
        ..Service::blank(kind)
    };
    let mut wan = one("fal.ai: Wan 2.7 edit", Kind::Fal, "fal-ai/wan/v2.7/edit-video", 0.15, Some(2.0), Some(10.0));
    wan.input.insert("resolution".into(), json!("1080p"));
    let mut kling = one(
        "fal.ai: Kling O3 edit",
        Kind::Fal,
        "fal-ai/kling-video/o3/pro/video-to-video/edit",
        0.168,
        Some(3.0),
        Some(15.0),
    );
    // (This model wants the clip named in the sentence.)
    kling.prompt = Some("In @Video1: {sentence}".to_owned());
    kling.input.insert("keep_audio".into(), json!(false));
    vec![
        wan,
        kling,
        one("fal.ai: Lucy Edit", Kind::Fal, "decart/lucy-edit/pro", 0.15, None, None),
        one("Runway: Aleph", Kind::Runway, "aleph2", 0.28, Some(2.0), Some(30.0)),
        one("Replicate: Wan 2.7 edit", Kind::Replicate, "wan-video/wan-2.7-videoedit", 0.15, Some(2.0), Some(10.0)),
        one("Replicate: P-Video edit", Kind::Replicate, "prunaai/p-video-edit", 0.045, None, Some(15.0)),
    ]
}

/// A program's words as one line, and back: words with spaces in them stand in double quotes.
pub fn command_line(command: &[String]) -> String {
    let quoted =
        |word: &String| if word.is_empty() || word.contains(' ') { format!("\"{word}\"") } else { word.clone() };
    command.iter().map(quoted).collect::<Vec<_>>().join(" ")
}

pub fn command_words(line: &str) -> Vec<String> {
    let (mut words, mut word, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in line.chars() {
        match c {
            '"' => (quoted, any) = (!quoted, true),
            c if c.is_whitespace() && !quoted => {
                if any || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                any = false;
            }
            c => word.push(c),
        }
    }
    if any || !word.is_empty() {
        words.push(word);
    }
    words
}

impl Service {
    /// One with nothing filled in yet.
    pub fn blank(kind: Kind) -> Service {
        Service {
            name: String::new(),
            kind,
            model: String::new(),
            key: None,
            key_env: None,
            price_per_second: None,
            min_seconds: None,
            max_seconds: None,
            video_field: None,
            prompt_field: None,
            prompt: None,
            result: None,
            input: Map::new(),
            base_url: None,
            command: Vec::new(),
        }
    }

    /// What is still missing before it can be used, in words for its window and for the file's reader.
    pub fn check(&self) -> Result<(), String> {
        match self.kind {
            _ if self.name.trim().is_empty() => Err("it needs a name".into()),
            Kind::Command if self.command.is_empty() => Err("it needs the program to run".into()),
            Kind::Command if !self.command.iter().any(|arg| arg.contains("{out}")) => {
                Err("the program needs {out} among its words: where it writes the video".into())
            }
            Kind::Command => Ok(()),
            _ if self.model.is_empty() => Err("it needs the model's name at the service".into()),
            _ if self.key().is_none() => Err("it needs your key from the service".into()),
            _ => Ok(()),
        }
    }

    fn key(&self) -> Option<String> {
        let from_env = self.key_env.as_ref().and_then(|name| std::env::var(name).ok());
        from_env.or_else(|| self.key.clone()).filter(|key| !key.is_empty())
    }

    /// Whether it runs on this machine (nothing is sent anywhere by Shotgun).
    pub fn is_local(&self) -> bool {
        self.kind == Kind::Command
    }

    /// Where the picture goes, for what is said before it is sent.
    pub fn goes_to(&self) -> String {
        let host = |own: &str| match &self.base_url {
            Some(other) => other.clone(),
            None => own.to_owned(),
        };
        match self.kind {
            Kind::Fal => format!("{} (model {})", host("fal.ai"), self.model),
            Kind::Runway => format!("{} (model {})", host("runwayml.com"), self.model),
            Kind::Replicate => format!("{} (model {})", host("replicate.com"), self.model),
            Kind::Command => self.command.join(" "),
        }
    }

    fn sentence(&self, sentence: &str) -> String {
        match &self.prompt {
            Some(around) => around.replace("{sentence}", sentence),
            None => sentence.to_owned(),
        }
    }

    /// The request's fields: the model's own, the video and the sentence.
    fn fields(&self, video: &str, video_field: &str, prompt_field: &str, sentence: &str) -> Map<String, Value> {
        let mut fields = self.input.clone();
        fields.insert(self.video_field.clone().unwrap_or_else(|| video_field.to_owned()), json!(video));
        fields.insert(
            self.prompt_field.clone().unwrap_or_else(|| prompt_field.to_owned()),
            json!(self.sentence(sentence)),
        );
        fields
    }
}

/// The name of the asset that is a part of `asset` changed by a sentence:
/// as [`crate::analysis::sky_name`], with what tells two changes of the
/// same part apart (the service and the sentence, as a few letters).
pub fn change_name(asset: &sg_core::Asset, service: &str, sentence: &str, src_in: i64, span: i64) -> String {
    let told = blake3::hash(format!("{service}\n{sentence}").as_bytes()).to_hex();
    format!("{} changed {} {src_in}-{span}", sg_core::text::slug(&asset.label), &told[..8])
}

/// Taken apart again: the slug of the video it was made from, and where in it the part begins.
pub fn change_origin(label: &str) -> Option<(&str, i64)> {
    let (rest, part) = label.rsplit_once(' ')?;
    let (rest, told) = rest.rsplit_once(' ')?;
    let slug = rest.strip_suffix(" changed")?;
    let (src_in, span) = part.split_once('-')?;
    span.parse::<i64>().ok()?;
    (told.len() == 8 && told.chars().all(|c| c.is_ascii_hexdigit())).then_some(())?;
    Some((slug, src_in.parse().ok()?))
}

/// What is going on, and how far it is where that is known.
pub type Stage<'a> = &'a mut dyn FnMut(&'static str, Option<f32>);

/// The part of a video asset that a clip shows, changed by `sentence` at
/// `service`: written into the project's cache with the size, frame rate
/// and length of the part itself, and returned to be imported like any
/// other file. `src_in` and `span` in frames of `rate` (the sequence's).
#[allow(clippy::too_many_arguments)]
pub fn change(
    root: &Path,
    asset: &sg_core::Asset,
    service: &Service,
    sentence: &str,
    src_in: i64,
    span: i64,
    rate: sg_core::Rate,
    cancel: &Cancel,
    stage: Stage<'_>,
) -> Result<PathBuf, ToolError> {
    let video = asset.video.as_ref().filter(|v| !v.is_still()).ok_or_else(|| ToolError::other("it is not a video"))?;
    let (source, from, frames) = crate::analysis::source_part(root, asset, video, src_in, span, rate)?;
    let per_second = video.rate.num() as f64 / video.rate.den() as f64;
    let seconds = frames as f64 / per_second;
    if let Some(most) = service.max_seconds.filter(|most| seconds > *most + 0.05) {
        return Err(ToolError::other(format!(
            "the clip is {seconds:.1} s long and {} takes at most {most} s",
            service.name
        )));
    }
    if let Some(least) = service.min_seconds.filter(|least| seconds < *least - 0.05) {
        return Err(ToolError::other(format!(
            "the clip is {seconds:.1} s long and {} takes at least {least} s",
            service.name
        )));
    }
    let folder = root.join(".shotgun/cache/ai");
    std::fs::create_dir_all(&folder)?;
    let name = change_name(asset, &service.name, sentence, src_in, span);
    let sent = folder.join(format!("{name}.sent.mp4"));
    let got = folder.join(format!("{name}.got.mp4"));
    let out = folder.join(format!("{name}.mp4"));

    stage("cutting the part out", Some(0.0));
    sg_media::derive::part(&source, &sent, from, frames, seconds, cancel, &mut |f| {
        stage("cutting the part out", Some(f))
    })?;
    let made = match service.kind {
        Kind::Command => run_command(service, &sent, &got, sentence, cancel, stage),
        Kind::Fal => fal(service, &sent, &got, sentence, cancel, stage),
        Kind::Runway => runway(service, &sent, &got, sentence, cancel, stage),
        Kind::Replicate => replicate(service, &sent, &got, sentence, cancel, stage),
    };
    let _ = std::fs::remove_file(&sent);
    made?;
    // What comes back has the model's size, frame rate and length; the
    // clip needs the part's own, frame for frame.
    stage("fitting what came back", Some(0.0));
    let (width, height) =
        if video.rotation.rem_euclid(180) == 90 { (video.height, video.width) } else { (video.width, video.height) };
    let fitted = sg_media::derive::fit(&got, &out, width, height, video.rate, frames, seconds, cancel, &mut |f| {
        stage("fitting what came back", Some(f))
    });
    let _ = std::fs::remove_file(&got);
    fitted?;
    Ok(out)
}

fn run_command(
    service: &Service,
    sent: &Path,
    got: &Path,
    sentence: &str,
    cancel: &Cancel,
    stage: Stage<'_>,
) -> Result<(), ToolError> {
    let place = |arg: &String| {
        arg.replace("{in}", &sent.to_string_lossy())
            .replace("{out}", &got.to_string_lossy())
            .replace("{prompt}", &service.sentence(sentence))
    };
    let mut args = service.command.iter().map(place);
    let program = args.next().ok_or_else(|| ToolError::other("no command"))?;
    let mut command = std::process::Command::new(program);
    command.args(args);
    if let Some(key) = service.key() {
        command.env("SHOTGUN_AI_KEY", key);
    }
    stage("the model is at work", None);
    // A line `progress 0.4` on its output is shown; everything else is its own business.
    sg_media::tool::run_lines(command, cancel, |line| {
        if let Some(done) = line.strip_prefix("progress ").and_then(|n| n.trim().parse::<f32>().ok()) {
            stage("the model is at work", Some(done.clamp(0.0, 1.0)));
        }
    })?;
    if !got.is_file() {
        return Err(ToolError::other("the command ended without writing the video it was asked for (`{out}`)"));
    }
    Ok(())
}

// ---------------------------------------------------------------- the network

/// How often a service is asked whether it is done.
const ASK_EVERY: Duration = Duration::from_secs(3);
/// How long a job may take there before it is given up.
const GIVE_UP_AFTER: Duration = Duration::from_secs(45 * 60);

struct Http {
    agent: ureq::Agent,
    /// The `Authorization` header and what else every request to the service carries.
    headers: Vec<(String, String)>,
}

fn failed(what: &str, e: impl std::fmt::Display) -> ToolError {
    ToolError::other(format!("{what}: {e}"))
}

impl Http {
    fn new(headers: Vec<(String, String)>) -> Http {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            // (An upload of a hundred megabytes on a slow line.)
            .timeout_global(Some(Duration::from_secs(20 * 60)))
            // An answer that is an error is read: it says what was wrong.
            .http_status_as_error(false)
            .build()
            .into();
        Http { agent, headers }
    }

    /// The answer as JSON; an error with what the service said if it refused.
    fn answer(what: &str, sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Value, ToolError> {
        let mut response = sent.map_err(|e| failed(what, e))?;
        let status = response.status();
        let mut text = String::new();
        response.body_mut().as_reader().take(1 << 20).read_to_string(&mut text).map_err(|e| failed(what, e))?;
        if !status.is_success() {
            let said: String = text.chars().take(600).collect();
            return Err(ToolError::other(format!("{what}: the service answered {status}: {said}")));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| failed(what, format!("an answer that cannot be read ({e})")))
    }

    fn post(&self, what: &str, url: &str, body: &Value) -> Result<Value, ToolError> {
        let mut request = self.agent.post(url).header("Content-Type", "application/json");
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        Http::answer(what, request.send(body.to_string()))
    }

    fn get(&self, what: &str, url: &str) -> Result<Value, ToolError> {
        let mut request = self.agent.get(url);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        Http::answer(what, request.call())
    }

    /// Bytes to an address that was handed out for them (no key goes there
    /// unless `auth` says so).
    fn put_bytes(
        &self,
        what: &str,
        post: bool,
        url: &str,
        headers: &[(&str, &str)],
        bytes: &[u8],
    ) -> Result<Value, ToolError> {
        let mut request = if post { self.agent.post(url) } else { self.agent.put(url) };
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        Http::answer(what, request.send(bytes))
    }

    /// The video at `url` into `path`.
    fn fetch(&self, url: &str, path: &Path, cancel: &Cancel, stage: Stage<'_>) -> Result<(), ToolError> {
        let what = "fetching the result";
        stage(what, None);
        let mut response = self.agent.get(url).call().map_err(|e| failed(what, e))?;
        if !response.status().is_success() {
            return Err(ToolError::other(format!("{what}: the service answered {}", response.status())));
        }
        let total =
            response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
        let mut reader = response.body_mut().as_reader();
        let mut file = std::fs::File::create(path)?;
        let mut buf = vec![0u8; 1 << 16];
        let mut done = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let n = reader.read(&mut buf).map_err(|e| failed(what, e))?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut file, &buf[..n])?;
            done += n as u64;
            stage(what, total.map(|total| (done as f32 / total.max(1) as f32).min(1.0)));
        }
        if done == 0 {
            return Err(ToolError::other(format!("{what}: it is empty")));
        }
        Ok(())
    }
}

/// A field of an answer by its path (`video.url`, `output.0`); a list
/// stands for its first entry and `{url: ...}` for its address, so that
/// one path fits the shapes models answer in.
fn dig<'a>(value: &'a Value, path: &str) -> Option<&'a str> {
    let mut at = value;
    for step in path.split('.').filter(|step| !step.is_empty()) {
        at = match at {
            Value::Array(list) => list.get(step.parse::<usize>().ok()?)?,
            other => other.get(step)?,
        };
    }
    loop {
        at = match at {
            Value::String(text) => return Some(text),
            Value::Array(list) => list.first()?,
            Value::Object(fields) => fields.get("url")?,
            _ => return None,
        };
    }
}

fn text<'a>(value: &'a Value, field: &str, what: &str) -> Result<&'a str, ToolError> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| failed(what, format!("the answer has no `{field}`")))
}

/// Waits a moment between two questions; false when cancelled or out of time.
fn pause(cancel: &Cancel, since: std::time::Instant) -> Result<(), ToolError> {
    let until = std::time::Instant::now() + ASK_EVERY;
    while std::time::Instant::now() < until {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if since.elapsed() > GIVE_UP_AFTER {
        return Err(ToolError::other("the service did not finish within 45 minutes; given up"));
    }
    Ok(())
}

/// A form with files, as browsers send it.
fn form(fields: &[(String, String)], file_field: &str, file_name: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!("shotgun-{}", blake3::hash(bytes).to_hex());
    let mut body = Vec::with_capacity(bytes.len() + 1024);
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; filename=\"{file_name}\"\r\nContent-Type: video/mp4\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn key_of(service: &Service) -> Result<String, ToolError> {
    service.key().ok_or_else(|| ToolError::other(format!("{} has no key", service.name)))
}

/// fal.ai: the file goes to its storage, the request into its queue
/// (https://fal.ai/docs/model-endpoints/queue; the upload as its own client does it).
fn fal(
    service: &Service,
    sent: &Path,
    got: &Path,
    sentence: &str,
    cancel: &Cancel,
    stage: Stage<'_>,
) -> Result<(), ToolError> {
    let http = Http::new(vec![("Authorization".into(), format!("Key {}", key_of(service)?))]);
    let (queue, rest) = match &service.base_url {
        Some(other) => (other.trim_end_matches('/').to_owned(), other.trim_end_matches('/').to_owned()),
        None => ("https://queue.fal.run".to_owned(), "https://rest.fal.ai".to_owned()),
    };
    stage("sending the part", None);
    let bytes = std::fs::read(sent)?;
    let what = "sending the part";
    let begun = http.post(
        what,
        &format!("{rest}/storage/upload/initiate?storage_type=gcs"),
        &json!({"file_name": "part.mp4", "content_type": "video/mp4"}),
    )?;
    http.put_bytes(what, false, text(&begun, "upload_url", what)?, &[("Content-Type", "video/mp4")], &bytes)?;
    let video = text(&begun, "file_url", what)?.to_owned();
    drop(bytes);

    let what = "asking the service";
    let fields = service.fields(&video, "video_url", "prompt", sentence);
    let asked = http.post(what, &format!("{queue}/{}", service.model), &Value::Object(fields))?;
    let (status_url, response_url) =
        (text(&asked, "status_url", what)?.to_owned(), text(&asked, "response_url", what)?.to_owned());
    let since = std::time::Instant::now();
    loop {
        if let Err(e) = pause(cancel, since) {
            if let Some(url) = asked.get("cancel_url").and_then(Value::as_str) {
                let _ = http.put_bytes(
                    "cancelling",
                    false,
                    url,
                    &http.headers.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect::<Vec<_>>(),
                    &[],
                );
            }
            return Err(e);
        }
        let status = http.get("waiting for the service", &status_url)?;
        match status.get("status").and_then(Value::as_str) {
            Some("COMPLETED") => break,
            Some("IN_QUEUE") => stage("waiting in the service's queue", None),
            _ => stage("the service is at work", None),
        }
    }
    let result = http.get("reading the result", &response_url)?;
    // (A failure is a finished request with an error in it.)
    if let Some(error) = result.get("error").filter(|e| !e.is_null()) {
        return Err(ToolError::other(format!("the service could not do it: {error}")));
    }
    let url = dig(&result, service.result.as_deref().unwrap_or("video"))
        .ok_or_else(|| ToolError::other(format!("the service's answer has no video: {}", brief(&result))))?;
    http.fetch(url, got, cancel, stage)
}

/// Runway (https://docs.dev.runwayml.com): an upload that lasts a day, a task, its output.
fn runway(
    service: &Service,
    sent: &Path,
    got: &Path,
    sentence: &str,
    cancel: &Cancel,
    stage: Stage<'_>,
) -> Result<(), ToolError> {
    let http = Http::new(vec![
        ("Authorization".into(), format!("Bearer {}", key_of(service)?)),
        ("X-Runway-Version".into(), "2024-11-06".into()),
    ]);
    let base =
        service.base_url.as_deref().unwrap_or("https://api.dev.runwayml.com/v1").trim_end_matches('/').to_owned();
    let what = "sending the part";
    stage(what, None);
    let bytes = std::fs::read(sent)?;
    let begun = http.post(what, &format!("{base}/uploads"), &json!({"filename": "part.mp4", "type": "ephemeral"}))?;
    let fields: Vec<(String, String)> = begun
        .get("fields")
        .and_then(Value::as_object)
        .map(|fields| fields.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect())
        .unwrap_or_default();
    let (content_type, body) = form(&fields, "file", "part.mp4", &bytes);
    http.put_bytes(what, true, text(&begun, "uploadUrl", what)?, &[("Content-Type", &content_type)], &body)?;
    let video = text(&begun, "runwayUri", what)?.to_owned();
    drop((bytes, body));

    let what = "asking the service";
    let mut fields = service.fields(&video, "videoUri", "promptText", sentence);
    fields.insert("model".into(), json!(service.model));
    let asked = http.post(what, &format!("{base}/video_to_video"), &Value::Object(fields))?;
    let task = format!("{base}/tasks/{}", text(&asked, "id", what)?);
    let since = std::time::Instant::now();
    loop {
        if let Err(e) = pause(cancel, since) {
            let mut request = http.agent.delete(&task);
            for (name, value) in &http.headers {
                request = request.header(name, value);
            }
            let _ = request.call();
            return Err(e);
        }
        let status = http.get("waiting for the service", &task)?;
        match status.get("status").and_then(Value::as_str) {
            Some("SUCCEEDED") => {
                let url = dig(&status, service.result.as_deref().unwrap_or("output")).ok_or_else(|| {
                    ToolError::other(format!("the service's answer has no video: {}", brief(&status)))
                })?;
                return http.fetch(url, got, cancel, stage);
            }
            Some("FAILED") | Some("CANCELLED") => {
                let why = status.get("failure").and_then(Value::as_str).unwrap_or("no reason given");
                return Err(ToolError::other(format!("the service could not do it: {why}")));
            }
            Some("RUNNING") => {
                let done = status.get("progress").and_then(Value::as_f64).map(|p| p.clamp(0.0, 1.0) as f32);
                stage("the service is at work", done)
            }
            _ => stage("waiting in the service's queue", None),
        }
    }
}

/// Replicate (https://replicate.com/docs/reference/http): a file, a prediction, its output.
fn replicate(
    service: &Service,
    sent: &Path,
    got: &Path,
    sentence: &str,
    cancel: &Cancel,
    stage: Stage<'_>,
) -> Result<(), ToolError> {
    let http = Http::new(vec![("Authorization".into(), format!("Bearer {}", key_of(service)?))]);
    let base = service.base_url.as_deref().unwrap_or("https://api.replicate.com/v1").trim_end_matches('/').to_owned();
    let what = "sending the part";
    stage(what, None);
    let bytes = std::fs::read(sent)?;
    let (content_type, body) = form(&[], "content", "part.mp4", &bytes);
    let headers: Vec<(&str, &str)> = http
        .headers
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .chain([("Content-Type", content_type.as_str())])
        .collect();
    let file = http.put_bytes(what, true, &format!("{base}/files"), &headers, &body)?;
    let video =
        dig(&file, "urls.get").ok_or_else(|| failed(what, "the answer has no address for the file"))?.to_owned();
    drop((bytes, body));

    let what = "asking the service";
    let fields = service.fields(&video, "video", "prompt", sentence);
    // A model by its name, or one version of a model as `owner/name:version`.
    let asked = match service.model.split_once(':') {
        Some((_, version)) => {
            http.post(what, &format!("{base}/predictions"), &json!({"version": version, "input": fields}))?
        }
        None => http.post(what, &format!("{base}/models/{}/predictions", service.model), &json!({"input": fields}))?,
    };
    let ask = dig(&asked, "urls.get").ok_or_else(|| failed(what, "the answer does not say where to ask"))?.to_owned();
    let since = std::time::Instant::now();
    let mut status = asked;
    loop {
        match status.get("status").and_then(Value::as_str) {
            Some("succeeded") => {
                let url = dig(&status, service.result.as_deref().unwrap_or("output")).ok_or_else(|| {
                    ToolError::other(format!("the service's answer has no video: {}", brief(&status)))
                })?;
                return http.fetch(url, got, cancel, stage);
            }
            Some("failed") | Some("canceled") | Some("aborted") => {
                let why = status.get("error").map(|e| e.to_string()).unwrap_or_else(|| "no reason given".into());
                return Err(ToolError::other(format!("the service could not do it: {why}")));
            }
            Some("processing") => stage("the service is at work", None),
            _ => stage("waiting in the service's queue", None),
        }
        if let Err(e) = pause(cancel, since) {
            if let Some(url) = dig(&status, "urls.cancel") {
                let _ = http.post("cancelling", url, &json!({}));
            }
            return Err(e);
        }
        status = http.get("waiting for the service", &ask)?;
    }
}

/// The beginning of an answer, for an error that shows what came instead of what was expected.
fn brief(value: &Value) -> String {
    value.to_string().chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    fn write(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("ai.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn the_example_is_a_file_that_can_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let services = services(&write(dir.path(), EXAMPLE)).unwrap();
        assert_eq!(
            services.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["fal: Wan 2.7 edit", "Runway: Aleph", "A program of mine"]
        );
        assert_eq!(services[0].goes_to(), "fal.ai (model fal-ai/wan/v2.7/edit-video)");
        assert!(services[2].is_local() && !services[0].is_local());
        // What the model takes besides, the video and the sentence, in one request.
        let fields = services[0].fields("https://x/part.mp4", "video_url", "prompt", "storm");
        assert_eq!(
            Value::Object(fields),
            json!({"resolution": "1080p", "video_url": "https://x/part.mp4", "prompt": "storm"})
        );
    }

    #[test]
    fn no_file_is_no_service_and_a_wrong_one_says_what_is_wrong() {
        let dir = tempfile::tempdir().unwrap();
        assert!(services(&dir.path().join("none.toml")).unwrap().is_empty());
        let no_key = write(dir.path(), "[[service]]\nname = \"a\"\nkind = \"fal\"\nmodel = \"m\"\n");
        assert!(services(&no_key).unwrap_err().contains("key"));
        let no_name = write(dir.path(), "[[service]]\nname = \" \"\nkind = \"fal\"\nmodel = \"m\"\nkey = \"k\"\n");
        assert!(services(&no_name).unwrap_err().contains("name"));
        let no_out = write(dir.path(), "[[service]]\nname = \"a\"\nkind = \"command\"\ncommand = [\"x\", \"{in}\"]\n");
        assert!(services(&no_out).unwrap_err().contains("{out}"));
        let unknown =
            write(dir.path(), "[[service]]\nname = \"a\"\nkind = \"fal\"\nmodel = \"m\"\nkey = \"k\"\nmodle = 1\n");
        assert!(services(&unknown).unwrap_err().contains("modle"));
    }

    #[test]
    fn services_written_by_the_window_are_read_again_and_only_by_their_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/ai.toml");
        let mut all = presets();
        for service in &mut all {
            assert!(service.check().unwrap_err().contains("key"), "{}", service.name);
            service.key = Some("secret".to_owned());
        }
        let mut own = Service::blank(Kind::Command);
        own.name = "Mine".to_owned();
        own.command = command_words("my-model --in {in} --out {out} --say \"{prompt} now\" \"\"");
        assert_eq!(own.command, ["my-model", "--in", "{in}", "--out", "{out}", "--say", "{prompt} now", ""]);
        assert_eq!(command_words(&command_line(&own.command)), own.command);
        all.push(own);
        save(&path, &all).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(services(&path).unwrap() == all);
        // Written over what was there, whole.
        save(&path, &all[..1]).unwrap();
        assert_eq!(services(&path).unwrap().len(), 1);
    }

    #[test]
    fn a_changed_part_is_known_again_by_its_name() {
        let label = "my-clip changed 0a1b2c3d 3600-90";
        assert_eq!(change_origin(label), Some(("my-clip", 3600)));
        assert_eq!(change_origin("my-clip changed everything 3600-90"), None);
        assert_eq!(change_origin("what changed 12"), None);
        assert_eq!(crate::analysis::sky_origin(label), None);
    }

    #[test]
    fn the_address_of_the_video_is_found_in_the_shapes_services_answer_in() {
        assert_eq!(dig(&json!({"video": {"url": "u"}}), "video"), Some("u"));
        assert_eq!(dig(&json!({"output": ["u", "v"]}), "output"), Some("u"));
        assert_eq!(dig(&json!({"output": "u"}), "output"), Some("u"));
        assert_eq!(dig(&json!({"data": {"videos": [{"url": "u"}]}}), "data.videos.0"), Some("u"));
        assert_eq!(dig(&json!({"output": null}), "output"), None);
    }

    /// A service that is only this test: answers each request from
    /// `answers` by its path, and keeps what it was asked.
    fn service_of(
        answers: impl Fn(&str, &str, &str) -> (u16, String) + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<(String, String, String)>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (told, own) = (seen.clone(), base.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut parts = first.split_whitespace();
                let (method, path) = (parts.next().unwrap_or("").to_owned(), parts.next().unwrap_or("").to_owned());
                let (mut length, mut auth) = (0usize, String::new());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let lower = line.to_ascii_lowercase();
                    if let Some(n) = lower.strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap();
                    }
                    if lower.starts_with("authorization:") {
                        auth = line["authorization:".len()..].trim().to_owned();
                    }
                    if line.trim().is_empty() {
                        break;
                    }
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).unwrap();
                let body = String::from_utf8_lossy(&body).into_owned();
                told.lock().unwrap().push((format!("{method} {path}"), auth, body.clone()));
                let (status, answer) = answers(&format!("{method} {path}"), &own, &body);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
            }
        });
        (base, seen)
    }

    fn one(dir: &Path, kind: &str, base: &str, more: &str) -> Service {
        let text = format!(
            "[[service]]\nname = \"t\"\nkind = \"{kind}\"\nmodel = \"own/model\"\nkey = \"secret\"\nbase_url = \"{base}\"\n{more}"
        );
        services(&write(dir, &text)).unwrap().remove(0)
    }

    #[test]
    fn a_part_goes_through_a_fal_queue_and_comes_back() {
        let asked_status = Arc::new(Mutex::new(0));
        let (base, seen) = service_of(move |request, own, _| {
            match request {
            "POST /storage/upload/initiate?storage_type=gcs" => {
                (200, json!({"upload_url": format!("{own}/put/part.mp4"), "file_url": "https://files/part.mp4"}).to_string())
            }
            "PUT /put/part.mp4" => (200, String::new()),
            "POST /own/model" => (
                200,
                json!({"request_id": "r1", "status_url": format!("{own}/own/requests/r1/status"), "response_url": format!("{own}/own/requests/r1")})
                    .to_string(),
            ),
            "GET /own/requests/r1/status" => {
                let mut n = asked_status.lock().unwrap();
                *n += 1;
                (200, json!({"status": if *n < 2 { "IN_QUEUE" } else { "COMPLETED" }}).to_string())
            }
            "GET /own/requests/r1" => (200, json!({"video": {"url": format!("{own}/result.mp4")}}).to_string()),
            "GET /result.mp4" => (200, "the video".to_owned()),
            _ => (404, "{}".to_owned()),
        }
        });
        let dir = tempfile::tempdir().unwrap();
        let service = one(
            dir.path(),
            "fal",
            &base,
            "prompt = \"In the clip: {sentence}\"\n[service.input]\nresolution = \"720p\"\n",
        );
        let (sent, got) = (dir.path().join("sent.mp4"), dir.path().join("got.mp4"));
        std::fs::write(&sent, b"the part").unwrap();
        let mut stages = Vec::new();
        fal(&service, &sent, &got, "storm clouds", &Cancel::new(), &mut |stage, _| stages.push(stage)).unwrap();
        assert_eq!(std::fs::read_to_string(&got).unwrap(), "the video");
        let seen = seen.lock().unwrap();
        // The key goes to the service, not to where the file is put.
        let auth_of =
            |request: &str| seen.iter().find(|(r, ..)| r == request).map(|(_, auth, _)| auth.clone()).unwrap();
        assert_eq!(auth_of("POST /own/model"), "Key secret");
        assert_eq!(auth_of("PUT /put/part.mp4"), "");
        assert_eq!(seen.iter().find(|(r, ..)| r == "PUT /put/part.mp4").unwrap().2, "the part");
        let request: Value =
            serde_json::from_str(&seen.iter().find(|(r, ..)| r == "POST /own/model").unwrap().2).unwrap();
        assert_eq!(
            request,
            json!({"resolution": "720p", "video_url": "https://files/part.mp4", "prompt": "In the clip: storm clouds"})
        );
        assert!(stages.contains(&"waiting in the service's queue"));
    }

    #[test]
    fn what_a_service_refuses_is_said_in_its_words() {
        let (base, _) = service_of(|_, _, _| (401, json!({"detail": "no such key"}).to_string()));
        let dir = tempfile::tempdir().unwrap();
        let (sent, got) = (dir.path().join("sent.mp4"), dir.path().join("got.mp4"));
        std::fs::write(&sent, b"the part").unwrap();
        for kind in ["fal", "runway", "replicate"] {
            let service = one(dir.path(), kind, &base, "");
            let result = match kind {
                "fal" => fal(&service, &sent, &got, "x", &Cancel::new(), &mut |_, _| {}),
                "runway" => runway(&service, &sent, &got, "x", &Cancel::new(), &mut |_, _| {}),
                _ => replicate(&service, &sent, &got, "x", &Cancel::new(), &mut |_, _| {}),
            };
            let said = result.unwrap_err().to_string();
            assert!(said.contains("401") && said.contains("no such key"), "{kind}: {said}");
            assert!(!said.contains("secret"), "{kind}: the key is in the error");
        }
    }

    #[test]
    fn a_part_goes_through_runway_and_replicate() {
        let (base, seen) = service_of(|request, own, _| match request {
            // Runway
            "POST /uploads" => (
                200,
                json!({"uploadUrl": format!("{own}/bucket"), "fields": {"policy": "p"}, "runwayUri": "runway://f1"})
                    .to_string(),
            ),
            "POST /bucket" => (204, String::new()),
            "POST /video_to_video" => (200, json!({"id": "t1"}).to_string()),
            "GET /tasks/t1" => {
                (200, json!({"status": "SUCCEEDED", "output": [format!("{own}/result.mp4")]}).to_string())
            }
            // Replicate
            "POST /files" => (201, json!({"urls": {"get": "https://files/f2"}}).to_string()),
            "POST /models/own/model/predictions" => {
                (201, json!({"status": "starting", "urls": {"get": format!("{own}/predictions/p1")}}).to_string())
            }
            "GET /predictions/p1" => {
                (200, json!({"status": "succeeded", "output": format!("{own}/result.mp4")}).to_string())
            }
            "GET /result.mp4" => (200, "the video".to_owned()),
            _ => (404, "{}".to_owned()),
        });
        let dir = tempfile::tempdir().unwrap();
        let (sent, got) = (dir.path().join("sent.mp4"), dir.path().join("got.mp4"));
        std::fs::write(&sent, b"the part").unwrap();
        runway(&one(dir.path(), "runway", &base, ""), &sent, &got, "storm", &Cancel::new(), &mut |_, _| {}).unwrap();
        assert_eq!(std::fs::read_to_string(&got).unwrap(), "the video");
        std::fs::remove_file(&got).unwrap();
        replicate(&one(dir.path(), "replicate", &base, ""), &sent, &got, "storm", &Cancel::new(), &mut |_, _| {})
            .unwrap();
        assert_eq!(std::fs::read_to_string(&got).unwrap(), "the video");
        let seen = seen.lock().unwrap();
        let body_of = |request: &str| seen.iter().find(|(r, ..)| r == request).map(|(.., body)| body.clone()).unwrap();
        let asked: Value = serde_json::from_str(&body_of("POST /video_to_video")).unwrap();
        assert_eq!(asked, json!({"model": "own/model", "videoUri": "runway://f1", "promptText": "storm"}));
        // The form carries the fields that were handed out, then the file.
        let form = body_of("POST /bucket");
        assert!(
            form.contains("name=\"policy\"\r\n\r\np\r\n")
                && form.contains("name=\"file\"; filename=\"part.mp4\"")
                && form.contains("the part")
        );
        let asked: Value = serde_json::from_str(&body_of("POST /models/own/model/predictions")).unwrap();
        assert_eq!(asked, json!({"input": {"video": "https://files/f2", "prompt": "storm"}}));
    }

    #[test]
    fn a_command_is_given_the_files_and_the_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let text = "[[service]]\nname = \"c\"\nkind = \"command\"\ncommand = [\"sh\", \"-c\", \"echo progress 0.5; printf '%s' \\\"$2\\\" > \\\"$1\\\"; cat \\\"$0\\\" >> \\\"$1\\\"\", \"{in}\", \"{out}\", \"{prompt}\"]\n";
        let service = services(&write(dir.path(), text)).unwrap().remove(0);
        let (sent, got) = (dir.path().join("sent.mp4"), dir.path().join("got.mp4"));
        std::fs::write(&sent, b" of the part").unwrap();
        let mut seen = Vec::new();
        run_command(&service, &sent, &got, "storm", &Cancel::new(), &mut |_, done| seen.push(done)).unwrap();
        assert_eq!(std::fs::read_to_string(&got).unwrap(), "storm of the part");
        assert!(seen.contains(&Some(0.5)));
    }
}
