fn main() {
    println!("cargo:rerun-if-changed=src/visualization_macos.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/visualization_macos.m")
            .flag("-fobjc-arc")
            .compile("kiln_visualization");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=WebKit");
    }
}
