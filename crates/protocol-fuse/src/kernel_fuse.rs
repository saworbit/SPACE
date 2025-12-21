use anyhow::Result;
use capsule_registry::pipeline::WritePipeline;
use common::CapsuleId;
use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, Request,
};
use libc::{EACCES, EINVAL, EIO, ENOENT, O_ACCMODE, O_RDONLY};
use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

const TTL: Duration = Duration::from_secs(1);
const ROOT_INO: u64 = 1;
const CONTENT_INO: u64 = 2;
const CONTENT_FILENAME: &str = "content";

pub struct SpaceFuse {
    pipeline: Arc<WritePipeline>,
    capsule_id: CapsuleId,
    capsule_size: u64,
    runtime: Runtime,
}

impl SpaceFuse {
    pub fn new(
        pipeline: Arc<WritePipeline>,
        capsule_id: CapsuleId,
        capsule_size: u64,
    ) -> Result<Self> {
        let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
        Ok(Self {
            pipeline,
            capsule_id,
            capsule_size,
            runtime,
        })
    }

    fn uid_gid() -> (u32, u32) {
        let uid = unsafe { libc::geteuid() } as u32;
        let gid = unsafe { libc::getegid() } as u32;
        (uid, gid)
    }

    fn root_attr() -> FileAttr {
        let (uid, gid) = Self::uid_gid();
        FileAttr {
            ino: ROOT_INO,
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid,
            gid,
            rdev: 0,
            flags: 0,
            blksize: 512,
        }
    }

    fn content_attr(&self) -> FileAttr {
        let (uid, gid) = Self::uid_gid();
        FileAttr {
            ino: CONTENT_INO,
            size: self.capsule_size,
            blocks: (self.capsule_size + 511) / 512,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::RegularFile,
            perm: 0o444,
            nlink: 1,
            uid,
            gid,
            rdev: 0,
            flags: 0,
            blksize: 512,
        }
    }

    fn read_range_blocking(
        &self,
        offset: u64,
        len: usize,
    ) -> std::result::Result<Vec<u8>, anyhow::Error> {
        let pipeline = Arc::clone(&self.pipeline);
        let capsule_id = self.capsule_id;
        self.runtime
            .handle()
            .block_on(async move { pipeline.read_range(capsule_id, offset, len).await })
    }
}

impl Filesystem for SpaceFuse {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent == ROOT_INO && name.to_str() == Some(CONTENT_FILENAME) {
            reply.entry(&TTL, &self.content_attr(), 0);
        } else {
            reply.error(ENOENT);
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        match ino {
            ROOT_INO => reply.attr(&TTL, &Self::root_attr()),
            CONTENT_INO => reply.attr(&TTL, &self.content_attr()),
            _ => reply.error(ENOENT),
        }
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        if ino != CONTENT_INO {
            reply.error(ENOENT);
            return;
        }

        if (flags & O_ACCMODE) != O_RDONLY {
            reply.error(EACCES);
            return;
        }

        reply.opened(0, 0);
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        if ino != CONTENT_INO {
            reply.error(ENOENT);
            return;
        }

        if offset < 0 {
            reply.error(EINVAL);
            return;
        }

        let offset = offset as u64;
        if offset >= self.capsule_size {
            reply.data(&[]);
            return;
        }

        let max_len = (self.capsule_size - offset) as usize;
        let len = std::cmp::min(size as usize, max_len);
        match self.read_range_blocking(offset, len) {
            Ok(bytes) => reply.data(&bytes),
            Err(_) => reply.error(EIO),
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if ino != ROOT_INO {
            reply.error(ENOENT);
            return;
        }

        if offset == 0 {
            let _ = reply.add(ROOT_INO, 0, FileType::Directory, ".");
            let _ = reply.add(ROOT_INO, 1, FileType::Directory, "..");
            let _ = reply.add(CONTENT_INO, 2, FileType::RegularFile, CONTENT_FILENAME);
        }
        reply.ok();
    }
}

pub fn mount_capsule_fuse(
    pipeline: Arc<WritePipeline>,
    capsule_id: CapsuleId,
    capsule_size: u64,
    target: impl AsRef<Path>,
) -> Result<()> {
    let filesystem = SpaceFuse::new(pipeline, capsule_id, capsule_size)?;
    fuser::mount2(
        filesystem,
        target.as_ref(),
        &[
            MountOption::RO,
            MountOption::FSName("space".into()),
            MountOption::AutoUnmount,
        ],
    )?;
    Ok(())
}
