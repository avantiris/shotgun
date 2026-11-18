//! Synthetic input through /dev/uinput (real evdev key codes, so the compositor
//! applies the configured xkb layout exactly as for a hardware keyboard).
//!
//!   inject [--require-class CLASS] TOKENS...
//!   tokens: t<code> tap key, d<code> press, u<code> release, s<ms> sleep,
//!           m<dx>,<dy> relative pointer motion, c<x>,<y> absolute warp (hyprctl). Codes are Linux evdev codes
//!           (KEY_Y=21, KEY_Z=44, BTN_LEFT=272, ...).
//! With --require-class, every press is skipped unless the focused Hyprland
//! window has that class (so keys never land in another application).

use std::io::Write;
use std::os::fd::AsRawFd;
use std::time::Duration;

const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_SET_RELBIT: libc::c_ulong = 0x4004_5566;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;

fn emit(f: &mut std::fs::File, ty: u16, code: u16, value: i32) {
    let mut b = [0u8; 24];
    b[16..18].copy_from_slice(&ty.to_ne_bytes());
    b[18..20].copy_from_slice(&code.to_ne_bytes());
    b[20..24].copy_from_slice(&value.to_ne_bytes());
    f.write_all(&b).unwrap();
}
fn syn(f: &mut std::fs::File) {
    emit(f, 0, 0, 0);
}

fn focused_class() -> String {
    let out = std::process::Command::new("hyprctl").arg("activewindow").output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("class: ").map(str::to_string))
        .unwrap_or_default()
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut require = None;
    if args.first().map(String::as_str) == Some("--require-class") {
        require = Some(args[1].clone());
        args.drain(0..2);
    }
    let mut f = std::fs::OpenOptions::new().write(true).open("/dev/uinput").expect("open /dev/uinput");
    let fd = f.as_raw_fd();
    unsafe {
        libc::ioctl(fd, UI_SET_EVBIT, 1 as libc::c_int); // EV_KEY
        libc::ioctl(fd, UI_SET_EVBIT, 2 as libc::c_int); // EV_REL
        for k in 1..249 {
            libc::ioctl(fd, UI_SET_KEYBIT, k as libc::c_int);
        }
        libc::ioctl(fd, UI_SET_KEYBIT, 272 as libc::c_int); // BTN_LEFT
        libc::ioctl(fd, UI_SET_RELBIT, 0 as libc::c_int);
        libc::ioctl(fd, UI_SET_RELBIT, 1 as libc::c_int);
        let mut setup = [0u8; 92];
        setup[0..2].copy_from_slice(&3u16.to_ne_bytes()); // BUS_USB
        setup[2..4].copy_from_slice(&0x1234u16.to_ne_bytes());
        setup[4..6].copy_from_slice(&0x5678u16.to_ne_bytes());
        let name = b"shotgun-s2-inject";
        setup[8..8 + name.len()].copy_from_slice(name);
        assert_eq!(libc::ioctl(fd, UI_DEV_SETUP, setup.as_ptr()), 0);
        assert_eq!(libc::ioctl(fd, UI_DEV_CREATE), 0);
    }
    std::thread::sleep(Duration::from_millis(1200)); // let the compositor pick the device up
    let mut held: Vec<u16> = Vec::new();
    for tok in &args {
        let (kind, rest) = tok.split_at(1);
        let ok = |what: &str| match &require {
            Some(c) if &focused_class() != c => {
                println!("inject: SKIP {what}: focused class is {:?}, need {c:?}", focused_class());
                false
            }
            _ => true,
        };
        match kind {
            "s" => std::thread::sleep(Duration::from_millis(rest.parse().unwrap())),
            "t" => {
                let c: u16 = rest.parse().unwrap();
                if ok(tok) {
                    emit(&mut f, 1, c, 1);
                    syn(&mut f);
                    std::thread::sleep(Duration::from_millis(30));
                    emit(&mut f, 1, c, 0);
                    syn(&mut f);
                    std::thread::sleep(Duration::from_millis(40));
                }
            }
            "d" => {
                let c: u16 = rest.parse().unwrap();
                if ok(tok) {
                    emit(&mut f, 1, c, 1);
                    syn(&mut f);
                    held.push(c);
                    std::thread::sleep(Duration::from_millis(30));
                }
            }
            "u" => {
                let c: u16 = rest.parse().unwrap();
                emit(&mut f, 1, c, 0);
                syn(&mut f);
                held.retain(|&h| h != c);
                std::thread::sleep(Duration::from_millis(30));
            }
            "c" => {
                // absolute cursor warp through the compositor: c<x>,<y>
                let (x, y) = rest.split_once(',').unwrap();
                let _ = std::process::Command::new("hyprctl").args(["dispatch", "movecursor", x, y]).output();
                std::thread::sleep(Duration::from_millis(60));
            }
            "m" => {
                let (dx, dy) = rest.split_once(',').unwrap();
                emit(&mut f, 2, 0, dx.parse().unwrap());
                emit(&mut f, 2, 1, dy.parse().unwrap());
                syn(&mut f);
                std::thread::sleep(Duration::from_millis(12));
            }
            _ => panic!("bad token {tok}"),
        }
    }
    for c in held {
        emit(&mut f, 1, c, 0);
        syn(&mut f);
    }
    std::thread::sleep(Duration::from_millis(100));
    unsafe { libc::ioctl(fd, UI_DEV_DESTROY) };
}
