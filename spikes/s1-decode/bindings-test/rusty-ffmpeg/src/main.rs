use rusty_ffmpeg::ffi;
fn main() {
    println!("rusty_ffmpeg ok: avcodec_version major={}", unsafe { ffi::avcodec_version() } >> 16);
}
