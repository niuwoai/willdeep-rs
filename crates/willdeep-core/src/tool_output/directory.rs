use std::fs::File;
use std::io;
use std::path::Path;

pub(super) struct Directory {
    #[cfg(unix)]
    handle: File,
    #[cfg(not(unix))]
    path: std::path::PathBuf,
}

impl Directory {
    pub(super) fn open(path: &Path, create: bool) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let root = path
                .parent()
                .ok_or_else(|| io::Error::other("missing archive root"))?;
            let parent = open_root(root, create)?;
            let scope = name(
                path.file_name()
                    .ok_or_else(|| io::Error::other("missing archive scope"))?,
            )?;
            if create {
                // SAFETY: The parent descriptor is live and scope is NUL-terminated.
                let created = unsafe { libc::mkdirat(parent.as_raw_fd(), scope.as_ptr(), 0o700) };
                if created != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                {
                    return Err(io::Error::last_os_error());
                }
            }
            let handle = open_at(&parent, &scope, libc::O_RDONLY | libc::O_DIRECTORY)?;
            Ok(Self { handle })
        }
        #[cfg(not(unix))]
        {
            if create {
                std::fs::create_dir_all(path)?;
            }
            if !std::fs::symlink_metadata(path)?.file_type().is_dir() {
                return Err(io::Error::other("archive scope must be a directory"));
            }
            Ok(Self {
                path: path.to_owned(),
            })
        }
    }

    pub(super) fn file(&self, filename: &str, create: bool) -> io::Result<File> {
        #[cfg(unix)]
        let file = open_at(
            &self.handle,
            &name(filename.as_ref())?,
            if create {
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
            } else {
                libc::O_RDONLY | libc::O_NONBLOCK
            },
        )?;
        #[cfg(not(unix))]
        let file = {
            let mut options = std::fs::OpenOptions::new();
            if create {
                options.write(true).create_new(true);
            } else {
                options.read(true);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
                options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
            }
            options.open(self.path.join(filename))?
        };
        if !file.metadata()?.file_type().is_file() {
            return Err(io::Error::other("tool output must be a regular file"));
        }
        Ok(file)
    }

    pub(super) fn publish(&self, temporary: &str, final_name: &str) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let source = name(temporary.as_ref())?;
            let target = name(final_name.as_ref())?;
            // SAFETY: Both names and the directory descriptor remain live.
            if unsafe {
                libc::linkat(
                    self.handle.as_raw_fd(),
                    source.as_ptr(),
                    self.handle.as_raw_fd(),
                    target.as_ptr(),
                    0,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        std::fs::hard_link(self.path.join(temporary), self.path.join(final_name))
    }

    pub(super) fn remove(&self, filename: &str) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let filename = name(filename.as_ref())?;
            // SAFETY: The name and directory descriptor remain live.
            if unsafe { libc::unlinkat(self.handle.as_raw_fd(), filename.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        std::fs::remove_file(self.path.join(filename))
    }

    pub(super) fn sync(&self) -> io::Result<()> {
        #[cfg(unix)]
        self.handle.sync_all()?;
        Ok(())
    }
}

#[cfg(unix)]
fn name(value: &std::ffi::OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(value.as_bytes()).map_err(|_| io::Error::other("invalid archive name"))
}

#[cfg(unix)]
fn open_root(path: &Path, create: bool) -> io::Result<File> {
    use std::os::fd::AsRawFd;
    use std::path::Component;
    let absolute = std::path::absolute(path)?;
    let mut parent = File::open("/")?;
    for component in absolute.components() {
        let component = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(value) => name(value)?,
            _ => {
                return Err(io::Error::other(
                    "archive root must not contain parent traversal",
                ));
            }
        };
        let next = open_at(&parent, &component, libc::O_RDONLY | libc::O_DIRECTORY);
        parent = match next {
            Ok(next) => next,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                // SAFETY: Parent and component remain live, and mode is private.
                if unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) } != 0
                    && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                {
                    return Err(io::Error::last_os_error());
                }
                parent.sync_all()?;
                open_at(&parent, &component, libc::O_RDONLY | libc::O_DIRECTORY)?
            }
            Err(error) => open_system_alias(&parent, &component).map_err(|_| error)?,
        };
    }
    Ok(parent)
}

/// macOS exposes /var and /tmp through administrator-owned aliases. Follow
/// only aliases that an unprivileged workspace process cannot replace.
#[cfg(unix)]
fn open_system_alias(parent: &File, component: &std::ffi::CStr) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;
    let metadata = parent.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::other("archive ancestor links are refused"));
    }
    let mut entry = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: stat buffer, parent descriptor and terminated name are valid.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            component.as_ptr(),
            entry.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Successful fstatat initialized the whole stat structure.
    let entry = unsafe { entry.assume_init() };
    if entry.st_uid != 0 || entry.st_mode & libc::S_IFMT != libc::S_IFLNK {
        return Err(io::Error::other("archive ancestor links are refused"));
    }
    // SAFETY: This root-owned alias is in a root-owned non-writable directory;
    // an unprivileged workspace process cannot exchange it between check/open.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_at(parent: &File, name: &std::ffi::CStr, flags: i32) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    // SAFETY: The parent descriptor and NUL-terminated name are valid. A successful
    // open transfers one new descriptor into File. All opens refuse leaf links.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
