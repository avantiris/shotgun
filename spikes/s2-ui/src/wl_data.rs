//! Wayland drag-and-drop + clipboard paste next to winit.
//!
//! winit 0.30 has no wl_data_device handling on Wayland (no DroppedFile, no
//! clipboard). This module opens a second event queue on winit's own
//! `wl_display`, binds `wl_seat` + `wl_data_device_manager` there, and handles
//! drag-and-drop and the selection (clipboard) on a dedicated thread.
//! Same pattern as smithay-clipboard, extended with DnD and arbitrary MIME types.

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};

use wayland_client::backend::{Backend, ObjectId};
use wayland_client::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{DndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_registry::{self, WlRegistry},
    wl_seat::WlSeat,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};

#[derive(Debug, Clone)]
pub enum DataEvent {
    /// A drag entered our surface; the MIME types it offers.
    DragEnter { mimes: Vec<String>, on_our_surface: bool },
    DragMotion { x: f64, y: f64 },
    DragLeave,
    /// Dropped payload, fully read.
    Drop { mime: String, data: Vec<u8>, x: f64, y: f64 },
    /// The clipboard content changed (only delivered while we have keyboard focus).
    Selection { mimes: Vec<String> },
    /// Result of `WlData::paste`.
    Paste { mime: String, data: Vec<u8> },
    PasteEmpty,
}

/// Parse a `text/uri-list` payload into local paths / other URIs.
pub fn parse_uri_list(data: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(data)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.strip_prefix("file://") {
            Some(p) => percent_decode(p),
            None => l.to_string(),
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Preference order when something is dropped or pasted.
pub const PREFERRED: [&str; 9] = [
    "text/uri-list",
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
];

pub fn pick_mime(mimes: &[String]) -> Option<String> {
    PREFERRED.iter().find(|p| mimes.iter().any(|m| m == *p)).map(|p| p.to_string())
}

type Sink = Arc<dyn Fn(DataEvent) + Send + Sync>;
type Selection = Arc<Mutex<Option<(WlDataOffer, Vec<String>)>>>;

#[derive(Clone)]
pub struct WlData {
    conn: Connection,
    selection: Selection,
    sink: Sink,
    stop: Arc<Mutex<Option<(std::io::PipeWriter, std::thread::JoinHandle<()>)>>>,
}

struct State {
    conn: Connection,
    our_surface: usize,
    sink: Sink,
    selection: Selection,
    seat: Option<WlSeat>,
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    offers: HashMap<ObjectId, Vec<String>>,
    drag: Option<(WlDataOffer, Option<String>)>,
    pos: (f64, f64),
}

impl WlData {
    /// `display` and `surface` are the raw `wl_display*` / `wl_surface*` from
    /// raw-window-handle (winit `Window::display_handle()/window_handle()`).
    ///
    /// # Safety
    /// `display` must be a valid wl_display that outlives the returned object's thread.
    pub unsafe fn spawn(
        display: *mut std::ffi::c_void,
        surface: *mut std::ffi::c_void,
        sink: impl Fn(DataEvent) + Send + Sync + 'static,
    ) -> WlData {
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let conn = Connection::from_backend(backend);
        let mut queue = conn.new_event_queue::<State>();
        let qh = queue.handle();
        let sink: Sink = Arc::new(sink);
        let selection: Selection = Arc::new(Mutex::new(None));
        let mut state = State {
            conn: conn.clone(),
            our_surface: surface as usize,
            sink: sink.clone(),
            selection: selection.clone(),
            seat: None,
            manager: None,
            device: None,
            offers: HashMap::new(),
            drag: None,
            pos: (0.0, 0.0),
        };
        conn.display().get_registry(&qh, ());
        let (stop_r, stop_w) = std::io::pipe().unwrap();
        let join = std::thread::Builder::new()
            .name("wl-data".into())
            .spawn(move || {
                use std::os::fd::AsRawFd;
                // Manual poll loop (instead of blocking_dispatch) so the thread can be
                // stopped before winit disconnects the display it is reading from.
                loop {
                    if queue.dispatch_pending(&mut state).is_err() || queue.flush().is_err() {
                        break;
                    }
                    let Some(guard) = queue.prepare_read() else { continue };
                    let mut fds = [
                        libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
                        libc::pollfd { fd: stop_r.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                    ];
                    let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
                    if fds[1].revents != 0 {
                        break; // guard dropped: cancels the read
                    }
                    if n > 0 && fds[0].revents & libc::POLLIN != 0 {
                        let _ = guard.read(); // WouldBlock is fine: another thread read first
                    } else if fds[0].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                        break;
                    }
                }
            })
            .unwrap();
        WlData {
            conn,
            selection,
            sink,
            stop: Arc::new(Mutex::new(Some((stop_w, join)))),
        }
    }

    /// Stop the thread. MUST be called before the winit event loop (which owns
    /// the wl_display) is dropped, otherwise the thread reads a freed display.
    pub fn shutdown(&self) {
        if let Some((w, join)) = self.stop.lock().unwrap().take() {
            use std::io::Write;
            let _ = (&w).write_all(&[1]);
            let _ = join.join();
        }
        if let Some((offer, _)) = self.selection.lock().unwrap().take() {
            offer.destroy();
        }
    }

    /// Read the current clipboard selection (best MIME type). Callable from any
    /// thread; the result arrives through the sink as `Paste`/`PasteEmpty`.
    pub fn paste(&self) {
        let guard = self.selection.lock().unwrap();
        let Some((offer, mimes)) = guard.as_ref() else {
            (self.sink)(DataEvent::PasteEmpty);
            return;
        };
        let Some(mime) = pick_mime(mimes) else {
            (self.sink)(DataEvent::PasteEmpty);
            return;
        };
        let (rd, wr) = std::io::pipe().unwrap();
        offer.receive(mime.clone(), wr.as_fd());
        let _ = self.conn.flush();
        drop(wr);
        drop(guard);
        let sink = self.sink.clone();
        std::thread::spawn(move || {
            let mut rd = rd;
            let mut data = Vec::new();
            let _ = rd.read_to_end(&mut data);
            sink(DataEvent::Paste { mime, data });
        });
    }
}

impl State {
    fn try_get_device(&mut self, qh: &QueueHandle<Self>) {
        if self.device.is_none() {
            if let (Some(seat), Some(mgr)) = (&self.seat, &self.manager) {
                self.device = Some(mgr.get_data_device(seat, qh, ()));
            }
        }
    }
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        st: &mut Self,
        reg: &WlRegistry,
        ev: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            match interface.as_str() {
                // Only the first seat. Version 1 is enough: we need no seat events.
                "wl_seat" if st.seat.is_none() => {
                    st.seat = Some(reg.bind::<WlSeat, _, _>(name, 1, qh, ()));
                }
                "wl_data_device_manager" => {
                    st.manager =
                        Some(reg.bind::<WlDataDeviceManager, _, _>(name, version.min(3), qh, ()));
                }
                _ => {}
            }
            st.try_get_device(qh);
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(_: &mut Self, _: &WlSeat, _: <WlSeat as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        st: &mut Self,
        offer: &WlDataOffer,
        ev: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = ev {
            st.offers.entry(offer.id()).or_default().push(mime_type);
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        st: &mut Self,
        _dev: &WlDataDevice,
        ev: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wl_data_device::Event as E;
        match ev {
            E::DataOffer { id } => {
                st.offers.insert(id.id(), Vec::new());
            }
            E::Enter { serial, surface, x, y, id } => {
                let Some(offer) = id else { return };
                let mimes = st.offers.get(&offer.id()).cloned().unwrap_or_default();
                let mime = pick_mime(&mimes);
                offer.accept(serial, mime.clone());
                if offer.version() >= 3 {
                    offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Copy);
                }
                st.pos = (x, y);
                let on_our_surface = surface.id().as_ptr() as usize == st.our_surface;
                st.drag = Some((offer, mime));
                (st.sink)(DataEvent::DragEnter { mimes, on_our_surface });
            }
            E::Motion { x, y, .. } => {
                st.pos = (x, y);
                (st.sink)(DataEvent::DragMotion { x, y });
            }
            E::Leave => {
                if let Some((offer, _)) = st.drag.take() {
                    st.offers.remove(&offer.id());
                    offer.destroy();
                }
                (st.sink)(DataEvent::DragLeave);
            }
            E::Drop => {
                let Some((offer, mime)) = st.drag.take() else { return };
                st.offers.remove(&offer.id());
                let Some(mime) = mime else {
                    offer.destroy();
                    return;
                };
                let (rd, wr) = std::io::pipe().unwrap();
                offer.receive(mime.clone(), wr.as_fd());
                let _ = st.conn.flush();
                drop(wr);
                let sink = st.sink.clone();
                let conn = st.conn.clone();
                let (x, y) = st.pos;
                // Read off-thread: the source may write slowly or a lot.
                std::thread::spawn(move || {
                    let mut rd = rd;
                    let mut data = Vec::new();
                    let _ = rd.read_to_end(&mut data);
                    if offer.version() >= 3 {
                        offer.finish();
                    }
                    offer.destroy();
                    let _ = conn.flush();
                    sink(DataEvent::Drop { mime, data, x, y });
                });
            }
            E::Selection { id } => {
                let mut sel = st.selection.lock().unwrap();
                if let Some((old, _)) = sel.take() {
                    st.offers.remove(&old.id());
                    old.destroy();
                }
                let mimes = match id {
                    Some(offer) => {
                        let mimes = st.offers.get(&offer.id()).cloned().unwrap_or_default();
                        *sel = Some((offer, mimes.clone()));
                        mimes
                    }
                    None => Vec::new(),
                };
                drop(sel);
                (st.sink)(DataEvent::Selection { mimes });
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

/// Raw wl_display / wl_surface pointers of a winit window (None when not on Wayland).
pub fn wayland_ptrs(
    window: &(impl raw_window_handle::HasDisplayHandle + raw_window_handle::HasWindowHandle),
) -> Option<(*mut std::ffi::c_void, *mut std::ffi::c_void)> {
    use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
    let d = match window.display_handle().ok()?.as_raw() {
        RawDisplayHandle::Wayland(d) => d.display.as_ptr(),
        _ => return None,
    };
    let s = match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Wayland(w) => w.surface.as_ptr(),
        _ => return None,
    };
    Some((d, s))
}
