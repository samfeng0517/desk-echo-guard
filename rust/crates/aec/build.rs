//! Builds the vendored SpeexDSP 1.2.1 subset (echo canceller, preprocessor,
//! KISS FFT; BSD licence, see vendor/speexdsp/COPYING) when the `speex`
//! feature is on.  `config.h` and `speexdsp_config_types.h` are hand-written
//! replacements for the autotools output, so MSVC builds work too.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "speex")]
    build_speex();
}

#[cfg(feature = "speex")]
fn build_speex() {
    let root = std::path::Path::new("vendor/speexdsp");
    println!("cargo:rerun-if-changed={}", root.display());
    let src = root.join("libspeexdsp");
    let mut b = cc::Build::new();
    b.include(root)
        .include(root.join("include"))
        .include(&src)
        .define("HAVE_CONFIG_H", None)
        .warnings(false);
    for f in ["mdf.c", "preprocess.c", "filterbank.c", "fftwrap.c", "kiss_fft.c", "kiss_fftr.c"] {
        b.file(src.join(f));
    }
    if b.get_compiler().is_like_msvc() {
        b.define("_CRT_SECURE_NO_WARNINGS", None);
    }
    b.compile("speexdsp_eg");
}
