# CaretLang

Windows 11 输入法状态提示工具。切换中英文后，在文字光标旁短暂显示“中 / 英”。默认 0.8 秒，支持深浅色、托盘设置和按需开机启动。

**当前版本：0.1.0 预览版，MIT 开源。** 首版仅适配微信输入法，部分自绘输入框仍有定位限制。已完成 29 项自动测试及部分应用烟测，完整兼容性验收仍待完成。详情见 [验证记录](docs/compatibility.md)。

[排障与验证指南](docs/troubleshooting.md) · [下载 Windows x64 便携包](https://github.com/debugrao-coder/caretlang/releases) · [反馈问题](https://github.com/debugrao-coder/caretlang/issues)

## 使用

解压便携包到固定目录，双击 `caretlang.exe`。应用常驻任务栏通知区域；再次启动不会创建第二个实例。若图标收在折叠菜单中，展开通知区域可找到青色 CaretLang 图标。

需要 Windows 11 x64 和 Visual C++ x64 运行库。当前构建依赖 `VCRUNTIME140.dll`，尚未做纯净系统验证。

- 在微信输入法中按 Shift 切换中英文，提示显示在文字光标右上方；不能定位光标时退到当前窗口右上角。
- 左键或右键点击托盘图标，可暂停提示，选择 0.5／0.8／1.2 秒、切换主题、预览提示或退出。
- 开机启动默认关闭，只在你点击菜单开关后写入当前用户的启动项。启用后请保持可执行文件路径不变；移动前先取消开机启动。
- 设置保存在 `%LOCALAPPDATA%\CaretLang\settings.ini`。应用退出后可移除便携文件；若曾开启开机启动，先从菜单关闭。

提示窗口不抢键盘焦点、不拦截鼠标，不会出现在任务栏和 Alt+Tab 列表。动画跟随 Windows 的动画开关。

## 检测与边界

本版专门按微信输入法的开放状态判断中英文：读取到 0 为英文、1 为中文；不使用在 Codex 中可能恒为 0 的转换模式。其他输入法尚不支持，请在微信输入法下使用。Shift 等按键只触发复查，不用于推算中英文。无法读取时不猜测状态。应用或输入框获得焦点后，首次读到确定状态就显示一次当前“中 / 英”；后续只有状态变化才再次提示。没有确定状态时不新建提示；同一焦点已有提示时，对短暂读取失败保留最近确认状态最多 200 毫秒，持续不可读则隐藏。应用激活事件会尝试重新提示，但 Codex 的 Alt+Tab 切回场景复测未通过，仍属已知问题。

光标定位依次使用 Win32、MSAA 与 UI Automation。辅助查询在独立子进程中运行，单次查询超过 180 毫秒会终止并退避重启。主程序退出时子进程自动清理。

程序不读取输入正文、不保存按键记录、不联网。可选诊断日志只记录程序启动、检测到的模式、光标是否可用、DPI 和错误。发布包不默认开启诊断。

首版不保证管理员窗口、安全桌面、远程桌面、游戏及没有提供可访问光标的自绘输入框。定位回退只代表提示可见，不等于精确光标定位成功。微信输入法版本变化后需要重新验证。

## 构建与测试

需要 Windows x64、Rust MSVC 工具链和 Windows SDK。依赖版本固定在 `Cargo.lock`。

```powershell
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked --bin caretlang
./scripts/Package.ps1
```

便携包输出到 `dist/CaretLang-portable.zip`，同目录包含 SHA-256 校验文件。

开发诊断命令：

```powershell
./target/release/caretlang.exe --diagnostic ./artifacts/runtime.log
./target/release/caretlang.exe --preview --dark
./target/release/caretlang.exe --export-preview ./artifacts/preview
./scripts/Measure-Idle.ps1 -Seconds 300
```

`--preview` 是独立视觉预览：中英文各显示约 30 秒后退出，不作为输入法检测证据。运行预览前需退出已有实例。`--export-preview` 导出实际 Direct2D 位图，包含两种主题和两种字样。性能脚本测量已经运行的发布程序及其子进程，不会自动启动应用或模拟输入。

## 代码结构

`core` 管理状态变化、过期结果、位置和透明度；`platform` 进行有超时的 Win32 查询；`uia` 隔离辅助定位；`render` 缓存原生位图；`settings` 和 `tray` 管理本地偏好与托盘。窗口回调只排队，不执行跨进程查询。
