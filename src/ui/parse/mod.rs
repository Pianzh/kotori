//! JSON from the daemon <-> the UI's own structs, plus the small pure helpers the
//! pages use (labels, placeholders, search matching, retry backoff).
//! No widget is built here.
//!
//! 按「一条回包 / 一个请求的完整生命周期」拆成子模块:环境检查(`environment`)、
//! Wine(`wine`)、云同步(`sync`)、游戏列表(`games`)、存档位置(`save_paths`)、
//! 缩放档案(`scale`)、自动重连退避(`reconnect`)。每个子模块只负责一条线,顶部
//! 写清它为什么和邻居分开 —— 不按类型分堆。
//!
//! 本文件只留三样东西:对外的门面(下面的 `pub(super) use` 让 `crate::ui::parse::*`
//! 这些路径照旧可用,调用方一行都不用改)、所有子模块共用的取字段原语
//! (`str_field` / `string_list` / `u32_field`)、以及子模块声明。

use super::*;

mod environment;
mod games;
mod process;
mod reconnect;
mod save_paths;
mod scale;
mod sync;
mod wine;

pub(super) use environment::*;
pub(super) use games::*;
pub(super) use process::*;
pub(super) use reconnect::*;
pub(super) use save_paths::*;
pub(super) use scale::*;
pub(super) use sync::*;
pub(super) use wine::*;

/// Text field of a JSON object, or empty when absent.
pub(in crate::ui) fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

pub(in crate::ui) fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub(in crate::ui) fn u32_field(parent: Option<&Value>, key: &str) -> Option<u32> {
    parent?.get(key)?.as_u64().map(|v| v as u32)
}
