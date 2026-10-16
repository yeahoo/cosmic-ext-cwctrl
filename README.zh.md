# cosmic-ext-cwctrl

[![CI](https://github.com/yeahoo/cosmic-ext-cwctrl/actions/workflows/ci.yml/badge.svg)](https://github.com/yeahoo/cosmic-ext-cwctrl/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[English](README.md) | **简体中文**

在 COSMIC™ 桌面上，用命令行查询和控制窗口，并注入鼠标与键盘输入。

枚举与筛选顶层窗口，聚焦 / 关闭 / 最大化 / 移动它们，读取窗口几何信息，并通过
内核 `uinput` 驱动指针和键盘。全部由一个约 2 MB 的同步二进制完成，没有解释器，
也没有 async runtime，控制类命令约 1 毫秒返回。

## 为什么要重写

上游的 Python 实现每次调用约 300ms：解释器启动、解析 264KB 协议库，以及
**无条件等待合成器那批约 150ms 的数据**。绑定到快捷键时这个延迟很明显。
本实现是一个纯同步的 Wayland 客户端，没有 async runtime，也没有解释器。

| | cwctrl | 上游 (Python) |
|---|---|---|
| `activate` / `close` / `maximize` 等 | **1.0 ms** | 307 ms |
| `list -n` | **1.0 ms** | — |
| `list`（含 geometry） | 150 ms | 317 ms（且无 geometry） |
| 二进制 | 2.0 MB | 5.8 MB venv |
| 常驻内存 | ~4 MB | ~26 MB |

## 安装

```bash
cargo install --git https://github.com/yeahoo/cosmic-ext-cwctrl
```

或从源码构建：

```bash
git clone https://github.com/yeahoo/cosmic-ext-cwctrl
cd cosmic-ext-cwctrl
cargo build --release
install -Dm755 target/release/cwctrl ~/.local/bin/cwctrl
```

需要 Rust 1.86+ 以及运行中的 COSMIC 会话（`cosmic-comp`）。

## 用法

```
cwctrl <命令> [参数...] [查询]

  state  [查询]                     以 JSON 输出窗口状态（含 geometry）
  list   [-n] [查询]                同 state（JSON 数组）
  activate [查询]                   激活该应用最近使用的那个窗口
  close    [查询]                   关闭匹配的窗口
  minimize <true|false> [查询]      最小化 / 取消最小化
  maximize <true|false> [查询]      最大化 / 取消最大化
  fullscreen <true|false> [查询]    全屏 / 取消全屏
  sticky   <true|false> [查询]      固定 / 取消固定
  move_to  <工作区> <显示器> [查询] 移动到指定工作区
  cycle    [-b] [查询]              在同组匹配窗口间循环切换焦点
  interfaces                        列出合成器暴露的 Wayland 接口
  mousemove <x> <y>                 指针绝对定位（逻辑像素）
  mousemove -r <dx> <dy>            指针相对移动
  mousedown|mouseup <left|right|middle>  按下 / 松开鼠标键
  click [left|right|middle] [--at X Y] [--count N]
  scroll <up|down|left|right> [N]   滚轮
  keydown <键> / keyup <键>         按下 / 松开按键
  key <键>                          按下并松开（组合键: ctrl+alt+t）

  -n, --no-wait   不等待 geometry（约 1ms，geometry 为 null）
```

### 查询语法

```
字符串字段: id, app_id, title, workspace.name, output.name
运算符:     =   相等
            !=  不等
            ~=  正则匹配
            值必须加引号；后缀 i 表示忽略大小写
布尔字段:   is_active, is_active_app_id, is_maximized, is_minimized,
            is_fullscreen, is_sticky, workspace.is_visible,
            workspace.has_focus, output.has_focus
组合:       and / or / not，以及括号
```

示例：

```bash
cwctrl list "app_id ~= 'chrome'i"
cwctrl list "workspace.is_visible and not is_active"
cwctrl activate "title ~= 'wezterm'"
cwctrl maximize true "app_id = 'com.microsoft.VSCode'"
cwctrl move_to 2 HDMI-A-1 "app_id = 'google-chrome'"
```

典型的 run-or-raise 快捷键（退出码 `10` 表示没有匹配）：

```bash
cwctrl activate "app_id = 'com.system76.CosmicTerm'" || cosmic-term
```

### 激活哪一个窗口

`activate` 只激活**一个**窗口，而不是所有匹配项：

1. 焦点已经在该应用的某个窗口上 → 不做任何事；
2. 否则激活**最近使用过**的那个。

这个顺序记录在 `$XDG_RUNTIME_DIR/cwctrl-focus-history`。Wayland 和 COSMIC 都
没有暴露焦点栈，所以只能由客户端自己记；`cycle` 会更新同一份列表，两个命令
保持一致。

这正是让 `Alt+S` 回到你**真正**在用的那个终端、而不是枚举顺序里第一个的
原因：

```text
wezterm1  wezterm2  wezterm3     # 你之前在 wezterm3
Alt+E  ->  chrome
Alt+S  ->  wezterm3              # 而不是 wezterm1
```

其余命令（`close`、`maximize` 等）仍然作用于所有匹配窗口。

### 输出

```json
[
  {
    "id": "jTQOIgg8kjnhQtLhJ7fOawc79y1MXJOb",
    "app_id": "google-chrome",
    "title": "Node.js — Google Chrome",
    "is_active": false,
    "is_active_app_id": false,
    "is_maximized": false,
    "is_minimized": false,
    "is_fullscreen": false,
    "is_sticky": false,
    "workspace": { "name": "1", "is_visible": true, "has_focus": true },
    "output":    { "name": "HDMI-A-1", "has_focus": true },
    "geometry":  { "output": "HDMI-A-1", "x": 2649, "y": 32,
                   "width": 2645, "height": 1486 }
  }
]
```

`geometry` 使用**逻辑像素**，`x`/`y` 相对该显示器左上角（逻辑像素 = 物理像素 /
缩放因子），可直接用于同样坐标系的 `cosmic-screenshot`。

`id` 是 `ext_foreign_toplevel_handle_v1` 的 identifier，跨进程稳定。

`--no-wait` 下未能读取的字段输出 `null` 而非 `false`，避免把"未知"误报成"否"。

### 输入注入（鼠标与键盘）

```bash
cwctrl mousemove 2648 464            # 绝对定位，逻辑像素（与 geometry 同一坐标空间）
cwctrl mousemove -r 40 30            # 相对移动
cwctrl mousemove 2648 464 --from 1200 800 --duration 600   # 平滑移动，观众能看清轨迹
cwctrl click left --at 2648 464      # 先移动再点击
cwctrl click left --at 2648 464 --from 1200 800 --duration 600   # 平滑移动后点击
cwctrl click --count 2               # 在当前位置双击
cwctrl drag 1200 800 2000 1200       # 在 (1200,800) 按下, 拖到 (2000,1200), 松开
cwctrl drag 1200 800 2000 1200 1600 1500   # 经过中间点 (可给任意多个点)
cwctrl drag 1200 800 2000 1200 --duration 800 --from 600 400   # 平滑靠近后拖动
cwctrl mousedown right && cwctrl mouseup right
cwctrl scroll down 3
cwctrl key ctrl+alt+t                # 按下并松开；支持组合键
cwctrl keydown shift && cwctrl keyup shift
cwctrl type 'hello world'            # 以模拟按键输入 ASCII 文本
cwctrl type 'hello' --delay 150      # 放慢逐字输入，便于观众看清
cwctrl type --paste '任意文本'       # 用剪贴板 (Ctrl+V) 输入任意文本（含中文）
```

`enter` / `leave` / `over` 不是独立命令：它们是合成器在指针进入/离开窗口时
自动产生的事件，`mousemove` 就能触发。

注入走内核 **uinput**，而不是 Wayland 协议——Wayland 刻意不提供指针注入协议，
COSMIC 也没有暴露 `zwlr_virtual_pointer_manager_v1`。这需要 `/dev/uinput` 的写
权限；udev 的 `uaccess` 规则通常已授予当前登录用户，否则请把用户加入 `input`
组。

限制：

- 坐标是**逻辑像素**，与 `geometry` 同一空间，所以 `click --at` 可以直接用
  `cwctrl list` 给出的窗口中心。
- 注入是全局的：作用于当前指针位置 / 键盘焦点，而不是你指定的窗口。要点击
  某窗口请先 `mousemove` 过去；发按键前可先 `activate`。
- `-r`（相对）会经过合成器的指针加速，因此是近似的；需要精确时用绝对
  `mousemove`。
- 平滑移动需要显式给出 `--from X Y`：Wayland 无法读取全局指针位置，起点无法
  自动获得；不给 `--from` 时仍是过去的瞬移。`--duration MS` 指定时长，
  `--ease in-out|linear` 指定缓动曲线（默认 `in-out`）。
- `keydown`/`keyup` 每次调用都会新建并销毁设备，所以跨两次调用按住的修饰键
  不会保持；一次调用内请用组合键（`key ctrl+t`）。
- `type` 按 US 布局模拟按键，只覆盖可打印 ASCII。非 ASCII 文本会被拒绝，
  除非加 `--paste`——它把文本写入剪贴板再发 Ctrl+V（会覆盖剪贴板）。
  `--delay MS` 控制字符间停顿（默认 20 毫秒）。
- `drag` 在**一次调用内**完成按下/移动/松开，这是真正做到“按住并移动”的唯一
  方式：跨两次调用的 `mousedown` + `mouseup` 做不到，因为每次调用都会新建
  并销毁自己的设备。可给两个或更多点，中间点会被依次经过。
  `--duration MS` 是整段手势的总时长（按各段长度比例分配），
  `--pause MS` 是按下与开始移动之间的停顿。
- 坐标范围只按第一个输出计算；多显示器布局可能需要额外偏移。

### 语言

消息跟随 locale，检测顺序为 `CWCTRL_LANG`、`LC_ALL`、`LC_MESSAGES`、`LANG`。
值以 `zh` 开头则用中文，否则英文。强制指定：

```bash
CWCTRL_LANG=zh cwctrl --help
```

## 实现说明

用到的协议：

| 协议 | 用途 |
|---|---|
| `ext_foreign_toplevel_list_v1` | 枚举所有顶层窗口（含 XWayland），提供 `identifier`/`title`/`app_id` |
| `zcosmic_toplevel_info_v1` | 用 `get_cosmic_toplevel()` 换取增强 handle |
| `zcosmic_toplevel_handle_v1` | `state` / `geometry` / `output_enter` / `ext_workspace_enter` |
| `zcosmic_toplevel_manager_v1` | `activate` / `close` / `set_maximized` / `set_fullscreen` / `move_to_ext_workspace` 等 |
| `ext_workspace_manager_v1` | 工作区名称与激活状态 |
| `wl_output` / `wl_seat` | 显示器名；`activate` 需要 seat |
| `uinput`（内核） | 鼠标/键盘注入（Wayland 无对应协议） |

两个容易踩的坑：

1. **`zcosmic_toplevel_info_v1` 自 v2 起不再推送 `title`/`app_id`/`toplevel`**
   （XML 里标为 `deprecated-since="2"`）。必须先拿到
   `ext_foreign_toplevel_handle_v1`，再调 `get_cosmic_toplevel()` 换取
   `zcosmic_toplevel_handle_v1`。

2. **`state`、`geometry`、`workspace` 是固定约 150ms 的延迟批量推送**，
   不是请求即答。本机实测 30 次：150.1–152.3ms，无抖动。因此本程序**只在
   需要这些字段时才等待**：控制类命令与 `list -n` 约 1ms 返回；`list` 会等待；
   控制类命令若查询用到状态字段且首轮无匹配，会再等一轮重试
   （`Query::uses_deferred_state()` 判定）。

### 为什么不用 `cosmic-protocols`

[`cosmic-protocols`](https://crates.io/crates/cosmic-protocols) crate 的许可是
**GPL-3.0-only**，会让本二进制也变成 GPL。但它分发的协议 XML 本身是 MIT/HPND
宽松许可。因此本项目用 MIT 许可的 `wayland-scanner` 直接从 `protocols/` 下
vendor 的 XML 生成绑定，从而保持整个项目为 MIT。

所有依赖均为宽松许可（`cargo metadata` 扫描无 GPL/AGPL）。各 XML 文件内保留了
上游版权声明。

## 致谢

- 窗口行为与命令集参考 [`cosmic-ext-window-helper`](https://github.com/lapause/cosmic-ext-window-helper)
- 协议定义来自 [`pop-os/cosmic-protocols`](https://github.com/pop-os/cosmic-protocols)。
- 基于 [`wayland-rs`](https://github.com/Smithay/wayland-rs)。

## 许可

MIT — 见 [LICENSE](LICENSE)。
