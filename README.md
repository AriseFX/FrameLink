# FrameLink 帧连

屏幕区域推流工具，通过 NDI 将指定区域发送到另一台电脑。支持 Windows 10 / 11 x64，新增 macOS 13+ 的原生采集与发送适配。

![FrameLink](docs/ui/desktop-1200.png)

当前版本：`3.1.2-dev`。应用代码使用 Rust，颜色转换使用 libyuv 的 SIMD 实现。

## 使用

Windows：解压构建包，启动 `FrameLink.exe`，保留同目录的 `Processing.NDI.Lib.x64.dll`。NDI DLL 可能需要 Microsoft Visual C++ 2015–2022 x64 运行库。

macOS：解压对应架构的构建包，打开 `FrameLink.app`。完整包内置 macOS NDI 6.3.2 运行库，无需另装 NDI。首次点击发送时申请屏幕录制权限；授权后退出并重新打开程序。

1. 选择显示器和采集区域，填写 NDI 源名称。
2. 点击“开始发送”。首次默认中心 512×512、240 fps、NV12、自动采集，关闭鼠标和预览。
3. 在同一局域网的接收软件中选择 `电脑名 (源名称)`。Windows 防火墙需允许专用网络通信。

Windows 可在“发送网卡”中选择有线网卡，查看本机 IPv4 和连接速率。切换后点击“应用并重启”，再开始发送；“自动选择”沿用原有 NDI 设置。选择会记住网卡本身，下次启动按当前 IP 配置，适用于有线、Wi-Fi 或虚拟网卡并存的情况。单独指定网卡不保证降低延迟。

发送时可切换预览；调整采集参数需先停止。Windows 关闭窗口默认收到托盘并继续发送，双击托盘图标恢复，右键可停止或退出；再次启动程序会恢复已有窗口，保留原发送任务和设置。

Mac 使用 ScreenCaptureKit，支持中心预设、手动坐标、NV12 / BGRX 和鼠标显示；暂不提供桌面框选、托盘或 Windows 的精确调度模式，关闭窗口直接退出。帧率设置是采集上限，只发送实际收到的新帧，受显示器和桌面合成速率限制；填写 240 fps 不代表能产生 240 张新画面。

设置保存在 `%LOCALAPPDATA%\FrameLink\settings.toml`，编辑后自动保存。首次启动可导入 `%LOCALAPPDATA%\ScreenCastSendRust\settings.toml` 或 `%LOCALAPPDATA%\ScreenCastSendCpp\settings.ini`，不会删除原配置。

macOS 设置保存在 `~/.config/FrameLink/settings.toml`。两平台的采集方式和托盘设置不同，不建议直接共用同一份配置。

程序只发送 SDR 8 位视频，不发送音频。DXGI 区域需位于单个非旋转显示器内；跨屏或旋转显示器可使用固定帧率 GDI。“发送端处理”只统计采集拷贝、转换和 NDI 提交调用的平均耗时；等待桌面出新帧的空闲时间单独显示为“等新帧”，跟随刷新率模式下它接近一个刷新周期，但帧一出现就立即发出，不增加延迟。两者都不包含网络、解码或分析延迟。Full NDI 不能保证逐像素数学无损。

## 构建

需要 Rust 1.88+、Python 3 和目标平台编译工具链。仓库包含所需的 libyuv 源码、资源和许可证；不包含 NDI 二进制运行库或构建包。编译无需安装 NDI SDK。

macOS 安装 Xcode Command Line Tools（包含 Swift 和 macOS SDK）后运行：

```sh
python3 tools/build_macos.py
```

默认构建本机架构，输出 `dist/FrameLink-3.1.2-dev-macOS-arm64.zip` 或 `macOS-x64.zip`，内含 `.app` 与 NDI 运行库。首次打包会从 [NDI 官方下载地址](https://ndi.link/NDIRedistV6Apple)获取固定版本的运行库，校验安装包签名和 SHA-256 后提取到本机 `runtime/macos/` 缓存，不执行系统安装，也不将二进制提交到源码仓库。

添加 `--target x86_64-apple-darwin` 可选择 Intel 目标，需先通过 rustup 安装对应目标。`--ndi-runtime /绝对路径/libndi.dylib` 可指定其他运行库；`--without-ndi` 仅用于界面开发包。命令行可通过 `FRAMELINK_NDI_RUNTIME` 指定运行库绝对路径。应用使用本机临时签名，未做 Apple 公证；内置运行库保留原厂签名，许可文件也放在应用包内。

Windows 安装 Visual Studio 2022 C++ Build Tools / Windows SDK，将 `Processing.NDI.Lib.x64.dll` 放到项目根目录后运行：

```powershell
python tools/build_rust.py --target x86_64-pc-windows-msvc
```

macOS / Linux 安装 MinGW-w64，确保 `x86_64-w64-mingw32-g++` 和 `x86_64-w64-mingw32-windres` 在 PATH 中：

```sh
rustup target add x86_64-pc-windows-gnu
python3 tools/build_rust.py
```

Windows 本机同样需要完整的 MinGW-w64：`bin` 里除编译器外还要有 `x86_64-w64-mingw32-windres` 以及 `libstdc++-6.dll`、`libwinpthread-1.dll`、`libgcc_s_seh-1.dll`。缺少运行库时 `g++` 直接以 0xC0000135 退出，`cargo clippy` 重编 build script 时还会报 windres 缺失，只有已缓存的 `cargo build` 能侥幸通过。

输出为 `dist/FrameLink-3.1.2-dev-Windows-x64.zip`。已有 Release 编译结果时，可加 `--skip-build` 只打包。只编译程序可使用：

```sh
cargo build --locked --release --target x86_64-pc-windows-msvc --bin FrameLink
```

## 开发与验证

```sh
cargo test --locked --lib
cargo check --locked --target x86_64-pc-windows-gnu --all-targets
cargo run --locked --bin FrameLink -- --ui-snapshot docs/ui
```

颜色转换性能基准，不需要 NDI 或屏幕权限，每项运行 2 秒并输出 JSON：

```sh
cargo run --locked --release --bin FrameLink -- --benchmark-pixels 2
```

Mac 构建包无需安装 Rust，在解压目录直接运行：

```sh
./FrameLink.app/Contents/MacOS/FrameLink --benchmark-pixels 2
```

检查实际加载的 NDI 路径、版本和初始化结果，不采集或发送屏幕：

```sh
./FrameLink.app/Contents/MacOS/FrameLink --check-ndi
```

基准覆盖 512 / 640 的 NV12 转换和 BGRX 复制，仅衡量这两个 CPU 环节，不包含采集、NDI 编码、网络或端到端延迟。界面截图来自程序离屏渲染，不启动采集或发送。

此版本将 Windows DXGI 释放上一帧的时机移到下次获取之前，按[微软建议](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-releaseframe)减少中间桌面更新带来的无用 GPU 复制。Windows 实际性能收益尚未测量。

Windows 发送诊断：先在界面保存采集参数并退出程序，保持接收软件运行，再在解压目录用 PowerShell 运行：

```powershell
Start-Process -FilePath .\FrameLink.exe -ArgumentList '--measure-send','10','sender-report.json' -Wait
```

启动后确认接收端已重新连接同名源。支持采样 1–60 秒；采样期间保持画面持续运动。报告包含原始样本及帧释放、获取、Map 等待、转换、NDI 调用的 p50 / p95 / p99 / 最大值，以及提交前后的桌面更新帧龄。帧龄来自同机 QPC，指显示器最后一次桌面更新至观测时刻，不代表 ROI 内容、网络或识别闭环延迟。诊断会临时关闭预览，不修改保存的配置，并遵守单实例限制。详细边界见[开发说明](docs/开发说明.md)。

实际采集、NDI 运行和双机延迟仍需实机验收；Mac 的测试结果不能代替 Windows DXGI 测试。模块说明和验收步骤见 [开发说明](docs/开发说明.md)。

NDI® is a registered trademark of Vizrt NDI AB. See [NDI](https://ndi.video/), [NDI 许可协议](<NDI License Agreement.pdf>) and [依赖许可](Processing.NDI.Lib.Licenses.txt). libyuv 与 Rust 依赖许可由打包脚本一并收集。
