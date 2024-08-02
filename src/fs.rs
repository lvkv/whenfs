use crate::cache::{BlockingCache, Cache};
use crate::object::{DirectoryEntry, DirectoryObject, FileObject, FileSystemObject};
use crate::store::RecoveryDetails;

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use fuser::{FileAttr, FileType, Filesystem, ReplyAttr, ReplyDirectory, Request, FUSE_ROOT_ID};
use thiserror::Error;
use tracing::{debug, error, info, warn};

#[derive(Debug, Error)]
pub enum WhenFSError<TCache: BlockingCache> {
    #[error("Cache error: {0}")]
    Cache(<TCache as Cache>::Error),
}

pub struct WhenFS<TCache: BlockingCache> {
    cache: TCache,
    rt: tokio::runtime::Handle,
    file_handle_count: AtomicU64,
}

impl<TCache: BlockingCache> WhenFS<TCache> {
    const BLOCK_SIZE: u32 = 512;
    const MAX_NAME_LENGTH: usize = 255;
    // const MAX_FILE_SIZE: u64 = 1024 * 1024 * 1024 * 1024;
    const FILE_HANDLE_READ_BIT: u64 = 1 << 63;
    const FILE_HANDLE_WRITE_BIT: u64 = 1 << 62;

    pub fn new(mut cache: TCache, rt: tokio::runtime::Handle) -> Result<Self, WhenFSError<TCache>> {
        info!("Initializing filesystem");
        if cache
            .get_blocking(FUSE_ROOT_ID)
            .map_err(|e| WhenFSError::<TCache>::Cache(e))?
            .is_none()
        {
            info!("Could not find object for root inode");
            const WELCOME: &str = "WelcomeToWhenFS";
            let now = SystemTime::now();
            let mut entries = HashSet::with_capacity(1);
            entries.insert(DirectoryEntry {
                ino: FUSE_ROOT_ID,
                file_type: FileType::Directory,
                name: String::from("."),
            });
            entries.insert(DirectoryEntry {
                ino: FUSE_ROOT_ID + 1,
                file_type: FileType::RegularFile,
                name: WELCOME.to_string(),
            });
            let root_dir_obj = DirectoryObject {
                attr: FileAttr {
                    ino: FUSE_ROOT_ID,
                    size: 0,
                    blocks: 0,
                    atime: now,
                    mtime: now,
                    ctime: now,
                    crtime: now,
                    kind: FileType::Directory,
                    perm: 0o777,
                    nlink: 2, // Parent directory + self (".")
                    uid: 0,
                    gid: 0,
                    rdev: 0,
                    blksize: Self::BLOCK_SIZE,
                    flags: 0,
                },
                entries,
                name: String::from("root event"),
            };
            let ino = cache
                .insert_blocking(FUSE_ROOT_ID, FileSystemObject::Dir(root_dir_obj))
                .map_err(|e| WhenFSError::<TCache>::Cache(e))?;
            assert_eq!(ino, FUSE_ROOT_ID);
            let next_ino = cache.new_inode();
            let recovery_file = FileObject {
                attr: FileAttr {
                    ino: next_ino,
                    size: 1024,
                    blocks: 1,
                    atime: now,
                    mtime: now,
                    ctime: now,
                    crtime: now,
                    kind: FileType::RegularFile,
                    perm: 0o444,
                    nlink: 1,
                    uid: 0,
                    gid: 0,
                    rdev: 0,
                    blksize: Self::BLOCK_SIZE,
                    flags: 0,
                },
                name: String::from(WELCOME),
                data: Vec::new(),
            };
            let ino = cache
                .insert_blocking(next_ino, FileSystemObject::File(recovery_file))
                .map_err(|e| WhenFSError::<TCache>::Cache(e))?;
            assert_eq!(next_ino, ino);
            assert_eq!(ino, FUSE_ROOT_ID + 1);
        }

        Ok(Self {
            cache,
            rt,
            file_handle_count: AtomicU64::new(0),
        })
    }

    fn get_recovery_file_contents(&self) -> String {
        let RecoveryDetails { cal_id, root_id } = self.cache.get_recovery_id();

        format!(
            r#"Welcome to WhenFS!
If you're reading this, then you've successfully turned your Google calendar into a FUSE filesystem.
To recover this filesystem, run WhenFS with the following arguments.
The --root-event ID in this file changes after write operations, so don't copy these arguments too early or some of your data may become inaccessible.

--calendar {cal_id}
--root-event {root_id}

If you poke around enough, you'll likely run into bugs, edge cases, and completely unimplemented features.
There are no plans to fix these, but contributions are more than welcome.
Note that contributors are subject to a contributor license agreement ("CLA"), which requires that all
contributions be accompanied by a lighthearted meme that makes the author chuckle slightly, but not too much.
"#
        )
    }

    fn get_filesystem_object_by_ino(&self, ino: u64) -> Result<Arc<RwLock<FileSystemObject>>, i32> {
        self.cache
            .get_blocking(ino)
            .map_err(|error| {
                error!(%error);
                libc::EIO
            })
            .and_then(|maybe_handle| maybe_handle.ok_or(libc::ENOENT))
    }

    fn as_file_type(mode: u32) -> Result<FileType, i32> {
        let kind = match mode & libc::S_IFMT {
            libc::S_IFREG => FileType::RegularFile,
            libc::S_IFDIR => FileType::Directory,
            mode => {
                warn!(%mode, "Unimplemented file type");
                return Err(libc::ENOSYS);
            }
        };
        Ok(kind)
    }

    fn check_access(
        file_uid: u32,
        file_gid: u32,
        file_mode: u16,
        uid: u32,
        gid: u32,
        mut access_mask: i32,
    ) -> bool {
        if access_mask == libc::F_OK {
            return true;
        }

        let file_mode = i32::from(file_mode);
        if uid == 0 {
            // root is allowed to read or write anything
            // root is only allowed to exec if one of the exec bits is set
            // todo: remove  experiment
            return (access_mask & libc::X_OK == 0) != (file_mode & 0o111 != 0);
        }

        // this is the same though
        if uid == file_uid {
            access_mask -= access_mask & (file_mode >> 6);
        } else if gid == file_gid {
            access_mask -= access_mask & (file_mode >> 3);
        } else {
            access_mask -= access_mask & file_mode;
        }

        access_mask == 0
    }

    fn new_file_handle(&self, read: bool, write: bool) -> u64 {
        let mut fh = self.file_handle_count.fetch_add(1, Ordering::SeqCst);

        // Assert that we haven't run out of file handles (fake overflow)
        assert!(fh < (Self::FILE_HANDLE_READ_BIT | Self::FILE_HANDLE_WRITE_BIT));
        if read {
            fh |= Self::FILE_HANDLE_READ_BIT;
        }
        if write {
            fh |= Self::FILE_HANDLE_WRITE_BIT;
        }

        fh
    }

    // fn check_file_handle_read(file_handle: u64) -> bool {
    //     (file_handle & Self::FILE_HANDLE_READ_BIT) != 0
    // }

    fn check_file_handle_write(file_handle: u64) -> bool {
        (file_handle & Self::FILE_HANDLE_WRITE_BIT) != 0
    }

    fn write_inode(&mut self, ino: u64, attr: FileAttr) -> Result<(), i32> {
        let obj = match self.get_filesystem_object_by_ino(ino) {
            Ok(obj) => obj,
            Err(errno) => {
                return Err(errno);
            }
        };
        let mut handle = match obj.write() {
            Ok(obj) => obj,
            Err(error) => {
                error!(%error);
                return Err(libc::EIO);
            }
        };
        *handle.mut_attr() = attr;
        let new = handle.clone();
        match self.cache.insert_blocking(ino, new) {
            Ok(_ino) => (),
            Err(error) => {
                error!(%error);
            }
        }
        Ok(())
    }
}

impl<TCache: BlockingCache> Filesystem for WhenFS<TCache> {
    fn init(
        &mut self,
        _req: &Request<'_>,
        _config: &mut fuser::KernelConfig,
    ) -> Result<(), libc::c_int> {
        Ok(())
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        let obj = match self.get_filesystem_object_by_ino(ino) {
            Ok(obj) => obj,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let obj = match obj.read() {
            Ok(obj) => obj,
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        reply.attr(&Duration::new(0, 0), &obj.get_attr())
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        debug!(%ino, "readdir() called");
        assert!(offset >= 0);
        let obj = match self.get_filesystem_object_by_ino(ino) {
            Ok(obj) => obj,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let obj = match obj.read() {
            Ok(obj) => obj,
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        match &*obj {
            FileSystemObject::Dir(dir) => {
                for (i, entry) in dir.entries.iter().skip(offset as usize).enumerate() {
                    let reply_buffer_full = reply.add(
                        entry.ino,
                        offset + i as i64 + 1,
                        entry.file_type,
                        OsStr::from_bytes(entry.name.as_bytes()),
                    );

                    if reply_buffer_full {
                        break;
                    }
                }
            }
            _ => {
                reply.error(libc::ENOTDIR);
                return;
            }
        };

        reply.ok()
    }

    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEntry) {
        if name.len() > Self::MAX_NAME_LENGTH {
            reply.error(libc::ENAMETOOLONG);
            return;
        }

        let parent_obj = match self.get_filesystem_object_by_ino(parent) {
            Ok(obj) => obj,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let parent_obj = match parent_obj.read() {
            Ok(obj) => obj,
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        let parent_dir = match &*parent_obj {
            FileSystemObject::Dir(dir) => dir,
            _not_directory => {
                reply.error(libc::ENOTDIR);
                return;
            }
        };

        let found = match parent_dir.get_entry_by_name(name) {
            Some(found) => found,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let maybe_found_handle = match self.cache.get_blocking(found.ino) {
            Ok(maybe_handle) => maybe_handle,
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        let found_handle = match maybe_found_handle {
            Some(obj) => obj,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let found_obj = match found_handle.read() {
            Ok(obj) => obj,
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        reply.entry(&Duration::new(0, 0), &found_obj.get_attr(), 0);
    }

    fn create(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        debug!("create() called with {:?} {:?}", parent, name);
        let (read, write) = match flags & libc::O_ACCMODE {
            libc::O_RDONLY => (true, false),
            libc::O_WRONLY => (false, true),
            libc::O_RDWR => (true, true),
            _ => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        let maybe_parent_handle = match self.cache.get_blocking(parent) {
            Ok(maybe_handle) => maybe_handle,
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        let parent_handle = match maybe_parent_handle {
            Some(obj) => obj,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let parent_obj = match parent_handle.read() {
            Ok(obj) => obj,
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        let parent_dir = match &*parent_obj {
            FileSystemObject::Dir(dir) => dir,
            _not_directory => {
                reply.error(libc::ENOTDIR);
                return;
            }
        };
        let mut new_parent_dir = parent_dir.clone();

        if parent_dir.get_entry_by_name(name).is_some() {
            reply.error(libc::EEXIST);
            return;
        };

        let kind = match Self::as_file_type(mode) {
            Ok(kind) => kind,
            Err(error) => {
                reply.error(error);
                return;
            }
        };

        let name = name.to_string_lossy().to_string();
        let now = SystemTime::now();
        let ino = self.cache.new_inode();
        let attr = FileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind,
            perm: mode as u16,
            nlink: 1,
            uid: req.uid(),
            gid: req.gid(),
            rdev: 0,
            blksize: Self::BLOCK_SIZE,
            flags: 0,
        };
        let attr_copy = attr;
        new_parent_dir.entries.insert(DirectoryEntry {
            ino,
            file_type: kind,
            name: name.clone(),
        });
        let obj = match kind {
            FileType::RegularFile => FileSystemObject::File(FileObject {
                attr,
                name,
                data: Vec::new(),
            }),
            FileType::Directory => FileSystemObject::Dir(DirectoryObject {
                attr,
                entries: {
                    let mut entries = HashSet::with_capacity(2);
                    entries.insert(DirectoryEntry {
                        ino,
                        file_type: FileType::Directory,
                        name: ".".to_string(),
                    });
                    entries.insert(DirectoryEntry {
                        ino: parent,
                        file_type: FileType::Directory,
                        name: "..".to_string(),
                    });
                    entries
                },
                name,
            }),
            kind => {
                warn!(?kind, "Unimplemented file kind");
                reply.error(libc::ENOSYS);
                return;
            }
        };

        match self.cache.insert_blocking(ino, obj) {
            Ok(ino) => ino,
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        let new_parent = FileSystemObject::Dir(new_parent_dir);
        match self.cache.insert_blocking(parent, new_parent) {
            Ok(ino) => ino,
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        let fh = self.new_file_handle(read, write);

        reply.created(&Duration::new(0, 0), &attr_copy, 0, fh, 0)
    }

    fn access(&mut self, req: &Request<'_>, ino: u64, mask: i32, reply: fuser::ReplyEmpty) {
        debug!("access() {ino:?} {mask:?}");
        let obj = match self.get_filesystem_object_by_ino(ino) {
            Ok(obj) => obj,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let attr = match obj.read() {
            Ok(handle) => handle.get_attr(),
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        if Self::check_access(attr.uid, attr.gid, attr.perm, req.uid(), req.gid(), mask) {
            reply.ok();
        } else {
            reply.error(libc::EACCES);
        }
    }

    fn setattr(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        fh: Option<u64>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        debug!(
            "setattr called with (ino: {:#x?}, mode: {:?}, uid: {:?}, \\
            gid: {:?}, size: {:?}, fh: {:?}, flags: {:?})",
            ino, mode, uid, gid, size, fh, flags
        );

        let obj = match self.get_filesystem_object_by_ino(ino) {
            Ok(obj) => obj,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let obj = match obj.read() {
            Ok(handle) => handle,
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        let mut attrs = obj.get_attr();
        if let Some(mode) = mode {
            debug!("chmod() called with {:?}, {:o}", ino, mode);
            if req.uid() != 0 && req.uid() != attrs.uid {
                reply.error(libc::EPERM);
                return;
            }
            if req.uid() != 0 && req.gid() != attrs.gid {
                // If SGID is set and the file belongs to a group that the caller is not part of
                // then the SGID bit is suppose to be cleared during chmod
                attrs.perm = (mode & !libc::S_ISGID) as u16;
            } else {
                attrs.perm = mode as u16;
            }
            attrs.ctime = SystemTime::now();
            match self.write_inode(attrs.ino, attrs) {
                Ok(()) => (),
                Err(e) => {
                    reply.error(e);
                    return;
                }
            }
            reply.attr(&Duration::new(0, 0), &attrs);
            return;
        }

        if uid.is_some() || gid.is_some() {
            debug!("chown() called with {:?} {:?} {:?}", ino, uid, gid);
            if let Some(_gid) = gid {
                // Non-root users can only change gid to a group they're in
                if req.uid() != 0 {
                    reply.error(libc::EPERM);
                    return;
                }
            }
            if let Some(uid) = uid {
                if req.uid() != 0
                    // but no-op changes by the owner are not an error
                    && !(uid == attrs.uid && req.uid() == attrs.uid)
                {
                    reply.error(libc::EPERM);
                    return;
                }
            }
            // Only owner may change the group
            if gid.is_some() && req.uid() != 0 && req.uid() != attrs.uid {
                reply.error(libc::EPERM);
                return;
            }

            if attrs.perm & (libc::S_IXUSR | libc::S_IXGRP | libc::S_IXOTH) as u16 != 0 {
                reply.error(libc::ENOSYS);
                return;
            }

            if let Some(uid) = uid {
                attrs.uid = uid;
                // Clear SETUID on owner change
                attrs.perm &= !libc::S_ISUID as u16;
            }
            if let Some(gid) = gid {
                attrs.gid = gid;
                // Clear SETGID unless user is root
                if req.uid() != 0 {
                    attrs.perm &= !libc::S_ISGID as u16;
                }
            }
            attrs.ctime = SystemTime::now();
            match self.write_inode(attrs.ino, attrs) {
                Ok(()) => (),
                Err(e) => {
                    reply.error(e);
                    return;
                }
            }
            reply.attr(&Duration::new(0, 0), &attrs);
            return;
        }

        if let Some(size) = size {
            debug!("truncate() called with {ino:?} {size:?}");
            reply.error(libc::ENOSYS);
            return;
        }

        let now = SystemTime::now();
        if let Some(atime) = atime {
            debug!("utimens() called with {ino:?}, atime={atime:?}");
        }
        if let Some(mtime) = mtime {
            debug!("utimens() called with {ino:?}, mtime={mtime:?}");
        }
        let attrs = obj.get_attr();
        reply.attr(&Duration::new(0, 0), &attrs);
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: fuser::ReplyData,
    ) {
        debug!(
            "read called with (ino: {:#x?}, fh: {}, offset: {}, size: {}, \\
            flags: {:#x?}, lock_owner: {:?})",
            ino, fh, offset, size, flags, lock_owner
        );
        if offset < 0 {
            warn!(%offset, "read: offset less than 0");
            reply.error(libc::EINVAL);
            return;
        }

        // oops haha
        // if !Self::check_file_handle_read(fh) {
        //     reply.error(libc::EACCES);
        //     return;
        // }

        if ino == FUSE_ROOT_ID + 1 {
            let data = self.get_recovery_file_contents();
            let data = data.as_bytes();
            let lower_bound = offset as usize;
            let upper_bound = (lower_bound + size as usize).min(data.len());
            reply.data(&data[lower_bound..upper_bound]);
            return;
        }

        let offset = offset as u64;
        let obj = match self.get_filesystem_object_by_ino(ino) {
            Ok(handle) => handle,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };
        let obj = match obj.read() {
            Ok(handle) => handle,
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };
        let obj = match &*obj {
            FileSystemObject::Dir(_) => {
                reply.error(libc::EISDIR);
                return;
            }
            FileSystemObject::File(old_obj) => old_obj,
        };

        let lower_bound = offset as usize;
        let upper_bound = (lower_bound + size as usize).min(obj.data.len());
        reply.data(&obj.data[lower_bound..upper_bound]);
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        write_flags: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        debug!(
            "write called with (ino: {:#x?}, fh: {}, offset: {}, data.len(): {}, \\
            write_flags: {:#x?}, flags: {:#x?}, lock_owner: {:?})",
            ino,
            fh,
            offset,
            data.len(),
            write_flags,
            flags,
            lock_owner
        );
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let offset = offset as u64;

        if !Self::check_file_handle_write(fh) {
            reply.error(libc::EACCES);
            return;
        }

        let obj = match self.get_filesystem_object_by_ino(ino) {
            Ok(handle) => handle,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let (mut new_obj, old_attr) = {
            let obj = match obj.write() {
                Ok(handle) => handle,
                Err(error) => {
                    error!(%error);
                    reply.error(libc::EIO);
                    return;
                }
            };

            let old_obj = match &*obj {
                FileSystemObject::Dir(_) => {
                    reply.error(libc::EISDIR);
                    return;
                }
                FileSystemObject::File(old_obj) => old_obj,
            };

            (old_obj.clone(), old_obj.attr)
        };

        let now = SystemTime::now();
        new_obj.attr.ctime = now;
        new_obj.attr.atime = now;
        new_obj.attr.mtime = now;
        let old_len = old_attr.size as usize;
        if data.len() + offset as usize > old_len {
            let new_len = new_obj.attr.size as usize + data.len();
            debug!(%old_len, %new_len, name = %new_obj.name, "read: resizing file buffer");
            new_obj.data.resize(new_len, 0);
            new_obj.attr.size = new_len as u64;
        } else {
            debug!(%old_len, name = %new_obj.name, "read: no need to resize file buffer");
        }
        new_obj.data[offset as usize..offset as usize + data.len()].copy_from_slice(data);
        match self
            .cache
            .insert_blocking(new_obj.attr.ino, FileSystemObject::File(new_obj))
        {
            Ok(_ino) => (),
            Err(error) => {
                error!(%error);
                reply.error(libc::EIO);
                return;
            }
        };

        reply.written(data.len() as u32);
    }

    fn destroy(&mut self) {}

    fn forget(&mut self, _req: &Request<'_>, _ino: u64, _nlookup: u64) {}

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: fuser::ReplyData) {
        debug!("[Not Implemented] readlink(ino: {:#x?})", ino);
        reply.error(libc::ENOSYS);
    }

    fn mknod(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
        reply: fuser::ReplyEntry,
    ) {
        debug!(
            "[Not Implemented] mknod(parent: {:#x?}, name: {:?}, mode: {}, \\
            umask: {:#x?}, rdev: {})",
            parent, name, mode, umask, rdev
        );
        reply.error(libc::ENOSYS);
    }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: fuser::ReplyEntry,
    ) {
        debug!(
            "[Not Implemented] mkdir(parent: {:#x?}, name: {:?}, mode: {}, umask: {:#x?})",
            parent, name, mode, umask
        );
        reply.error(libc::ENOSYS);
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        debug!(
            "[Not Implemented] unlink(parent: {:#x?}, name: {:?})",
            parent, name,
        );
        reply.error(libc::ENOSYS);
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        debug!(
            "[Not Implemented] rmdir(parent: {:#x?}, name: {:?})",
            parent, name,
        );
        reply.error(libc::ENOSYS);
    }

    fn symlink(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        link_name: &OsStr,
        target: &std::path::Path,
        reply: fuser::ReplyEntry,
    ) {
        debug!(
            "[Not Implemented] symlink(parent: {:#x?}, link_name: {:?}, target: {:?})",
            parent, link_name, target,
        );
        reply.error(libc::EPERM);
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: fuser::ReplyEmpty,
    ) {
        debug!(
            "[Not Implemented] rename(parent: {:#x?}, name: {:?}, newparent: {:#x?}, \\
            newname: {:?}, flags: {})",
            parent, name, newparent, newname, flags,
        );
        reply.error(libc::ENOSYS);
    }

    fn link(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: fuser::ReplyEntry,
    ) {
        debug!(
            "[Not Implemented] link(ino: {:#x?}, newparent: {:#x?}, newname: {:?})",
            ino, newparent, newname
        );
        reply.error(libc::EPERM);
    }

    fn open(&mut self, _req: &Request<'_>, _ino: u64, _flags: i32, reply: fuser::ReplyOpen) {
        reply.opened(0, 0);
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        reply.ok();
    }

    fn fsync(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        datasync: bool,
        reply: fuser::ReplyEmpty,
    ) {
        debug!(
            "[Not Implemented] fsync(ino: {:#x?}, fh: {}, datasync: {})",
            ino, fh, datasync
        );
        reply.error(libc::ENOSYS);
    }

    fn opendir(&mut self, _req: &Request<'_>, _ino: u64, _flags: i32, reply: fuser::ReplyOpen) {
        reply.opened(0, 0);
    }

    fn readdirplus(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        reply: fuser::ReplyDirectoryPlus,
    ) {
        debug!(
            "[Not Implemented] readdirplus(ino: {:#x?}, fh: {}, offset: {})",
            ino, fh, offset
        );
        reply.error(libc::ENOSYS);
    }

    fn releasedir(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _flags: i32,
        reply: fuser::ReplyEmpty,
    ) {
        reply.ok();
    }

    fn fsyncdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        datasync: bool,
        reply: fuser::ReplyEmpty,
    ) {
        debug!(
            "[Not Implemented] fsyncdir(ino: {:#x?}, fh: {}, datasync: {})",
            ino, fh, datasync
        );
        reply.error(libc::ENOSYS);
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: fuser::ReplyStatfs) {
        reply.statfs(0, 0, 0, 0, 0, 512, 255, 0);
    }

    fn setxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        _value: &[u8],
        flags: i32,
        position: u32,
        reply: fuser::ReplyEmpty,
    ) {
        debug!(
            "[Not Implemented] setxattr(ino: {:#x?}, name: {:?}, flags: {:#x?}, position: {})",
            ino, name, flags, position
        );
        reply.error(libc::ENOSYS);
    }

    fn getxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: fuser::ReplyXattr,
    ) {
        debug!(
            "[Not Implemented] getxattr(ino: {:#x?}, name: {:?}, size: {})",
            ino, name, size
        );
        reply.error(libc::ENOSYS);
    }

    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: fuser::ReplyXattr) {
        debug!(
            "[Not Implemented] listxattr(ino: {:#x?}, size: {})",
            ino, size
        );
        reply.error(libc::ENOSYS);
    }

    fn removexattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        reply: fuser::ReplyEmpty,
    ) {
        debug!(
            "[Not Implemented] removexattr(ino: {:#x?}, name: {:?})",
            ino, name
        );
        reply.error(libc::ENOSYS);
    }

    fn getlk(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        lock_owner: u64,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
        reply: fuser::ReplyLock,
    ) {
        debug!(
            "[Not Implemented] getlk(ino: {:#x?}, fh: {}, lock_owner: {}, start: {}, \\
            end: {}, typ: {}, pid: {})",
            ino, fh, lock_owner, start, end, typ, pid
        );
        reply.error(libc::ENOSYS);
    }

    fn setlk(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        lock_owner: u64,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
        sleep: bool,
        reply: fuser::ReplyEmpty,
    ) {
        debug!(
            "[Not Implemented] setlk(ino: {:#x?}, fh: {}, lock_owner: {}, start: {}, \\
            end: {}, typ: {}, pid: {}, sleep: {})",
            ino, fh, lock_owner, start, end, typ, pid, sleep
        );
        reply.error(libc::ENOSYS);
    }

    fn bmap(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        blocksize: u32,
        idx: u64,
        reply: fuser::ReplyBmap,
    ) {
        debug!(
            "[Not Implemented] bmap(ino: {:#x?}, blocksize: {}, idx: {})",
            ino, blocksize, idx,
        );
        reply.error(libc::ENOSYS);
    }

    fn ioctl(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        flags: u32,
        cmd: u32,
        in_data: &[u8],
        out_size: u32,
        reply: fuser::ReplyIoctl,
    ) {
        debug!(
            "[Not Implemented] ioctl(ino: {:#x?}, fh: {}, flags: {}, cmd: {}, \\
            in_data.len(): {}, out_size: {})",
            ino,
            fh,
            flags,
            cmd,
            in_data.len(),
            out_size,
        );
        reply.error(libc::ENOSYS);
    }

    fn fallocate(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        length: i64,
        mode: i32,
        reply: fuser::ReplyEmpty,
    ) {
        debug!(
            "[Not Implemented] fallocate(ino: {:#x?}, fh: {}, offset: {}, \\
            length: {}, mode: {})",
            ino, fh, offset, length, mode
        );
        reply.error(libc::ENOSYS);
    }

    fn lseek(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        whence: i32,
        reply: fuser::ReplyLseek,
    ) {
        debug!(
            "[Not Implemented] lseek(ino: {:#x?}, fh: {}, offset: {}, whence: {})",
            ino, fh, offset, whence
        );
        reply.error(libc::ENOSYS);
    }

    fn copy_file_range(
        &mut self,
        _req: &Request<'_>,
        ino_in: u64,
        fh_in: u64,
        offset_in: i64,
        ino_out: u64,
        fh_out: u64,
        offset_out: i64,
        len: u64,
        flags: u32,
        reply: fuser::ReplyWrite,
    ) {
        debug!(
            "[Not Implemented] copy_file_range(ino_in: {:#x?}, fh_in: {}, \\
            offset_in: {}, ino_out: {:#x?}, fh_out: {}, offset_out: {}, \\
            len: {}, flags: {})",
            ino_in, fh_in, offset_in, ino_out, fh_out, offset_out, len, flags
        );
        reply.error(libc::ENOSYS);
    }
}
