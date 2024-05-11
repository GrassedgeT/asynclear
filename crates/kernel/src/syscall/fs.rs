use core::{ops::Deref, ptr};

use defines::{
    error::{errno, KResult},
    fs::{FstatFlags, IoVec, Stat, StatMode},
};
use triomphe::Arc;
use user_check::{UserCheck, UserCheckMut};

use crate::{
    fs::{self, DEntry, DirFile, File, FileDescriptor, OpenFlags, PagedFile, PathToInode, VFS},
    hart::local_hart,
};

/// 操纵某个特殊文件的底层设备，尤其是字符特殊文件。目前只进行错误检验
///
/// 参数：
/// - `fd` 文件描述符
/// - `request` 请求码，其含义依赖于设备。包含参数是入参还是出参，以及 `argp` 指向的大小
/// - `argp` 一个指针
///
/// 参考：<https://man7.org/linux/man-pages/man2/ioctl.2.html>
pub fn sys_ioctl(fd: usize, request: usize, argp: usize) -> KResult {
    // TODO: [low] 完善 sys_ioctl
    let Some(desc) = local_hart()
        .curr_process()
        .lock_inner_with(|inner| inner.fd_table.get(fd).cloned())
    else {
        return Err(errno::EBADF);
    };

    // TODO: [low] 目前只支持字符设备，块设备不知道会不会用到
    if !desc.meta().mode().contains(StatMode::CHAR_DEVICE) {
        return Err(errno::ENOTTY);
    }

    // TODO: [mid] 实现 `sys_ioctl` 的逻辑
    desc.ioctl(request, argp)
}

// pub fn sys_mkdirat(dirfd: usize, path: *const u8, mode: usize) -> Result {
//     // let path = unsafe { check_cstr(path)? };

//     // info!("mkdir {dirfd}, {path}, {mode:#o}");

//     // let absolute_path = path_with_fd(dirfd, path)?;
//     // // FIXME: 目前这个语义是错误的，创建目录要抽象出另一个函数来
//     // let inode = open_file(absolute_path, OpenFlags::O_CREAT |
// OpenFlags::O_DIRECTORY)?;     // let process = curr_process();
//     // let mut inner = process.inner();
//     // let fd = inner.alloc_fd();
//     // inner.fd_table[fd] = Some(Arc::new(inode));
//     // Ok(0)
//     todo!("[blocked] sys_mkdirat")
// }

fn prepare_io<const READ: bool>(fd: usize) -> KResult<FileDescriptor> {
    let process = local_hart().curr_process();
    let inner = process.lock_inner();
    let file = inner.fd_table.get(fd).ok_or(errno::EBADF)?;
    if (READ && file.readable()) || (!READ && file.writable()) {
        Ok(file.clone())
    } else {
        Err(errno::EBADF)
    }
}

/// 从 `fd` 指示的文件中读至多 `len`
/// 字节的数据到用户缓冲区中。成功时返回读入的字节数
///
/// 参数：
/// - `fd` 指定的文件描述符，若无效则返回 `EBADF`，若是目录则返回 `EISDIR`
/// - `buf` 指定用户缓冲区，若无效则返回 `EINVAL`
pub async fn sys_read(fd: usize, buf: UserCheckMut<[u8]>) -> KResult {
    if fd == 0 {
        trace!("read stdin, len = {}", buf.len());
    } else {
        debug!("read fd = {fd}, len = {}", buf.len());
    }

    let file = prepare_io::<true>(fd)?;
    let nread = file.read(buf).await?;
    Ok(nread as isize)
}

/// 向 fd 指示的文件中写入至多 `len` 字节的数据。成功时返回写入的字节数
///
/// 参数：
/// - `fd` 指定的文件描述符，若无效则返回 `EBADF`，若是目录则返回 `EISDIR`
/// - `buf` 指定用户缓冲区，其中存放需要写入的内容，若无效则返回 `EINVAL`
pub async fn sys_write(fd: usize, buf: UserCheck<[u8]>) -> KResult {
    if fd == 1 || fd == 2 {
        trace!("write stdout/stderr, len = {}", buf.len());
    } else {
        debug!("write fd = {fd}, len = {}", buf.len());
    }

    let file = prepare_io::<false>(fd)?;
    let nwrite = file.write(buf).await?;
    Ok(nwrite as isize)
}

/// 从 fd 中读入数据，写入多个用户缓冲区中。
///
/// 理论上需要保证原子性，也就是说，即使同时有其他进程（线程）对同一个 fd 进行读操作，这一个系统调用也会读入一块连续的区域
///
/// 参数：
/// - `fd` 指定文件描述符
/// - `iovec` 指定 `IoVec` 数组
pub async fn sys_readv(fd: usize, iovec: UserCheck<[IoVec]>) -> KResult {
    if fd == 1 || fd == 2 {
        trace!("writev stdout/stderr");
    } else {
        debug!("writev fd = {fd}");
    }
    let iovec = iovec.check_slice()?;
    let file = prepare_io::<true>(fd)?;
    let mut tot_read = 0;
    // TODO: [mid] 改变 `sys_readv` 的实现方式使其满足原子性
    // NOTE: `IoVec` 带裸指针所以不 Send 也不 Sync，因此用下标而非迭代器来绕一下
    let mut iov_index = 0;
    while iov_index < iovec.len() {
        let buf = UserCheckMut::new(ptr::slice_from_raw_parts_mut(
            iovec[iov_index].iov_base,
            iovec[iov_index].iov_len,
        ));
        iov_index += 1;
        let nread = file.read(buf).await?;
        if nread == 0 {
            break;
        }
        tot_read += nread;
    }
    Ok(tot_read as isize)
}

/// 向 fd 中写入数据，数据来自多个用户缓冲区。
///
/// 理论上需要保证原子性，也就是说，即使同时有其他进程（线程）对同一个 fd 进行写操作，这一个系统调用也会写入一块连续的区域。
///
/// 参数：
/// - `fd` 指定文件描述符
/// - `iovec` 指定 `IoVec` 数组
/// - `vlen` 指定数组的长度
pub async fn sys_writev(fd: usize, iovec: UserCheck<[IoVec]>) -> KResult {
    if fd == 0 {
        trace!("writev stdout");
    } else {
        debug!("writev fd = {fd}");
    }
    let iovec = iovec.check_slice()?;
    let file = prepare_io::<false>(fd)?;
    let mut total_write = 0;
    let mut iov_index = 0;
    // TODO: [mid] 改变 `sys_writev` 的实现方式使其满足原子性
    // NOTE: `IoVec` 带裸指针所以不 Send 也不 Sync，因此用下标而非迭代器来绕一下
    while iov_index < iovec.len() {
        let buf = UserCheck::new(ptr::slice_from_raw_parts(
            iovec[iov_index].iov_base,
            iovec[iov_index].iov_len,
        ));
        iov_index += 1;
        let nwrite = file.write(buf).await?;
        if nwrite == 0 {
            break;
        }
        total_write += nwrite;
    }
    Ok(total_write as isize)
}

/// 打开指定的文件。返回非负的文件描述符，这个文件描述符一定是当前进程尚未打开的最小的那个
///
/// 参数：
/// - `dir_fd` 与 `path` 组合形成最终的路径。
///     - 若 `path` 本身是绝对路径，则忽略。
///     - 若 `dir_fd` 等于 `AT_FDCWD`(-100)，则以 cwd 为起点计算相对路径
/// - `path` 路径，可以是绝对路径或相对路径，以 `/` 为分隔符
/// - `flags` 包括文件打开模式、创建标志、状态标志。
///     - 创建标志如 `CLOEXEC`, `CREAT` 等，仅在打开文件时发生作用
///     - 状态标志影响后续的 I/O 方式，而且可以动态修改
/// - `mode` 是用于指定创建新文件时，该文件的 mode。目前应该不会用到
///     - 它只会影响未来访问该文件的模式，但这一次打开该文件可以是随意的
pub async fn sys_openat(dir_fd: usize, path: UserCheck<u8>, flags: u32, mut _mode: u32) -> KResult {
    let path = path.check_cstr()?;

    let Some(flags) = OpenFlags::from_bits(flags) else {
        todo!("[low] unsupported OpenFlags: {flags:#b}");
    };
    info!("oepnat flags {flags:?}");

    // TODO: [low] OpenFlags::DIRECT 目前是被忽略的

    // 不是创建文件（以及临时文件）时，mode 被忽略
    if !flags.contains(OpenFlags::CREATE) {
        _mode = 0;
    }

    // TODO: [low] 暂时在测试中忽略 `mode` 的检查
    // assert_eq!(_mode, 0, "dir_fd: {dir_fd}, flags: {flags:?}");

    // 64 位版本应当是保证可以打开大文件的
    // TODO: [low] 暂时在测试中忽略 `OpenFlags::LARGEFILE` 的检查
    // assert!(flags.contains(OpenFlags::LARGEFILE));

    // 暂时先不支持这些
    if flags.intersects(OpenFlags::ASYNC | OpenFlags::DSYNC) {
        todo!("[low] unsupported openflags: {flags:#b}");
    }

    let p2i = resolve_path_with_dir_fd(dir_fd, &path)?;
    let ret_fd;
    if let Some(final_dentry) = p2i.dir.lookup(p2i.last_component) {
        // 指定了必须要创建文件，但该文件已存在
        if flags.contains(OpenFlags::CREATE | OpenFlags::EXCL) {
            return Err(errno::EEXIST);
        }

        let new_file = match final_dentry {
            DEntry::Dir(dir) => {
                // 路径名指向一个目录，但是需要写入
                if flags.intersects(OpenFlags::WRONLY | OpenFlags::RDWR) {
                    return Err(errno::EISDIR);
                };
                File::Dir(Arc::new(DirFile::new(dir)))
            }
            DEntry::Paged(paged) => {
                if flags.contains(OpenFlags::DIRECTORY) {
                    return Err(errno::ENOTDIR);
                }
                File::Paged(Arc::new(PagedFile::new(paged)))
            }
        };

        ret_fd = local_hart()
            .curr_process()
            .lock_inner_with(|inner| inner.fd_table.add(FileDescriptor::new(new_file, flags)));
    } else {
        // 找不到该文件，而且又没有指定 `OpenFlags::CREATE`
        if !flags.contains(OpenFlags::CREATE) {
            return Err(errno::ENOENT);
        }
        todo!("[mid] openat create file");
    }
    Ok(ret_fd as isize)
}

fn resolve_path_with_dir_fd(dir_fd: usize, path: &str) -> KResult<PathToInode> {
    let start_dir;
    // 忽略 fd，从当前工作目录开始
    const AT_FDCWD: usize = -100isize as usize;
    // 绝对路径则忽视 fd
    if path.starts_with('/') {
        start_dir = Arc::clone(VFS.root_dir());
    } else {
        let process = local_hart().curr_process();
        let inner = process.lock_inner();
        if dir_fd == AT_FDCWD {
            start_dir = Arc::clone(&inner.cwd);
        } else if let Some(base) = inner.fd_table.get(dir_fd) {
            // 相对路径名，需要从一个目录开始
            let File::Dir(dir) = &**base else {
                return Err(errno::ENOTDIR);
            };
            start_dir = Arc::clone(dir.dentry());
        } else {
            return Err(errno::EBADF);
        }
    }

    fs::path_walk(start_dir, path)
}

pub fn sys_close(fd: usize) -> KResult {
    let process = local_hart().curr_process();
    if process
        .lock_inner_with(|inner| inner.fd_table.remove(fd))
        .is_none()
    {
        return Err(errno::EBADF);
    }

    // TODO: [low] 还要释放相关的记录锁
    // TODO: [mid] 如果文件被 `unlink()` 了且当前 fd 是最后一个引用该文件的，则要删除该文件

    Ok(0)
}

// /// 创建管道，返回 0
// ///
// /// 参数
// /// - `filedes`: 用于保存 2 个文件描述符。其中，`filedes[0]` 为管道的读出端，`filedes[1]` 为管道的写入端。
// pub fn sys_pipe2(filedes: *mut i32) -> Result {
//     let filedes = unsafe { check_slice_mut(filedes, 2)? };
//     let process = curr_process();
//     let mut inner = process.inner();
//     let (pipe_read, pipe_write) = make_pipe();
//     let read_fd = inner.alloc_fd();
//     inner.fd_table[read_fd] = Some(Arc::new(File::new(FileEntity::ReadPipe(pipe_read))));
//     let write_fd = inner.alloc_fd();
//     inner.fd_table[write_fd] = Some(Arc::new(File::new(FileEntity::WritePipe(pipe_write))));
//     info!("read_fd {read_fd}, write_fd {write_fd}");
//     filedes[0] = read_fd as i32;
//     filedes[1] = write_fd as i32;
//     // Ok(0)
//     todo!("[blocked] sys_pipe2")
// }

/// 获取目录项信息
pub fn sys_getdents64(fd: usize, buf: UserCheckMut<[u8]>) -> KResult {
    let process = local_hart().curr_process();
    let Some(File::Dir(dir)) =
        process.lock_inner_with(|inner| inner.fd_table.get(fd).map(Deref::deref).cloned())
    else {
        return Err(errno::EBADF);
    };
    let mut buf = buf.check_slice_mut()?;

    let ret = dir.getdirents(&mut buf)?;
    Ok(ret as isize)
}

/// 操控文件描述符
///
/// 参数：
/// - `fd` 是指定的文件描述符
/// - `cmd` 指定需要进行的操作
/// - `arg` 是该操作可选的参数
pub fn sys_fcntl64(fd: usize, cmd: usize, arg: usize) -> KResult {
    // 未说明返回值的命令成功时都返回 0
    /// 复制该 fd 到大于等于 `arg` 的第一个可用 fd。成功后返回新的 fd
    const F_DUPFD: usize = 0;
    /// 同 `F_DUPFD`，不过为新 fd 设置 `CLOEXEC` 标志
    const F_DUPFD_CLOEXEC: usize = 1030;
    // 下面两个是文件描述符标志操作。目前只有一个 `FD_CLOEXEC` 标志
    /// 返回文件描述符标志，`arg` 将被忽略
    const F_GETFD: usize = 1;
    /// 将文件描述符标志设置为 `arg` 指定的值
    const F_SETFD: usize = 2;
    // 下面两个是文件状态标志操作
    // /// 返回文件访问模式和文件状态标志，`arg` 将被忽略
    // const F_GETFL: i32 = 3;
    // /// 将文件状态标志设置为 `arg` 指定的值。
    // ///
    // /// 在 Linux 上，此命令只能更改 `O_APPEND`、`O_ASYNC`、`O_DIRECT`、`O_NOATIME`` 和 `O_NONBLOCK` 标志。
    // /// 无法更改 `O_DSYNC` 和 `O_SYNC` 标志。
    // const F_SETFL: i32 = 4;

    debug!("fd: {fd}, cmd: {cmd:#x}, arg: {arg:#x}");

    let process = local_hart().curr_process();
    let mut inner = process.lock_inner();

    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let mut desc = inner.fd_table.get(fd).ok_or(errno::EBADF)?.clone();
            if cmd == F_DUPFD_CLOEXEC {
                desc.set_close_on_exec(true);
            }
            let new_fd = inner.fd_table.add_from(desc, arg);
            debug!(
                "dup fd {fd}({}) to {new_fd}, with close_on_exec = {}",
                inner.fd_table.get(new_fd).unwrap().meta().name(),
                cmd == F_DUPFD_CLOEXEC
            );
            Ok(new_fd as isize)
        }
        F_GETFD => {
            let desc = inner.fd_table.get(fd).ok_or(errno::EBADF)?;
            debug!("get the flag of fd {fd}({})", desc.meta().name());
            if desc.flags().contains(OpenFlags::CLOEXEC) {
                Ok(1)
            } else {
                Ok(0)
            }
        }
        F_SETFD => {
            let desc = inner.fd_table.get_mut(fd).ok_or(errno::EBADF)?;
            debug!(
                "set the flag of fd {fd}({}) to {}",
                desc.meta().name(),
                arg & 1 != 0
            );
            desc.set_close_on_exec(arg & 1 != 0);
            Ok(0)
        }
        _ => {
            error!("unsupported cmd: {cmd}, with arg: {arg}");
            Err(errno::EINVAL)
        }
    }
}

// /// 复制文件描述符，复制到当前进程最小可用 fd
// ///
// /// 参数：
// /// - `fd` 是被复制的文件描述符
// pub fn sys_dup(fd: usize) -> Result {
//     // let process = curr_process();
//     // let mut inner = process.inner();
//     // if fd >= inner.fd_table.len() {
//     //     return Err(errno::UNSUPPORTED);
//     // }
//     // if inner.fd_table[fd].is_none() {
//     //     return Err(errno::UNSUPPORTED);
//     // }
//     // let new_fd = inner.alloc_fd();
//     // inner.fd_table[new_fd] =
// Some(Arc::clone(inner.fd_table[fd].as_ref().unwrap()));     // Ok(new_fd as
// isize)     todo!("[blocked] sys_dup")
// }

/// 复制文件描述符 `old_fd` 到指定描述符 `new_fd`
///
/// 如果 `new_fd` 已经被打开，则它被原子地关闭再复用
///
/// 如果 `flags` 包括 CLOEXEC 位，则新描述符会被设置该标志
///
/// 参数：
/// - `old_fd` 被复制的描述符
/// - `new_fd` 要复制到的描述符
/// - `flags` [`OpenFlags`]，实际应该只用到 [`OpenFlags::CLOEXEC`]
pub fn sys_dup3(old_fd: usize, new_fd: usize, flags: u32) -> KResult {
    let Some(flags) = OpenFlags::from_bits(flags) else {
        todo!("[low] unsupported OpenFlags: {flags:#b}");
    };
    let process = local_hart().curr_process();
    let mut inner = process.lock_inner();
    let Some(desc) = inner.fd_table.get(old_fd) else {
        return Err(errno::EBADF);
    };
    if old_fd == new_fd {
        return Err(errno::EINVAL);
    }
    let mut new_desc = desc.clone();
    if flags.contains(OpenFlags::CLOEXEC) {
        new_desc.set_close_on_exec(true);
    }
    inner.fd_table.insert(new_fd, new_desc);
    Ok(new_fd as isize)
}

/// 获取一个文件的信息
///
/// 参数：
/// - `dir_fd` 开始搜索文件的目录，参考 [`sys_openat()`]
/// - `path` 相对路径或绝对路径
/// - `statbuf` 文件信息写入的目的地
/// - `flags` fstat 的一些 flags
pub fn sys_newfstatat(
    dir_fd: usize,
    path: UserCheck<u8>,
    statbuf: UserCheckMut<Stat>,
    flags: usize,
) -> KResult {
    let flags = FstatFlags::from_bits(u32::try_from(flags).map_err(|_e| errno::EINVAL)?)
        .ok_or(errno::EINVAL)?;
    let file_name = path.check_cstr()?;
    if file_name.is_empty() && !flags.contains(FstatFlags::AT_EMPTY_PATH) {
        return Err(errno::ENOENT);
    }
    let mut statbuf = statbuf.check_ptr_mut()?;
    if file_name.is_empty() {
        let process = local_hart().curr_process();
        let inner = process.lock_inner();
        let file = inner.fd_table.get(dir_fd).ok_or(errno::EBADF)?;
        fs::stat_from_meta(&mut statbuf, file.meta());
    } else {
        let p2i = resolve_path_with_dir_fd(dir_fd, &file_name)?;
        let dentry = p2i.dir.lookup(p2i.last_component).ok_or(errno::ENOENT)?;
        fs::stat_from_meta(&mut statbuf, dentry.meta());
    };

    Ok(0)
}

// pub fn sys_fstat(fd: usize, kst: *mut Stat) -> Result {
//     let kst = unsafe { check_ptr_mut(kst)? };
//     let process = curr_process();
//     let inner = process.inner();
//     let Some(Some(file)) = inner.fd_table.get(fd) else {
//         return Err(errno::EBADF);
//     };
//     *kst = file.fstat();

//     Ok(0)
// }

// /// 移除指定文件的链接（可用于删除文件）
// ///
// /// 参数
// ///
// /// TODO: 完善 sys_unlinkat，写文档
// pub fn sys_unlinkat(dirfd: usize, path: *const u8, _flags: u32) -> Result {
//     let path = unsafe { check_cstr(path) }?;
//     let path = path_with_fd(dirfd, path)?;
//     let dir_path;
//     let base_name;
//     if path.ends_with('/') {
//         base_name = path[1..path.len() - 1].split('/').next_back().unwrap();
//         dir_path = &path[..path.len() - base_name.len() - 1];
//     } else {
//         base_name = path[1..].split('/').next_back().unwrap();
//         dir_path = &path[..path.len() - base_name.len()];
//     }
//     let Fat32DirOrFile::Dir(dir) = open_fat_entry(
//         dir_path.to_string(),
//         OpenFlags::O_WRONLY | OpenFlags::O_DIRECTORY,
//     )?
//     else {
//         unreachable!("")
//     };
//     dir.remove(base_name)?;
//     Ok(0)
// }

// pub fn sys_linkat(_old_name: *const u8, _new_name: *const u8) -> Result {
//     // FIXME: 尚未实现 sys_linkat
//     Err(errno::UNSUPPORTED)
// }

// /// TODO: sys_umount 完善，写文档
// pub fn sys_umount(_special: *const u8, _flags: i32) -> Result {
//     Ok(0)
// }

// /// TODO: sys_mount 完善，写文档
// pub fn sys_mount(
//     _special: *const u8,
//     _dir: *const u8,
//     _fstype: *const u8,
//     _flags: usize,
//     _data: *const u8,
// ) -> Result {
//     Ok(0)
// }

// /// TODO: sys_chdir 完善，写文档
// pub fn sys_chdir(path: *const u8) -> Result {
//     let path = unsafe { check_cstr(path)? };

//     let mut new_cwd = if !path.starts_with('/') {
//         format!("/{path}")
//     } else {
//         path.to_string()
//     };
//     // 保证目录的格式都是 xxxx/
//     if !new_cwd.ends_with('/') {
//         new_cwd.push('/');
//     }
//     curr_process().inner().cwd = new_cwd;
//     Ok(0)
// }

// /// 获取当前进程当前工作目录的绝对路径。
// ///
// /// 参数：
// /// - `buf` 用于写入路径，以 `\0` 表示字符串结尾
// /// - `size` 如果路径（包括 `\0`）长度大于 `size` 则返回 ERANGE
// pub fn sys_getcwd(buf: *mut u8, size: usize) -> Result {
//     let process = curr_process();
//     let inner = process.inner();
//     let cwd = &inner.cwd;
//     // 包括 '\0'
//     let buf_len = cwd.len() + 1;
//     if buf_len > size {
//         return Err(errno::ERANGE);
//     }
//     {
//         let buf = unsafe { check_slice_mut(buf, buf_len)? };
//         buf[..buf_len - 1].copy_from_slice(cwd.as_bytes());
//         buf[buf_len - 1] = 0;
//     }
//     Ok(buf as isize)
// }

// /// 等待文件描述符上的事件
// ///
// /// TODO: 暂不实现 sys_ppoll
// pub fn sys_ppoll() -> Result {
//     Ok(1)
// }
