# FrameLink 帧连 · 3.0.1-dev

Windows 10 / 11 x64 的高性能屏幕推流工具，将屏幕区域通过 NDI 发送到另一台电脑，用于远程分析、AI 识别等处理。支持中心 512×512 / 640×640、240 Hz 等配置。应用采集、调度、发送和桌面界面使用 Rust；libyuv 保留第三方 C/C++ SIMD 实现，通过 FFI 调用。

FrameLink 帧连沿用原 ScreenCast Send 项目的传输实现；更名不改变 NDI 传输协议，也不要求接收端改用新的协议。

![Rust 桌面界面](docs/ui/desktop-1200.png)

界面图由真实 egui 界面离屏渲染，显示的是未发送状态；不是发流实测或虚构性能数据。Windows x64 Release 已交叉编译成功，平台无关测试通过。Windows DXGI / NDI 实际发流、240 fps 稳态、托盘和全屏切换恢复尚未在设备上验证，双机延迟尚未测量。

## 运行

解压 `dist/FrameLink-3.0.1-dev-Windows-x64.zip`，启动 `FrameLink.exe`，保留同目录的 `Processing.NDI.Lib.x64.dll`。NDI DLL 可能需要 Microsoft Visual C++ 2015–2022 x64 Redistributable。

选择显示器和中心 512 / 640 区域，或点击“在屏幕上框选区域”；源名称是接收端发现画面的标识。默认固定 240 fps、NV12、自动优先 DXGI，关闭鼠标和预览。高级设置包含采集方式、视频格式和精确调度；发送时锁定采集参数，停止后可调整。预览可热切换，最多每秒两次，单个最新图像邮箱避免预览积压。

关闭窗口默认收到托盘，发送继续；双击托盘恢复，右键可停止或退出。收起或最小化暂停预览和持续界面重绘。托盘创建失败会保留可用退出路径。开始与停止在后台执行，界面不等待采集或 NDI 网络调用。

Windows 版重复双击程序或快捷方式时，会唤回已运行的窗口；最小化或收到托盘后也能恢复。已有实例的发送状态、采集区域与设置保持原样，后启动的进程通知完成即退出。进程退出或被强制结束后会释放单实例锁，下次可以正常启动。锁按整台机器生效，不同目录的新版也共用；窗口只能在原实例所在的交互桌面恢复。升级前请先退出没有单实例机制的旧版本。

两台电脑需连接同一局域网，在 Windows 防火墙允许专用网络通信。接收端选择 `电脑名 (源名称)`。程序只发送视频，支持 SDR 8 位。DXGI 区域需位于单个非旋转显示器内；跨屏或旋转可用固定帧率 GDI。没有 HDR 保真处理或音频发送。

“发送端处理”是采集、转换、异步提交的平均调用耗时合计，不是网络、解码或接收端分析延迟。Full NDI 仍由 NDI DLL 编码传输，不能保证数学逐像素无损。NV12 / BGRX 应在实际接收端比较效果和延迟。

## 设置兼容

新设置保存在 `%LOCALAPPDATA%\FrameLink\settings.toml`，编辑后自动保存。新配置不存在时，优先导入 `%LOCALAPPDATA%\ScreenCastSendRust\settings.toml`，同时兼容更早的 `%LOCALAPPDATA%\ScreenCastSendCpp\settings.ini`。旧文件均保留；INI 导入支持 UTF-16 中文源名、负坐标、`lowLatency` 和原中心预设索引。自定义区域重启后保持自定义。

源码仓库保留原 C++ 源码 `src/` 用于对照。原始 EXE 和本机生成的测试包不随源码提交。Rust 构建使用 `rust/`，不编译或调用旧 C++ 发送器。

## 构建与验证

Cargo 包与库名为 `framelink`，可执行目标名为 `FrameLink`（Windows 输出 `FrameLink.exe`）。需要 Rust 1.88+。Windows 推荐 Visual Studio 2022 C++ Build Tools / Windows SDK：

仓库包含构建所需的 libyuv 源码和许可证，不包含 NDI DLL、EXE 或已打包的 ZIP。Rust 编译本身无需安装 NDI SDK；运行时须将合法获取的 Windows x64 `Processing.NDI.Lib.x64.dll` 放在 `FrameLink.exe` 旁。使用打包脚本或旧 C++ 构建脚本前，将该 DLL 放到项目根目录，脚本会将它复制到输出目录。NDI 许可文件已保留在仓库中。

```powershell
cargo build --locked --release --target x86_64-pc-windows-msvc --bin FrameLink
python tools/build_rust.py --target x86_64-pc-windows-msvc --skip-build
```

程序输出到 `target/x86_64-pc-windows-msvc/release/FrameLink.exe`。`build_rust.py` 复制 NDI DLL、许可证、第三方声明和使用说明，生成完整 ZIP。未设置 `--skip-build` 时先构建。MSVC 路径已提供，尚未实际执行。

已有 MinGW-w64 的 macOS / Linux 可交叉编译（本轮实际验证路径）：

```sh
rustup target add x86_64-pc-windows-gnu
python3 tools/build_rust.py
```

MinGW 链接器和资源编译器需在 PATH；依赖版本由 `Cargo.lock` 固定。libyuv 来自仓库已有 `third_party/libyuv/`，不需要安装 NDI SDK 开发头文件。

本机测试与真实界面快照：

```sh
cargo test --locked --lib
cargo check --locked --target x86_64-pc-windows-gnu --all-targets
cargo run --locked --bin FrameLink -- --ui-snapshot docs/ui
```

macOS 可打开界面用于设计预览，但采集和发送明确只支持 Windows。命令行 `--ui-snapshot` 只渲染界面，不采集屏幕、不启动发送，也不保存用户配置。

Windows 单实例自动验收（先退出已运行的 FrameLink）：

```powershell
powershell -ExecutionPolicy Bypass -File tests/SingleInstanceVerification.ps1 -Executable .\dist\FrameLink-3.0.1-dev-Windows-x64\FrameLink.exe
```

脚本也随包放在 `tests/` 中，在解压目录运行时可省略 `-Executable`。它验证重复启动、最小化恢复、托盘恢复、并发启动和强制结束后重新启动，仅清理它自己启动的进程，不编辑用户配置。若设置了 `close_to_tray = false`，会明确跳过关闭到托盘一项。测试期间请勿手动操作或另外启动程序；发送中的状态保持需另按迁移说明进行人工验收。当前环境没有 Windows 设备，此脚本尚未运行，交叉编译不代表上述行为已实机通过。开发用 `--ui-snapshot` 绕过单实例，不会唤回正在运行的窗口。

源文件索引、功能与验证边界见 [Rust 迁移说明](docs/Rust迁移说明.md)。旧 `延迟实测.md`、`tests/UiVerification.ps1`、`tests/RunBenchmarks.ps1` 都对应 C++ 版；它们的 Win32 控件脚本不能用于新的 egui 界面。

NDI® is a registered trademark of Vizrt NDI AB. See https://ndi.video/ and the included NDI license files. libyuv 与 Rust 依赖声明随测试包提供。
