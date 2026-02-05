use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let presets_dir = manifest_dir.join("../../presets");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dest = out_dir.join("embedded_presets.rs");

    let mut entries: Vec<PathBuf> = Vec::new();
    match fs::read_dir(&presets_dir) {
        Ok(read_dir) => {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
                    entries.push(path);
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => panic!("failed to read presets dir: {err}"),
    }

    entries.sort();

    let mut output = String::new();
    output.push_str("pub const EMBEDDED_PRESETS: &[EmbeddedPreset] = &[\n");
    for path in &entries {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");
        let content = fs::read_to_string(path).unwrap_or_default();
        output.push_str("    EmbeddedPreset { name: ");
        output.push_str(&format!("{name:?}"));
        output.push_str(", content: ");
        output.push_str(&format!("{content:?}"));
        output.push_str(" },\n");
    }
    output.push_str("];\n");

    fs::write(dest, output).expect("failed to write embedded presets");
    println!("cargo:rerun-if-changed={}", presets_dir.display());
}
