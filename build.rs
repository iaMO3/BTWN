#[path = "src/brand.rs"]
mod brand;

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=src/brand.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
    {
        return;
    }

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo build directory"));
    let linker = env::var_os("RUSTC_LINKER").map(PathBuf::from);
    let windres = linker
        .as_deref()
        .and_then(|path| path.parent())
        .filter(|path| !path.as_os_str().is_empty())
        .map(|dir| dir.join("windres.exe"))
        .unwrap_or_else(|| PathBuf::from("windres"));
    fs::write(out.join("btwn.ico"), icon()).expect("Write BTWN icon");
    fs::write(
        out.join("btwn.rc"),
        format!(
            r#"1 ICON "btwn.ico"
1 VERSIONINFO
 FILEVERSION {major},{minor},{patch},0
 PRODUCTVERSION {major},{minor},{patch},0
 BEGIN
   BLOCK "StringFileInfo"
   BEGIN
     BLOCK "040904B0"
     BEGIN
       VALUE "FileDescription", "BTWN\0"
       VALUE "ProductName", "BTWN\0"
       VALUE "FileVersion", "{version}\0"
       VALUE "ProductVersion", "{version}\0"
     END
   END
   BLOCK "VarFileInfo"
   BEGIN
     VALUE "Translation", 0x0409, 1200
   END
 END
"#,
            major = env!("CARGO_PKG_VERSION_MAJOR"),
            minor = env!("CARGO_PKG_VERSION_MINOR"),
            patch = env!("CARGO_PKG_VERSION_PATCH"),
            version = env!("CARGO_PKG_VERSION"),
        ),
    )
    .expect("Write BTWN resources");
    match Command::new(windres)
        .current_dir(&out)
        .args(["btwn.rc", "-O", "coff", "-o", "btwn-resource.o"])
        .status()
    {
        Ok(result) if result.success() => println!(
            "cargo:rustc-link-arg={}",
            out.join("btwn-resource.o").display()
        ),
        Ok(_) => panic!("Could not compile BTWN Windows resources"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("cargo:warning=windres unavailable. BTWN will still use its icon at runtime.")
        }
        Err(error) => panic!("Could not run windres: {error}"),
    }
}

fn icon() -> Vec<u8> {
    let sizes = [16u32, 32, 48, 64, 128, 256];
    let mut header = vec![0, 0, 1, 0, sizes.len() as u8, 0];
    let mut sprites = Vec::new();
    for size in sizes {
        let bytes = size * size * 4;
        let mask = size.div_ceil(32) * 4 * size;
        let offset = 6 + sizes.len() as u32 * 16 + sprites.len() as u32;
        header.extend([size as u8, size as u8, 0, 0, 1, 0, 32, 0]);
        header.extend((40 + bytes + mask).to_le_bytes());
        header.extend(offset.to_le_bytes());
        for field in [40, size, size * 2, (32 << 16) | 1, 0, bytes, 0, 0, 0, 0] {
            sprites.extend(field.to_le_bytes());
        }
        let rgba = brand::pixels(size);
        for row in rgba.chunks_exact(size as usize * 4).rev() {
            for pixel in row.chunks_exact(4) {
                sprites.extend([pixel[2], pixel[1], pixel[0], pixel[3]]);
            }
        }
        sprites.resize(sprites.len() + mask as usize, 0);
    }
    header.extend(sprites);
    header
}
