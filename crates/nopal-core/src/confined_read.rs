//! Bounded, no-follow reads relative to one held directory capability.
//!
//! Every path component is opened relative to the previously held directory
//! with no-follow semantics. A pathname observation never authorizes a later
//! ambient read, so concurrent symlink replacement cannot redirect authority.

use std::fs;
use std::io::{self, Read as _};
use std::path::{Component, Path};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::fs::{Dir, OpenOptions};

pub fn read_utf8(root: &Path, relative: &Path, max_bytes: usize) -> io::Result<Option<String>> {
    let bytes = match read_bytes(root, relative, max_bytes)? {
        Some(bytes) => bytes,
        None => return Ok(None),
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn regular_file_exists(root: &Path, relative: &Path) -> io::Result<bool> {
    let (parent, name) = match open_parent(root, relative) {
        Ok(opened) => opened,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    match parent.open_with(name, &options) {
        Ok(file) => Ok(file.metadata()?.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn read_bytes(root: &Path, relative: &Path, max_bytes: usize) -> io::Result<Option<Vec<u8>>> {
    let (parent, name) = match open_parent(root, relative) {
        Ok(opened) => opened,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = match parent.open_with(name, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "authority source must be a regular non-symlink file",
        ));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("authority source exceeds the {max_bytes}-byte read bound"),
        ));
    }
    Ok(Some(bytes))
}

fn open_parent<'a>(root: &Path, relative: &'a Path) -> io::Result<(Dir, &'a std::ffi::OsStr)> {
    let name = relative.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "authority path has no file name",
        )
    })?;
    let mut directory = open_root(root)?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    for component in parent.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "authority path must be a confined relative path",
            ));
        };
        let metadata = directory.symlink_metadata(name)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "authority directory component must be a regular non-symlink directory",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_DIRECTORY);
        }
        let opened = directory.open_with(name, &options)?;
        if !opened.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "authority directory component changed while opening",
            ));
        }
        directory = Dir::from_std_file(opened.into_std());
    }
    Ok((directory, name))
}

fn open_root(root: &Path) -> io::Result<Dir> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "authority root must be a regular non-symlink directory",
        ));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let opened = options.open(root)?;
    if !opened.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "authority root changed while opening",
        ));
    }
    Ok(Dir::from_std_file(opened))
}
