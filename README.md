# FrameLink 帧连

Windows 10 / 11 x64 屏幕推流工具，通过 NDI 将指定屏幕区域发送到另一台电脑。支持中心区域、桌面框选、NV12 / BGRX、托盘运行和重复启动恢复窗口。

![FrameLink](docs/ui/desktop-1200.png)

当前版本：`3.0.1-dev`。应用代码使用 Rust，颜色转换使用 libyuv 的 SIMD 实现。

## 使用

解压构建包，启动 `FrameLink.exe`，保留同目录的 `Processing.NDI.Lib.x64.dll`。NDI DLL 可能需要 Microsoft Visual C++ 2015–2022 x64 运行库。

1. 选择显示器和采集区域，填写 NDI 源名称。
2. 点击“开始发送”。首次默认中心 512×512、240 fps、NV12、自动采集，关闭鼠标和预览。
3. 在同一局域网的接收软件中选择 `电脑名 (源名称)`。Windows 防火墙需允许专用网络通信。

发送时可切换预览；调整采集参数需先停止。关闭窗口默认收到托盘并继续发送，双击托盘图标恢复，右键可停止或退出。再次启动程序会恢复已有窗口，保留原发送任务和设置。

设置保存在 `%LOCALAPPDATA%\FrameLink\settings.toml`，编辑后自动保存。首次启动可导入 `%LOCALAPPDATA%\ScreenCastSendRust\settings.toml` 或 `%LOCALAPPDATA%\ScreenCastSendCpp\settings.ini`，不会删除原配置。

程序只发送 SDR 8 位视频，不发送音频。DXGI 区域需位于单个非旋转显示器内；跨屏或旋转显示器可使用固定帧率 GDI。“发送端处理”只统计采集、转换和 NDI 提交调用的平均耗时，不包含网络、解码或分析延迟。Full NDI 不能保证逐像素数学无损。

## 构建

需要 Rust 1.88+、Python 3 和 Windows x64 编译工具链。仓库包含所需的 libyuv 源码、Windows 资源和许可证；不包含 NDI DLL、EXE 或打包 ZIP。编译无需安装 NDI SDK，运行和打包需要自行提供合法获取的 `Processing.NDI.Lib.x64.dll`。

Windows 安装 Visual Studio 2022 C++ Build Tools / Windows SDK，将 NDI DLL 放到项目根目录后运行：

```powershell
python tools/build_rust.py --target x86_64-pc-windows-msvc
```

macOS / Linux 安装 MinGW-w64，确保 `x86_64-w64-mingw32-g++` 和 `x86_64-w64-mingw32-windres` 在 PATH 中：

```sh
rustup target add x86_64-pc-windows-gnu
python3 tools/build_rust.py
```

输出为 `dist/FrameLink-3.0.1-dev-Windows-x64.zip`。已有 Release 编译结果时，可加 `--skip-build` 只打包。只编译程序可使用：

```sh
cargo build --locked --release --target x86_64-pc-windows-msvc --bin FrameLink
```

## 开发与验证

```sh
cargo test --locked --lib
cargo check --locked --target x86_64-pc-windows-gnu --all-targets
cargo run --locked --bin FrameLink -- --ui-snapshot docs/ui
```

macOS 可预览界面，采集和发送仅支持 Windows。界面截图来自程序离屏渲染，不启动采集或发送。

已完成本机逻辑测试、Windows GNU 交叉编译和界面快照检查。MSVC 构建、Windows 采集 / NDI 运行、托盘与窗口恢复、240 fps 稳态及双机延迟仍需实机验证。模块说明和验收步骤见 [开发说明](docs/开发说明.md)。

NDI® is a registered trademark of Vizrt NDI AB. See [NDI](https://ndi.video/), [NDI 许可协议](<NDI License Agreement.pdf>) and [依赖许可](Processing.NDI.Lib.Licenses.txt). libyuv 与 Rust 依赖许可由打包脚本一并收集。
