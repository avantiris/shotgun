use rsmpeg::ffi;
fn main() {
    println!("rsmpeg ok: avcodec_version major={}", unsafe { ffi::avcodec_version() } >> 16);
}
