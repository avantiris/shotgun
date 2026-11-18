//! S4 spike: offer one media file on the Wayland clipboard under several MIME types at once
//! (which `wl-copy` cannot do: it offers exactly one type per invocation).
//! Usage: clip-multi <absolute-file> <mime>      e.g. clip-multi /x/a.gif image/gif
//! Offers: text/uri-list, x-special/gnome-copied-files, <mime> (raw bytes), text/plain (path).
//! Stays in the foreground and serves the selection until another client replaces it.
use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};

fn main() {
    let mut a = std::env::args().skip(1);
    let path = a.next().expect("file");
    let mime = a.next().expect("mime");
    assert!(path.starts_with('/'), "absolute path required");
    let bytes = std::fs::read(&path).expect("read file");
    // NOTE: a real implementation must percent-encode the path for the URI.
    let uri = format!("file://{path}");
    let src = |data: Vec<u8>, ty: &str| MimeSource {
        source: Source::Bytes(data.into_boxed_slice()),
        mime_type: MimeType::Specific(ty.to_string()),
    };
    let sources = vec![
        src(format!("{uri}\r\n").into_bytes(), "text/uri-list"),
        src(format!("copy\n{uri}").into_bytes(), "x-special/gnome-copied-files"),
        src(bytes, &mime),
        src(path.clone().into_bytes(), "text/plain;charset=utf-8"),
    ];
    let mut opts = Options::new();
    opts.foreground(true);
    opts.copy_multi(sources).expect("copy_multi");
}
