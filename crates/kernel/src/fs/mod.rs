// FIXME: 完整实现 fs 模块并去除 `#![allow(unused)]`
#![allow(unused)]

mod dentry;
mod fat32;
mod file;
mod inode;
mod page_cache;
mod stdio;

use alloc::{collections::BTreeMap, vec::Vec};

use cervine::Cow;
use compact_str::{CompactString, ToCompactString};
use defines::{
    error::{errno, KResult},
    fs::{Stat, StatMode},
};
pub use dentry::{DEntry, DEntryDir, DEntryPaged};
pub use file::{DirFile, FdTable, File, FileDescriptor, OpenFlags, PagedFile};
use inode::InodeMeta;
pub use inode::{DynPagedInode, InodeMode};
use klocks::{Lazy, SpinNoIrqMutex};
pub use page_cache::BackedPage;
use triomphe::Arc;
use uninit::extension_traits::{AsOut, VecCapacity};

use crate::{
    drivers::qemu_block::{BLOCK_DEVICE, BLOCK_SIZE},
    uart_console::println,
};

pub fn init() {
    Lazy::force(&VFS);
}

pub struct VirtFileSystem {
    root_dir: Arc<DEntryDir>,
    mount_table: SpinNoIrqMutex<BTreeMap<CompactString, FileSystem>>,
}

impl VirtFileSystem {
    pub fn root_dir(&self) -> &Arc<DEntryDir> {
        &self.root_dir
    }

    // pub fn mount(&self, mount_point: &str, device_path: CompactString, fs_type: FileSystemType) {}
}

pub static VFS: Lazy<VirtFileSystem> = Lazy::new(|| {
    debug!("Init vfs");
    let root_fs = fat32::new_fat32_fs(
        &BLOCK_DEVICE,
        CompactString::from_static_str("/"),
        CompactString::from_static_str("/dev/mmcblk0"),
    )
    .expect("root_fs init failed");

    root_fs
        .root_dentry
        .read_dir()
        .expect("read root dir failed");
    {
        let children = root_fs.root_dentry.lock_children();
        for name in children.keys() {
            println!("{name}");
        }
    }

    let root_dir = Arc::clone(&root_fs.root_dentry);
    let mount_table = BTreeMap::from([(CompactString::from_static_str("/"), root_fs)]);
    VirtFileSystem {
        root_dir,
        mount_table: SpinNoIrqMutex::new(mount_table),
    }
});

pub struct FileSystem {
    root_dentry: Arc<DEntryDir>,
    device_path: CompactString,
    fs_type: FileSystemType,
    mounted_dentry: Option<DEntry>,
}

pub enum FileSystemType {
    Fat32,
}

/// 类似于 linux 的 `struct nameidata`，存放 path walk 的结果。
///
/// 也就是路径最后一个 component 和前面的其他部分解析得到的目录 dentry
pub struct PathToInode {
    pub dir: Arc<DEntryDir>,
    pub last_component: CompactString,
}

pub fn path_walk(start_dir: Arc<DEntryDir>, path: &str) -> KResult<PathToInode> {
    debug!(
        "walk path: {path}, from {}",
        start_dir.inode().meta().name()
    );
    let mut split = path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .split('/');

    let mut ret = PathToInode {
        dir: start_dir,
        last_component: CompactString::from_static_str("."),
    };

    let Some(mut curr_component) = split.next() else {
        return Ok(ret);
    };

    for next_component in split {
        match ret.dir.lookup(Cow::Borrowed(curr_component)) {
            Some(DEntry::Dir(next_dir)) => ret.dir = next_dir,
            Some(_) => return Err(errno::ENOTDIR),
            None => return Err(errno::ENOENT),
        }
        curr_component = next_component;
    }
    ret.last_component = curr_component.to_compact_string();
    Ok(ret)
}

pub fn find_file(start_dir: Arc<DEntryDir>, path: &str) -> KResult<DEntry> {
    let p2i = path_walk(start_dir, path)?;
    p2i.dir
        .lookup(Cow::Owned(p2i.last_component))
        .ok_or(errno::ENOENT)
}

pub fn read_file(file: &DEntryPaged) -> KResult<Vec<u8>> {
    // NOTE: 这里其实可能有 race？读写同时发生时 `data_len` 可能会比较微妙
    let inner = &file.inode().inner;
    let meta = file.inode().meta();
    let mut ret = Vec::new();
    let out = ret
        .reserve_uninit(meta.lock_inner_with(|inner| inner.data_len))
        .as_out();
    let len = inner.read_at(meta, out, 0)?;
    // SAFETY: `0..len` 在 read_at 中已被初始化
    unsafe { ret.set_len(len) }
    Ok(ret)
}

pub fn stat_from_meta(meta: &InodeMeta) -> Stat {
    let mut stat = Stat::default();
    // TODO: fstat 的 device id 暂时是一个随意的数字
    stat.st_dev = 114514;
    stat.st_ino = meta.ino() as u64;
    stat.st_mode = StatMode::from(meta.mode());
    stat.st_nlink = 1;
    stat.st_uid = 0;
    stat.st_gid = 0;
    stat.st_rdev = 0;
    // TODO: 特殊文件也先填成 BLOCK_SIZE 吧
    stat.st_blksize = BLOCK_SIZE as u32;
    // TODO: 文件有空洞时，可能小于 st_size/512。而且可能实际占用的块数量会更多
    meta.lock_inner_with(|meta_inner| {
        stat.st_size = meta_inner.data_len as u64;
        stat.st_atime = meta_inner.access_time;
        stat.st_mtime = meta_inner.modify_time;
        stat.st_ctime = meta_inner.change_time;
    });
    stat.st_blocks = stat.st_size.div_ceil(stat.st_blksize as u64);
    stat
}
