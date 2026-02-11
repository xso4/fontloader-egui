# NewFontLoader (egui)

这是一个使用 Rust 和 egui 重写的 [FontLoaderSub](https://github.com/yzwduck/FontLoaderSub) 移植版本。它旨在为 ASS/SSA 字幕自动加载所需的字体文件，提供更现代的 UI 交互和更稳定的性能。

## 功能特性

- **自动关联加载**：拖入字幕文件，程序会自动扫描同目录及子目录下的字体文件并加载。
- **多种模式支持**：
  - **无残留模式 (默认)**：程序关闭时自动卸载所有已加载字体，不占用系统资源。
  - **普通模式**：手动控制加载与卸载。
- **增强型清理**：提供“强制清理目录残留”功能，可一键解除特定目录下所有字体的系统占用。
- **便携性**：配置与缓存均保存在软件同级目录下，不污染系统路径。
- **现代化 UI**：基于 egui 构建，支持黑暗模式，支持高分屏缩放，界面响应迅速。
- **中文字体支持**：内置微软雅黑及系统符号字体支持，杜绝乱码。

## 使用方法

1.  下载并运行 `fontloader-egui.exe`。
2.  将 `.ass` / `.ssa` 字幕文件或包含字体的文件夹拖入程序窗口。
3.  点击 **开始处理** 按钮，程序将自动分析字幕并加载对应字体。
4.  观看结束后，直接关闭程序或点击 **卸载已加载字体**。

## 编译运行

如果你想从源码构建：

```bash
# 克隆仓库
git clone https://github.com/你的用户名/fontloader-egui.git
cd fontloader-egui

# 编译并运行
cargo run --release
```

*注意：编译环境需为 Windows，并安装有 Rust 工具链。*

## 技术栈

- **语言**: Rust
- **UI 框架**: [egui](https://github.com/emilk/egui) / [eframe](https://github.com/emilk/egui/tree/master/crates/eframe)
- **系统调用**: [windows-rs](https://github.com/microsoft/windows-rs) (GDI Font API)
- **文件对话框**: [rfd](https://github.com/PolyhedralDev/rfd)

## 致谢

- 感谢 [yzwduck/FontLoaderSub](https://github.com/yzwduck/FontLoaderSub) 提供的原始 C++ 实现思路。
- 灵感来源于 CryptWizard 的原版 FontLoader。

---
License: MIT or Apache-2.0
