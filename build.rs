use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=native/eds.c");
    println!("cargo:rerun-if-changed=native/source-identity.c");
    println!("cargo:rerun-if-changed=data/org.gnome.pigeon.gschema.xml");
    println!("cargo:rerun-if-changed=po/LINGUAS");
    println!("cargo:rerun-if-env-changed=PREFIX");

    let output_directory = PathBuf::from(
        std::env::var_os("OUT_DIR").expect("Cargo must provide an OUT_DIR to the build script"),
    );
    compile_development_schemas(&output_directory);
    compile_message_catalogs(&output_directory);

    let camel = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("camel-1.2")
        .expect("camel-1.2 development files are required");
    let eds = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("libedataserver-1.2")
        .expect("libedataserver-1.2 development files are required");

    let mut build = cc::Build::new();
    build.files(["native/eds.c", "native/source-identity.c"]);

    for include in camel.include_paths.iter().chain(eds.include_paths.iter()) {
        build.include(include);
    }

    build.compile("mail_bridge");
}

fn compile_development_schemas(output_directory: &Path) {
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

fn compile_message_catalogs(output_directory: &Path) {
    let locale_directory = output_directory.join("share/locale");
    if locale_directory.exists() {
        fs::remove_dir_all(&locale_directory)
            .expect("failed to clear the generated locale directory");
    }
    let languages = fs::read_to_string("po/LINGUAS")
        .expect("po/LINGUAS must list the supported translations");

    for language in languages
        .lines()
        .flat_map(|line| {
            let content = line
                .split_once('#')
                .map_or(line, |(content, _comment)| content);
            content.split_whitespace()
        })
    {
        let source = format!("po/{language}.po");
        let destination = locale_directory
            .join(language)
            .join("LC_MESSAGES")
            .join("pigeon.mo");
        fs::create_dir_all(
            destination
                .parent()
                .expect("a message catalog always has a parent directory"),
        )
        .expect("failed to create the development locale directory");
        let status = Command::new("msgfmt")
            .args(["--check", "-o"])
            .arg(&destination)
            .arg(&source)
            .status()
            .expect("GNU msgfmt is required to build Pigeon Mail translations");
        assert!(status.success(), "failed to compile {source}");
        println!("cargo:rerun-if-changed={source}");
    }

    let runtime_prefix = std::env::var_os("PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| output_directory.to_path_buf());
    println!("cargo:rustc-env=PREFIX={}", runtime_prefix.display());
}
