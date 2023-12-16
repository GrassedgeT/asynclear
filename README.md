# asynclear

基于 Rust 的异步操作系统内核。

## 如何运行

1. 安装 qemu-system-riscv64，版本 7.0.x 或 7.1.x（7.2.0 有未知问题）
   - Windows: <https://qemu.weilnetz.de/w64/2022/qemu-w64-setup-20220831.exe>
   - Linux：<https://www.qemu.org/download/#linux>。找不到合适版本可能得自己从源码编译，[参考下文](#在-linux-上编译-qemu-system-riscv64)
2. 安装 rust 环境，请务必用[官方提供的安装方式](https://www.rust-lang.org/learn/get-started)
3. 运行 cargo env
4. 运行 cargo qemu

### 在 Linux 上编译 qemu-system-riscv64

```sh
wget https://download.qemu.org/qemu-7.0.0.tar.xz
tar xvJf qemu-7.0.0.tar.xz
cd qemu-7.0.0
./configure --target-list=riscv64-softmmu --prefix=/opt/qemu-7.0.0 --enable-virtfs
make -j12
sudo make install
# 然后将 qemu-system-riscv64 添加到 PATH 里
```

## 开发指南

1. 若使用 vscode + rust-analyzer，建议将以下设置加入 vscode 设置 `"rust-analyzer.check.overrideCommand": ["cargo", "check", "--workspace", "--message-format=json", "--bins", "--target", "riscv64imac-unknown-none-elf", "--exclude", "xtask"],`。注意，在这种情况下，由于 xtask 目录被排除，vscode 中只会为 xtask 提供基本的补全、跳转，错误信息不会显示。

## Todo

### 基础设施

- [ ] Testing
- [ ] 栈回溯（基于 span）
- [x] Logging（日志事件、span 上下文）
- [ ] Profiling（可视化）

### 比较独立的工作

- [ ] buddy_system_allocator 增加调试信息，包括碎片率、分配耗时等等
- [ ] frame_allocator 可以试着用别的测试测试性能
- [ ] 某些堆分配可以用 Allocaotr API 试着优化
- [ ] trap 改为 vector 模式（会有优化吗？）

### 具体任务

按优先级排列：

- [ ] rCore-Tutorial I/O 设备管理（中断）
- [ ] 内核线程
- [ ] Testing
- [ ] kernel_tracer（Profiling 可视化）
- [ ] 用户指针检查通过内核异常来做
- [ ] CoW、Lazy Page，顺便重构 memory 模块
- [ ] RCU
- [ ] 信号机制
- [ ] async-task 和 embassy 的原理
- [ ] kernel 内容能否放入 huge page？
- [ ] 虚拟文件系统和页缓存
- [ ] 思考 `Future` 和 `Send`

## 参考资料

- [riscv sbi 规范](https://github.com/riscv-non-isa/riscv-sbi-doc)
    - binary-encoding 是调用约定
    - ext-debug-console 是一种更好的输入和输出控制台的方式
    - ext-legacy 是一些旧版的功能
