//! 设置页:后台服务(守护进程)的启停与 Wine prefix。

use super::*;

pub(super) fn push_settings(ui: &mut Ui) {
    let app = &ui.app;
    let w = &ui.window;

    let (status, state) = service_state(app);
    push_str(w.get_service_status(), &status, |v| w.set_service_status(v));
    push_int(w.get_service_state(), state, |v| w.set_service_state(v));
    push_bool(
        w.get_service_running(),
        app.daemon_connected == Some(true),
        |v| w.set_service_running(v),
    );
    push_bool(w.get_service_busy(), app.service_busy, |v| {
        w.set_service_busy(v)
    });
    let message = app.service_msg.clone().unwrap_or_default();
    let ok = !message.contains("失败");
    push_str(w.get_service_message(), &message, |v| {
        w.set_service_message(v)
    });
    push_bool(w.get_service_message_ok(), ok, |v| {
        w.set_service_message_ok(v)
    });
    push_str(
        w.get_service_hint(),
        &service_hint(app.daemon_paused),
        |v| w.set_service_hint(v),
    );

    push_str(w.get_wine_prefix(), &app.wine_prefix_input, |v| {
        w.set_wine_prefix(v)
    });
    let message = app.wine_msg.clone().unwrap_or_default();
    let ok = message.starts_with("已保存");
    push_str(w.get_wine_message(), &message, |v| w.set_wine_message(v));
    push_bool(w.get_wine_message_ok(), ok, |v| w.set_wine_message_ok(v));

    let wine = app.wine_status.as_ref();
    push_str(
        w.get_wine_effective(),
        wine.and_then(|status| status.configured.clone())
            .as_deref()
            .unwrap_or("自动探测"),
        |v| w.set_wine_effective(v),
    );
    push_str(
        w.get_wine_default(),
        wine.map(|status| status.default_prefix.as_str())
            .unwrap_or("读取中…"),
        |v| w.set_wine_default(v),
    );
    push_str(
        w.get_wine_environment(),
        wine.and_then(|status| status.environment.as_deref())
            .unwrap_or("未设置"),
        |v| w.set_wine_environment(v),
    );
    let detected: Vec<SharedString> = wine
        .map(|status| {
            status
                .detected
                .iter()
                .map(|path| path.as_str().into())
                .collect()
        })
        .unwrap_or_default();
    push_eq(w.get_wine_detected(), strings(detected), |v| {
        w.set_wine_detected(v)
    });

    // ── 环境检查 ──────────────────────────────────────────────────────────
    let environment = app.environment.clone().unwrap_or_default();
    push_bool(w.get_env_loaded(), app.environment.is_some(), |v| {
        w.set_env_loaded(v)
    });
    push_str(w.get_env_summary(), &environment.summary(), |v| {
        w.set_env_summary(v)
    });
    push_str(w.get_env_distro(), &environment.distro_line(), |v| {
        w.set_env_distro(v)
    });
    push_checks(w, &environment.checks);
}

/// 环境检查那一组:只读、行数少,所以整表重建不会踩到"输入焦点"那个坑
/// (见 `detail::push_saves` 的注释)。即便如此也只在真的不一样时才写 ——
/// 每次 render 都换一个新模型,滚动位置与悬停状态都会抖一下。
fn push_checks(w: &AppWindow, checks: &[EnvCheck]) {
    let wanted: Vec<EnvCheckRow> = checks.iter().map(check_row).collect();
    let model = w.get_env_checks();
    let same = model.row_count() == wanted.len()
        && wanted
            .iter()
            .enumerate()
            .all(|(index, row)| model.row_data(index).as_ref() == Some(row));
    if !same {
        w.set_env_checks(ModelRc::new(VecModel::from(wanted)));
    }
}

/// `model::EnvCheck` → 窗口里那一行(视图类型在 `slint/types.slint`)。
fn check_row(check: &EnvCheck) -> EnvCheckRow {
    EnvCheckRow {
        title: check.title.clone().into(),
        state: check.state,
        state_label: check.state_label.clone().into(),
        detail: check.detail.clone().into(),
        impact: check.impact.clone().into(),
        install: check.install.clone().into(),
    }
}

/// 「守护进程」那一行的状态字与颜色码(0 检测中 / 1 运行中 / 2 未运行)。
///
/// "用户自己停掉的"和"没起来的"分开说:前者是他按的,后者可能只是还没探测完。
fn service_state(app: &App) -> (String, i32) {
    if app.service_busy {
        return ("● 处理中…".to_string(), 0);
    }
    match app.daemon_connected {
        Some(true) => ("● 运行中".to_string(), 1),
        Some(false) if app.daemon_paused => ("● 已停止".to_string(), 2),
        Some(false) => ("● 未运行".to_string(), 2),
        None => ("● 检测中…".to_string(), 0),
    }
}

/// 这一组下面的灰字说明。用户停过一次之后,那两句必须说清"现在这样会怎样"。
fn service_hint(paused: bool) -> String {
    let base = "启动服务 = 把守护进程拉起来(界面本身不会替它跑)。停止服务只停守护进程:\
                正在玩的这一局不受影响,但它退出后不会再自动上传存档;下次打开界面时也会自动把它拉起来。";
    if paused {
        format!(
            "{base}\n现在它是停着的状态,所以「刷新 / 重连」都不会把它拉起来 —— 要它回来就点「启动服务」。"
        )
    } else {
        base.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_line_tells_stopped_by_hand_apart_from_not_running() {
        let mut app = App::new().0;

        assert_eq!(service_state(&app), ("● 检测中…".to_string(), 0));
        app.daemon_connected = Some(true);
        assert_eq!(service_state(&app).1, 1);

        app.daemon_connected = Some(false);
        assert_eq!(service_state(&app).0, "● 未运行");
        app.daemon_paused = true;
        assert_eq!(service_state(&app).0, "● 已停止");

        app.service_busy = true;
        assert_eq!(service_state(&app).1, 0);

        // 停着的时候说明里要写"刷新也拉不起来",否则用户会去点刷新。
        assert!(service_hint(true).contains("重连"));
        assert!(!service_hint(false).contains("重连"));
    }
}
