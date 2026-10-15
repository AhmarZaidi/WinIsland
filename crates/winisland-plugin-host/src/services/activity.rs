use std::ffi::c_void;
use std::time::Instant;

use winisland_plugin_api::{
    ACTIVITY_ENABLED, ACTIVITY_KEEP_VISIBLE, ActivitySpecV2, CAP_ACTIVITY, PluginStatus,
    PluginToken, ResourceId, SURFACE_COMPACT_MAIN, SURFACE_PAGE, WidgetId,
};

use super::{read_struct, runtime};
use crate::resources::ResourceKind;
use crate::runtime::{ActivityRecord, HostRuntime, ServiceState};

fn validate(
    host: &HostRuntime,
    token: PluginToken,
    spec: &ActivitySpecV2,
) -> Result<(), PluginStatus> {
    host.registry.require(token, CAP_ACTIVITY)?;
    if spec.priority > 2
        || spec.flags & !(ACTIVITY_ENABLED | ACTIVITY_KEEP_VISIBLE) != 0
        || !spec.preferred_width.is_finite()
        || !spec.preferred_height.is_finite()
        || !(0.0..=2048.0).contains(&spec.preferred_width)
        || !(0.0..=2048.0).contains(&spec.preferred_height)
    {
        return Err(PluginStatus::InvalidArgument);
    }
    host.resources
        .require(token, ResourceKind::Widget, spec.compact_surface.get())?;
    if spec.expanded_page != WidgetId::INVALID {
        host.resources
            .require(token, ResourceKind::Widget, spec.expanded_page.get())?;
    }
    Ok(())
}

fn validate_surfaces(state: &ServiceState, spec: &ActivitySpecV2) -> Result<(), PluginStatus> {
    for (id, kind) in [
        (spec.compact_surface, SURFACE_COMPACT_MAIN),
        (spec.expanded_page, SURFACE_PAGE),
    ] {
        if id == WidgetId::INVALID && kind == SURFACE_PAGE {
            continue;
        }
        let record = state
            .widgets
            .get(&id.get())
            .ok_or(PluginStatus::StaleHandle)?;
        if record.disabled || record.surface.is_none_or(|surface| surface.kind != kind) {
            return Err(PluginStatus::InvalidArgument);
        }
    }
    Ok(())
}

pub unsafe extern "C" fn create(
    context: *mut c_void,
    token: PluginToken,
    spec: *const ActivitySpecV2,
    out: *mut ResourceId,
) -> PluginStatus {
    let result = (|| {
        if out.is_null() {
            return Err(PluginStatus::InvalidArgument);
        }
        // SAFETY: ABI callers provide a live instance context and a readable sized specification.
        let (host, spec) = unsafe { (runtime(context)?, read_struct(spec)?) };
        validate(host, token, &spec)?;
        let mut state = host.state.lock().map_err(|_| PluginStatus::Internal)?;
        validate_surfaces(&state, &spec)?;
        let id = host.resources.allocate(token, ResourceKind::Activity, 0)?;
        state.activities.insert(
            id,
            ActivityRecord {
                spec,
                updated_at: Instant::now(),
            },
        );
        state.activity_revision = state.activity_revision.wrapping_add(1);
        drop(state);
        host.extensions.changed();
        // SAFETY: The caller supplied a writable resource output and the ID belongs to it.
        unsafe { *out = ResourceId::from_raw(id) };
        Ok(())
    })();
    result.err().unwrap_or(PluginStatus::Ok)
}

pub unsafe extern "C" fn update(
    context: *mut c_void,
    token: PluginToken,
    id: ResourceId,
    spec: *const ActivitySpecV2,
) -> PluginStatus {
    let result = (|| {
        // SAFETY: ABI callers provide a live instance context and a readable sized specification.
        let (host, spec) = unsafe { (runtime(context)?, read_struct(spec)?) };
        validate(host, token, &spec)?;
        host.resources
            .require(token, ResourceKind::Activity, id.get())?;
        let mut state = host.state.lock().map_err(|_| PluginStatus::Internal)?;
        validate_surfaces(&state, &spec)?;
        let record = state
            .activities
            .get_mut(&id.get())
            .ok_or(PluginStatus::StaleHandle)?;
        record.spec = spec;
        record.updated_at = Instant::now();
        state.activity_revision = state.activity_revision.wrapping_add(1);
        drop(state);
        host.extensions.changed();
        Ok(())
    })();
    result.err().unwrap_or(PluginStatus::Ok)
}

pub unsafe extern "C" fn release(
    context: *mut c_void,
    token: PluginToken,
    id: ResourceId,
) -> PluginStatus {
    let result = (|| {
        // SAFETY: ABI callers provide a live instance context.
        let host = unsafe { runtime(context)? };
        host.registry.require(token, CAP_ACTIVITY)?;
        let mut state = host.state.lock().map_err(|_| PluginStatus::Internal)?;
        host.resources
            .release(token, ResourceKind::Activity, id.get())?;
        state.activities.remove(&id.get());
        state.activity_revision = state.activity_revision.wrapping_add(1);
        drop(state);
        host.extensions.changed();
        Ok(())
    })();
    result.err().unwrap_or(PluginStatus::Ok)
}
