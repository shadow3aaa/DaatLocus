use super::{DashboardState, ReducedMotion};

pub(super) fn dashboard_state_needs_animation(state: &DashboardState) -> bool {
    if state.reduced_motion != ReducedMotion::Full {
        return false;
    }
    state.runtime_status.as_deref() == Some("Working") || !state.live_activity_cells.is_empty()
}
