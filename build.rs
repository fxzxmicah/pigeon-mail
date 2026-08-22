use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=native/eds.c");
    println!("cargo:rerun-if-changed=data/org.gnome.pigeon.gschema.xml");

    compile_development_schemas();

    let camel = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("camel-1.2")
        .expect("camel-1.2 development files are required");
    let eds = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("libedataserver-1.2")
        .expect("libedataserver-1.2 development files are required");

    let mut build = cc::Build::new();
    build.file("native/eds.c");

    for include in camel.include_paths.iter().chain(eds.include_paths.iter()) {
        build.include(include);
    }

    build.compile("mail_bridge");
}

fn compile_development_schemas() {
    let output_directory = PathBuf::from(
        std::env::var_os("OUT_DIR").expect("Cargo must provide an OUT_DIR to the build script"),
    );
    let status = Command::new("glib-compile-schemas")
        .arg("--strict")
        .arg("--targetdir")
        .arg(&output_directory)
        .arg("data")
        .status()
        .expect("glib-compile-schemas is required to build Pigeon Mail");

    assert!(
        status.success(),
        "failed to compile the Pigeon Mail GSettings schema"
    );
}
