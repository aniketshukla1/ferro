//! Embeds the product UI (`web/`) gzip-compressed: about a quarter of its size in the binary,
//! and served as-is to every browser that accepts gzip (legacy.rs). The e2e suite (`web/tests`)
//! and the mock backend (`web/src/mock`) never ship (BACKEND.md B1).

use std::io::Write;
use std::path::{Path, PathBuf};

const PARTS: [&str; 5] = ["index.html", "next.html", "src", "styles", "assets"];

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p != root.join("src/mock") {
                walk(root, &p, out);
            }
        } else {
            out.push(p);
        }
    }
}

/// FNV-1a over the whole file: a stable weak ETag without a hashing dependency.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut files = Vec::new();
    for part in PARTS {
        let p = root.join(part);
        println!("cargo:rerun-if-changed={}", p.display());
        if p.is_dir() {
            walk(&root, &p, &mut files);
        } else if p.is_file() {
            files.push(p);
        }
    }
    // Sorted by the path string: legacy.rs looks files up with a binary search.
    let mut files: Vec<(String, PathBuf)> = files
        .into_iter()
        .map(|f| {
            (
                f.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
                f,
            )
        })
        .collect();
    files.sort();
    let mut table = String::from("pub static WEB: &[(&str, &[u8], &str)] = &[\n");
    for (i, (rel, f)) in files.iter().enumerate() {
        let raw = std::fs::read(f).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gz.write_all(&raw).unwrap();
        let gz_path = out_dir.join(format!("web{i}.gz"));
        std::fs::write(&gz_path, gz.finish().unwrap()).unwrap();
        table.push_str(&format!(
            "    ({rel:?}, include_bytes!({:?}), \"W/\\\"{:x}-{:x}\\\"\"),\n",
            gz_path.display().to_string(),
            raw.len(),
            fnv(&raw)
        ));
    }
    table.push_str("];\n");
    std::fs::write(out_dir.join("web_assets.rs"), table).unwrap();
}
