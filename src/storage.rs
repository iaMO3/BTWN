use anyhow::{Context, Result, bail, ensure};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_reader(std::io::BufReader::new(
        fs::File::open(path).with_context(|| format!("Open {}", path.display()))?,
    ))
    .context("Read backup metadata")
}

pub fn sibling(path: &Path) -> PathBuf {
    path.with_file_name(format!(".between-{}.tmp", Uuid::new_v4().simple()))
}

pub fn mkdir_parent(path: &Path) -> Result<()> {
    fs::create_dir_all(path.parent().context("File has no parent folder")?)?;
    Ok(())
}

#[cfg(windows)]
pub fn lock(path: &Path) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    mkdir_parent(path)?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
        .context("Another Between window is already syncing")
}

#[cfg(windows)]
fn replace(from: &Path, into: &Path) -> Result<()> {
    move_file(from, into, true)
}

#[cfg(windows)]
fn move_file(from: &Path, into: &Path, replace: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let full_path = |path: &Path| -> Result<Vec<u16>> {
        let parent = fs::canonicalize(path.parent().context("File has no parent folder")?)?;
        let name = path.file_name().context("File has no name")?;
        Ok(parent
            .join(name)
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect())
    };
    let from = full_path(from)?;
    let into = full_path(into)?;
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            into.as_ptr(),
            if replace {
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH
            } else {
                MOVEFILE_WRITE_THROUGH
            },
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace(from: &Path, into: &Path) -> Result<()> {
    Ok(fs::rename(from, into)?)
}

pub fn write<T: Serialize + ?Sized>(path: &Path, data: &T) -> Result<()> {
    mkdir_parent(path)?;
    let temp = sibling(path);
    let result = (|| {
        let mut file = fs::File::create_new(&temp)?;
        serde_json::to_writer(&mut file, data)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        replace(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("Open {}", path.display()))?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

pub fn checksum(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn scoped(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = relative.replace('\\', "/");
    let mut path = root.to_path_buf();
    ensure!(!relative.is_empty(), "Empty path in backup");
    for component in Path::new(&relative).components() {
        let Component::Normal(part) = component else {
            bail!("Unsafe backup path: {relative}")
        };
        let part = part.to_str().context("Invalid file name")?;
        let stem = part.split('.').next().unwrap().to_ascii_uppercase();
        let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit());
        ensure!(
            !reserved
                && !part.ends_with([' ', '.'])
                && !part
                    .chars()
                    .any(|c| c.is_control() || "<>:\"|?*".contains(c)),
            "Unsafe backup path: {relative}"
        );
        path.push(part);
        if let Ok(meta) = fs::symlink_metadata(&path) {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                ensure!(
                    meta.file_attributes() & 0x400 == 0,
                    "A linked folder is in the transfer path: {}",
                    path.display()
                );
            }
            #[cfg(not(windows))]
            ensure!(
                !meta.is_symlink(),
                "A linked folder is in the transfer path"
            );
        }
    }
    Ok(path)
}

pub fn verified_stream(
    input: &mut impl Read,
    path: &Path,
    size: u64,
    expected: &str,
) -> Result<()> {
    ensure!(is_hash(expected), "Invalid file checksum");
    mkdir_parent(path)?;
    let temp = sibling(path);
    let result = (|| {
        let mut output = fs::File::create_new(&temp)?;
        let mut digest = Sha256::new();
        let mut left = size;
        let mut block = vec![0; 1024 * 1024];
        while left != 0 {
            let count = block.len().min(left as usize);
            let bytes = input.read(&mut block[..count])?;
            ensure!(bytes != 0, "The transfer ended before a file arrived");
            output.write_all(&block[..bytes])?;
            digest.update(&block[..bytes]);
            left -= bytes as u64;
        }
        ensure!(
            format!("{:x}", digest.finalize()) == expected,
            "A file failed its checksum"
        );
        output.flush()?;
        output.sync_all()?;
        drop(output);
        #[cfg(windows)]
        let moved = move_file(&temp, path, false);
        #[cfg(not(windows))]
        let moved = fs::hard_link(&temp, path).map_err(anyhow::Error::from);
        if moved.is_err() && path.exists() {
            ensure!(
                hash(path)? == expected,
                "A different file already exists at {}",
                path.display()
            );
        } else {
            moved?;
        }
        if temp.exists() {
            fs::remove_file(&temp)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub fn copy_link(source: &Path, dest: &Path) -> Result<()> {
    mkdir_parent(dest)?;
    if !dest.exists() && fs::hard_link(source, dest).is_err() {
        let mut input = fs::File::open(source)?;
        let temp = sibling(dest);
        let result = (|| {
            let mut output = fs::File::create_new(&temp)?;
            std::io::copy(&mut input, &mut output)?;
            drop(output);
            fs::rename(&temp, dest)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result?;
    }
    Ok(())
}

pub fn copy_replace(source: &Path, dest: &Path) -> Result<()> {
    mkdir_parent(dest)?;
    let temp = sibling(dest);
    let result = (|| {
        fs::copy(source, &temp)?;
        ensure!(
            hash(source)? == hash(&temp)?,
            "File copy did not pass verification"
        );
        fs::OpenOptions::new()
            .write(true)
            .open(&temp)?
            .sync_all()
            .context("Flush the verified backup copy")?;
        replace(&temp, dest).with_context(|| format!("Install verified copy at {}", dest.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub fn windows_path(value: &Path) -> String {
    let text = value.to_string_lossy();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
}
