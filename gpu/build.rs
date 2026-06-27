// compile the cuda shim with nvcc (via cc's cuda mode) and link nvjpeg2000 + cudart.
// runs only when this crate is built — i.e. `cli --features gpu` on a cuda box. the
// cuda toolkit + the nvjpeg2000 dev package (libnvjpeg2k0-dev-cuda-12) must be
// installed; cloud-init-gpu.yaml provisions both. CUDA_PATH overrides the toolkit
// root; NVJPEG2K_INCLUDE/NVJPEG2K_LIB override the (multiarch) nvjpeg2000 paths.
fn main() {
    let cuda = std::env::var("CUDA_PATH").unwrap_or_else(|_| "/usr/local/cuda".into());
    // ubuntu/debian put nvjpeg2000 in a versioned multiarch dir, not in CUDA's tree.
    let j_inc = std::env::var("NVJPEG2K_INCLUDE").unwrap_or_else(|_| "/usr/include/libnvjpeg2k/12".into());
    let j_lib = std::env::var("NVJPEG2K_LIB").unwrap_or_else(|_| "/usr/lib/x86_64-linux-gnu/libnvjpeg2k/12".into());

    cc::Build::new().cuda(true).file("src/shim.cu")
        .include(format!("{cuda}/include")).include(&j_inc).compile("s2gpu");

    // link-search/link-lib propagate from this build script to the final binary's link;
    // runtime resolution of libcudart/libnvjpeg2k.so.0 (dirs not on the default ld path)
    // is handled by LD_LIBRARY_PATH from /etc/profile.d/cuda.sh, which box.sh sources.
    for sub in ["lib64", "targets/x86_64-linux/lib"] {
        println!("cargo:rustc-link-search=native={cuda}/{sub}");
    }
    println!("cargo:rustc-link-search=native={j_lib}");
    println!("cargo:rustc-link-lib=nvjpeg2k");
    println!("cargo:rustc-link-lib=cudart");
    println!("cargo:rerun-if-changed=src/shim.cu");
    println!("cargo:rerun-if-env-changed=CUDA_PATH");
}
