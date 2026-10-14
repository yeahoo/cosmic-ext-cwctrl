//! Bilingual messages (English, Simplified Chinese).
//!
//! Language detection order: `CWCTRL_LANG`, then `LC_ALL`, `LC_MESSAGES`, `LANG`.
//! A value starting with `zh` selects Chinese; anything else selects English.

use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Zh,
}

static LANG: OnceLock<Lang> = OnceLock::new();

pub fn lang() -> Lang {
    *LANG.get_or_init(detect)
}

fn detect() -> Lang {
    for var in ["CWCTRL_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
        let Ok(v) = std::env::var(var) else { continue };
        if v.is_empty() {
            continue;
        }
        return if v.to_ascii_lowercase().starts_with("zh") {
            Lang::Zh
        } else {
            Lang::En
        };
    }
    Lang::En
}

/// Message templates. Entries taking arguments use `{0}`, `{1}`, ... placeholders.
pub struct Msg {
    pub help: &'static str,

    // main.rs
    pub unknown_command: &'static str,
    pub toggle_required: &'static str,
    pub toggle_invalid: &'static str,
    pub move_to_arity: &'static str,
    pub no_match: &'static str,
    pub no_toplevel_manager: &'static str,
    pub no_seat: &'static str,
    pub no_output: &'static str,
    pub no_workspace: &'static str,

    // state.rs
    pub connect_failed: &'static str,

    // query.rs
    pub query_error: &'static str,
    pub q_unterminated: &'static str,
    pub q_missing_rparen: &'static str,
    pub q_unexpected_end: &'static str,
    pub q_quoted_field: &'static str,
    pub q_unexpected_paren: &'static str,
    pub q_missing_value: &'static str,
    pub q_unknown_str_field: &'static str,
    pub q_unknown_bool_field: &'static str,
    pub q_trailing: &'static str,
}

pub static EN: Msg = Msg {
    help: "\
cwctrl — COSMIC window query & control

USAGE: cwctrl <COMMAND> [ARGS...] [QUERY]

COMMANDS:
  state  [QUERY]                        Print window state as JSON (with geometry)
  list   [-n] [QUERY]                   Same as state (JSON array)
  activate [QUERY]                      Focus the most recently used matching window
  close    [QUERY]                      Close matching windows
  minimize <true|false> [QUERY]         Minimize / unminimize
  maximize <true|false> [QUERY]         Maximize / unmaximize
  fullscreen <true|false> [QUERY]       Fullscreen / unfullscreen
  sticky   <true|false> [QUERY]         Stick / unstick
  move_to  <WORKSPACE> <OUTPUT> [QUERY] Move windows to a workspace
  cycle    [-b] [QUERY]                 Cycle focus through the matching windows
  interfaces                            List Wayland interfaces the compositor exposes
  -h, --help                            Show this help
  -V, --version                         Show version

OPTIONS:
  -n, --no-wait   Do not wait for geometry; returns in ~1ms with geometry: null.
                  COSMIC pushes state/geometry as a fixed ~150ms batch, so this
                  skips that wait. Control commands never wait to begin with.

QUERY SYNTAX (omit to match every window):
  string fields: id, app_id, title, workspace.name, output.name
  operators:     =   equal
                 !=  not equal
                 ~=  regular expression
                 Values must be quoted. Append `i` for case-insensitive matching.
  bool fields:   is_active, is_active_app_id, is_maximized, is_minimized,
                 is_fullscreen, is_sticky, workspace.is_visible,
                 workspace.has_focus, output.has_focus
  combinators:   and / or / not, plus parentheses

EXAMPLES:
  cwctrl list \"app_id ~= 'chrome'i\"
  cwctrl activate \"title ~= 'wezterm'\"
  cwctrl maximize true \"app_id = 'com.microsoft.VSCode'\"
  cwctrl list \"workspace.is_visible and not is_active\"
  cwctrl cycle \"is_active_app_id\"        # cycle windows of the focused app
",
    unknown_command: "unknown command '{0}' (see `cwctrl --help`)",
    toggle_required: "'{0}' needs a true or false argument, e.g. `cwctrl {0} true`",
    toggle_invalid: "toggle argument must be true or false, got '{0}'",
    move_to_arity: "'move_to' needs two arguments: <WORKSPACE> <OUTPUT>",
    no_match: "no matching window",
    no_toplevel_manager: "compositor does not support zcosmic_toplevel_manager_v1",
    no_seat: "wl_seat unavailable, cannot activate windows",
    no_output: "no output named '{0}'",
    no_workspace: "no workspace named '{0}'",
    connect_failed: "cannot connect to the Wayland display server: {0}",
    query_error: "query syntax error: {0}",
    q_unterminated: "unterminated string literal",
    q_missing_rparen: "missing closing ')'",
    q_unexpected_end: "unexpected end of expression",
    q_quoted_field: "field name must not be a string literal",
    q_unexpected_paren: "unexpected parenthesis",
    q_missing_value: "field '{0}' is missing a comparison value",
    q_unknown_str_field: "unknown string field '{0}'",
    q_unknown_bool_field: "unknown boolean field '{0}'",
    q_trailing: "unexpected trailing content after token {0}",
};

pub static ZH: Msg = Msg {
    help: "\
cwctrl — COSMIC 窗口查询与控制

用法: cwctrl <命令> [参数...] [查询]

命令:
  state  [查询]                     以 JSON 输出窗口状态 (含 geometry)
  list   [-n] [查询]                同 state (JSON 数组)
  activate [查询]                   激活该应用最近使用的那个窗口
  close    [查询]                   关闭匹配的窗口
  minimize <true|false> [查询]      最小化 / 取消最小化
  maximize <true|false> [查询]      最大化 / 取消最大化
  fullscreen <true|false> [查询]    全屏 / 取消全屏
  sticky   <true|false> [查询]      固定 / 取消固定
  move_to  <工作区> <显示器> [查询] 移动到指定工作区
  cycle    [-b] [查询]              在同组匹配窗口间循环切换焦点
  interfaces                        列出合成器暴露的 Wayland 接口
  -h, --help                        显示帮助
  -V, --version                     显示版本

选项:
  -n, --no-wait   list/state 不等待 geometry, 约 1ms 返回 (geometry 为 null)。
                  COSMIC 的 state/geometry 是固定约 150ms 延迟批量推送的,
                  该选项跳过这段等待; 控制类命令本来就不等。

查询语法 (省略则匹配所有窗口):
  字符串字段: id, app_id, title, workspace.name, output.name
  运算符:     =   相等
              !=  不等
              ~=  正则匹配
              值必须加引号; 后缀 i 表示忽略大小写
  布尔字段:   is_active, is_active_app_id, is_maximized, is_minimized,
              is_fullscreen, is_sticky, workspace.is_visible,
              workspace.has_focus, output.has_focus
  组合:       and / or / not, 以及括号

示例:
  cwctrl list \"app_id ~= 'chrome'i\"
  cwctrl activate \"title ~= 'wezterm'\"
  cwctrl maximize true \"app_id = 'com.microsoft.VSCode'\"
  cwctrl list \"workspace.is_visible and not is_active\"
  cwctrl cycle \"is_active_app_id\"        # 同一应用多窗口循环切换
",
    unknown_command: "未知命令 '{0}' (用 `cwctrl --help` 查看用法)",
    toggle_required: "'{0}' 需要 true 或 false 参数, 例如: `cwctrl {0} true`",
    toggle_invalid: "开关参数必须是 true 或 false, 收到 '{0}'",
    move_to_arity: "'move_to' 需要 <工作区> <显示器> 两个参数",
    no_match: "没有匹配的窗口",
    no_toplevel_manager: "合成器不支持 zcosmic_toplevel_manager_v1",
    no_seat: "wl_seat 不可用, 无法激活窗口",
    no_output: "找不到名为 '{0}' 的显示器",
    no_workspace: "找不到名为 '{0}' 的工作区",
    connect_failed: "无法连接 Wayland 显示服务器: {0}",
    query_error: "查询语法错误: {0}",
    q_unterminated: "字符串字面量缺少结束引号",
    q_missing_rparen: "缺少右括号 ')'",
    q_unexpected_end: "表达式意外结束",
    q_quoted_field: "字段名不能是字符串字面量",
    q_unexpected_paren: "意外的括号",
    q_missing_value: "字段 '{0}' 后缺少比较值",
    q_unknown_str_field: "未知的字符串字段 '{0}'",
    q_unknown_bool_field: "未知的布尔字段 '{0}'",
    q_trailing: "第 {0} 个 token 之后有多余内容",
};

/// Substitute the `{0}` placeholder in a message template.
///
/// `format!` requires a compile-time literal as its first argument, so
/// `format!(i18n::m().xxx, a)` is impossible; we substitute by hand here. Every
/// template that takes an argument has exactly one placeholder.
pub fn fill1(tmpl: &str, arg: impl std::fmt::Display) -> String {
    tmpl.replace("{0}", &arg.to_string())
}

/// Message table for the current language.
pub fn m() -> &'static Msg {
    if lang() == Lang::Zh { &ZH } else { &EN }
}
