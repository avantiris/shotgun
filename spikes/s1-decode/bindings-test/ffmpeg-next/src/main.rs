extern crate ffmpeg_next as ffmpeg;
use ffmpeg::format::input;
use ffmpeg::media::Type;
fn main() -> Result<(), ffmpeg::Error> {
    ffmpeg::init()?;
    let path = std::env::args().nth(1).unwrap();
    let mut ictx = input(&path)?;
    let st = ictx.streams().best(Type::Video).unwrap();
    let idx = st.index();
    let ctx = ffmpeg::codec::context::Context::from_parameters(st.parameters())?;
    let mut dec = ctx.decoder().video()?;
    let mut n = 0;
    let (mut w, mut h, mut fmt) = (0, 0, None);
    let mut frame = ffmpeg::util::frame::video::Video::empty();
    for (s, pkt) in ictx.packets() {
        if s.index() != idx { continue; }
        dec.send_packet(&pkt)?;
        while dec.receive_frame(&mut frame).is_ok() {
            n += 1;
            (w, h, fmt) = (frame.width(), frame.height(), Some(frame.format()));
        }
        if n >= 30 { break; }
    }
    println!("ffmpeg-next ok: decoded {} frames {}x{} fmt {:?}; avcodec_version={}", n, w, h, fmt, unsafe { ffmpeg::ffi::avcodec_version() } >> 16);
    Ok(())
}
