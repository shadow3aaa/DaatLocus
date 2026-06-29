use super::*;

pub(super) async fn sync_workspace_apps_from_invalidation(context: &mut Context) {
    let report = match context
        .workspace_apps
        .sync_dirty_apps(&mut context.apps)
        .await
    {
        Ok(report) => report,
        Err(err) => {
            tracing::error!("failed to sync workspace apps from invalidation: {err:?}");
            return;
        }
    };

    if report.is_empty() {
        return;
    }

    for removed in &report.removed {
        context.clear_active_app_notice(removed);
    }
    if !report.added.is_empty() {
        tracing::info!(
            apps = report
                .added
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
            "loaded workspace apps from source changes",
        );
    }
    if !report.reloaded.is_empty() {
        tracing::info!(
            apps = report
                .reloaded
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
            "reloaded workspace apps from source changes",
        );
    }
    if !report.removed.is_empty() {
        tracing::info!(
            apps = report
                .removed
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
            "unloaded workspace apps removed from source tree",
        );
    }
    for error in report.errors {
        tracing::warn!("{error}");
    }
}
