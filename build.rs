use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::fs::File;
use std::hash::Hasher;
use std::io::Read;

const ASSET_FILES: [&str; 4] = [
    "static/app.css",
    "static/app.js",
    "static/icons.svg",
    "static/htmx.min.js",
];

fn hash_assets() -> Result<String, Box<dyn Error>> {
    let mut hasher = DefaultHasher::new();
    for path in ASSET_FILES {
        let mut bytes = Vec::new();
        File::open(path)?.read_to_end(&mut bytes)?;
        hasher.write(&bytes);
    }
    return Ok(format!("{:016x}", hasher.finish()));
}

fn main() -> Result<(), Box<dyn Error>> {
    for path in ASSET_FILES {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rustc-env=ASSET_VERSION={}", hash_assets()?);
    return Ok(());
}
