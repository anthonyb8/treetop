//! Embeds the built web app, `web/dist`, in the binary, so `treetop serve`
//! needs nothing beside it. A checkout where the app has not been built gets
//! a page saying how to build it, so `cargo build` and `cargo test` never
//! need Node.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let dist = root.join("web").join("dist");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=web/dist");

    let mut files = Vec::new();
    if dist.join("index.html").is_file() {
        collect(&dist, &dist, &mut files);
    } else {
        let page = out.join("unbuilt.html");
        fs::write(
            &page,
            "<!doctype html><meta charset=utf-8><title>treetop</title>\
             <p>The web app is not built into this treetop. \
             Run <code>npm ci &amp;&amp; npm run build</code> in <code>web/</code>, \
             then build treetop again.</p>",
        )
        .unwrap();
        files.push(("index.html".to_string(), page));
    }
    files.sort();

    let mut code = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for (path, file) in &files {
        writeln!(
            code,
            "    ({path:?}, include_bytes!({:?})),",
            file.display().to_string()
        )
        .unwrap();
    }
    code.push_str("];\n");
    fs::write(out.join("assets.rs"), code).unwrap();
}

/// Every file under `dir`, by its path from `base` with `/` between parts.
fn collect(base: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(base, &path, files);
        } else {
            let name = path
                .strip_prefix(base)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            files.push((name, path));
        }
    }
}
