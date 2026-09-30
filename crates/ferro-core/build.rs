fn main() {
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=FERRO_BUILD_TARGET={target}");
    println!("cargo:rerun-if-env-changed=TARGET");
}
