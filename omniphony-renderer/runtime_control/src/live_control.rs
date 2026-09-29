//! OSC writes that only change the renderer's live state.
//!
//! Each handler here validates its message, mutates [`RendererControl`] and
//! describes what else has to happen as [`ControlEffects`]; it never touches a
//! socket or a file. The engine dispatcher (`orender_engine::osc::dispatch`)
//! runs [`apply_live_control`] first and turns the effects into I/O through
//! the same `apply_control_effects` as every other core handler — so a write
//! here reaches the other clients exactly the way a write anywhere else does.
//!
//! [`RendererControl`]: renderer::live_params::RendererControl

use rosc::{OscMessage, OscType};

use crate::context::RuntimeControlContext;
use crate::osc::{ControlEffects, Notify, parse_f32_arg};
use crate::osc_contract;

/// Handle the live-state writes this module owns. `None` when `msg` is not one
/// of them, so the dispatcher moves on.
pub fn apply_live_control(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let addr = msg.addr.as_str();
    let control = &ctx.renderer;

    // Monitoring cadences live on RendererControl (the source of truth): both
    // CLI and embedded engine read them, they persist to config, and they are
    // broadcast in the live-state bundle.
    if addr == osc_contract::CONTROL_METERING_RATE_HZ || addr == osc_contract::CONTROL_DIAG_RATE_HZ
    {
        let Some(hz) = parse_f32_arg(msg.args.first()).filter(|hz| hz.is_finite()) else {
            return Some(ControlEffects::default());
        };
        let (what, applied) = if addr == osc_contract::CONTROL_METERING_RATE_HZ {
            control.set_meter_rate_hz(hz);
            ("metering", control.meter_rate_hz())
        } else {
            control.set_diag_rate_hz(hz);
            ("diag", control.diag_rate_hz())
        };
        let mut effects = ControlEffects::dirty(Notify::Snapshot);
        effects.log_message = Some(format!("OSC {what} rate set to {applied:.1} Hz"));
        return Some(effects);
    }

    // Live object-generator (PAD: strength / hpf_hz / gain_db) and
    // phantom-extraction (strength / passes / lift) parameters.
    let generator = addr == osc_contract::CONTROL_OBJECT_GENERATOR_PARAM;
    if generator || addr == osc_contract::CONTROL_PHANTOM_EXTRACT_PARAM {
        let key = match msg.args.first() {
            Some(OscType::String(s)) => s.trim().to_ascii_lowercase(),
            _ => return Some(ControlEffects::default()),
        };
        let Some(value) = parse_f32_arg(msg.args.get(1)) else {
            return Some(ControlEffects::default());
        };
        if key.is_empty() || !value.is_finite() {
            return Some(ControlEffects::default());
        }
        // Store the override generically; the stage validates and clamps it by
        // key when the render thread applies it (declared-schema design).
        {
            let mut live = control.live.write();
            let params = if generator {
                &mut live.object_generator_params
            } else {
                &mut live.phantom_params
            };
            params.insert(key, value);
        }
        // Params are NOT persisted immediately (a slider drag is a burst of
        // updates — no config write per tick), so the Save button is the only
        // way to keep them and the dirty state must reach it. The bundle is
        // coalesced for the same reason.
        return Some(ControlEffects::dirty(Notify::CoalescedSnapshot));
    }

    // Per-family placement of fixed channels (`renderer::placement`): the
    // mode a family is placed with, and the family's own entries. Both
    // live-tunable from Studio's editor; both persist to config on save.
    // The legacy `virtual_bed` address is the generic family's entries.
    if addr == osc_contract::CONTROL_PLACEMENT_MODE {
        return Some(apply_placement_mode(msg, ctx));
    }
    if addr == osc_contract::CONTROL_PLACEMENT_LAYOUT || addr == osc_contract::CONTROL_VIRTUAL_BED {
        return Some(apply_placement_layout(msg, ctx));
    }

    None
}

fn apply_placement_mode(msg: &OscMessage, ctx: &RuntimeControlContext) -> ControlEffects {
    use renderer::placement::{PlacementMode, SourceFamily};
    let (Some(OscType::String(family)), Some(OscType::String(mode))) =
        (msg.args.first(), msg.args.get(1))
    else {
        return ControlEffects::default();
    };
    let Some(family) = SourceFamily::parse(family) else {
        log::warn!("OSC placement mode: unknown family '{}'", family);
        return ControlEffects::default();
    };
    let mode = if mode.trim().eq_ignore_ascii_case("inherit") {
        None
    } else {
        match PlacementMode::parse(mode) {
            Some(mode) => Some(mode),
            None => {
                log::warn!("OSC placement mode: unknown mode '{}'", mode);
                return ControlEffects::default();
            }
        }
    };
    let changed = {
        let mut live = ctx.renderer.live.write();
        let slot = &mut live.placement.family_mut(family).mode;
        std::mem::replace(slot, mode) != mode
    };
    // The mode re-plans the stream: the fixed-prefix planner caches on the
    // options epoch. Bumped only on a real change, like a `REPLAN` registry
    // option (`renderer::options::apply_to_control`): a redundant re-send (a
    // client echoing state back, Studio reconnecting) must not force a
    // re-plan, which can carry an audible re-prime transient.
    if changed {
        ctx.renderer.bump_options_epoch();
    }
    placement_effects(format!(
        "OSC placement mode: {} → {}",
        family.as_str(),
        mode.map_or("inherit", |m| m.as_str())
    ))
}

fn apply_placement_layout(msg: &OscMessage, ctx: &RuntimeControlContext) -> ControlEffects {
    use renderer::placement::SourceFamily;
    let (family, arg) = if msg.addr == osc_contract::CONTROL_PLACEMENT_LAYOUT {
        match msg.args.first() {
            Some(OscType::String(family)) => match SourceFamily::parse(family) {
                Some(family) => (family, msg.args.get(1)),
                None => {
                    log::warn!("OSC placement layout: unknown family '{}'", family);
                    return ControlEffects::default();
                }
            },
            _ => return ControlEffects::default(),
        }
    } else {
        (SourceFamily::Generic, msg.args.first())
    };
    let Some(OscType::String(s)) = arg else {
        return ControlEffects::default();
    };
    let trimmed = s.trim();
    let layout = if trimmed.is_empty() {
        None
    } else {
        match renderer::speaker_layout::SpeakerLayout::entries_from_yaml_str(trimmed) {
            Ok(layout) => Some(layout),
            Err(e) => {
                log::warn!("OSC placement layout: failed to parse entries: {}", e);
                return ControlEffects::default();
            }
        }
    };
    let cleared = layout.is_none();
    // No epoch bump: both channel planners compare the family's placement by
    // value (`virtual_bed::ChannelPlanKey`).
    ctx.renderer
        .live
        .write()
        .placement
        .family_mut(family)
        .layout = layout;
    placement_effects(format!(
        "OSC placement layout: {} {}",
        family.as_str(),
        if cleared { "cleared" } else { "updated" }
    ))
}

/// A placement write is a config edit announced with a coalesced bundle: the
/// placement editor's gain and pose controls resend the family's whole entry
/// list on every tick of a drag.
fn placement_effects(log: String) -> ControlEffects {
    let mut effects = ControlEffects::dirty(Notify::CoalescedSnapshot);
    effects.log_message = Some(log);
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::placement::{PlacementMode, SourceFamily};

    fn ctx() -> RuntimeControlContext {
        RuntimeControlContext::new(crate::test_support::fixture_control())
    }

    fn msg(addr: &str, args: Vec<OscType>) -> OscMessage {
        OscMessage {
            addr: addr.to_string(),
            args,
        }
    }

    #[test]
    fn placement_mode_bumps_the_options_epoch_only_on_a_real_change() {
        let ctx = ctx();
        let set_room = msg(
            osc_contract::CONTROL_PLACEMENT_MODE,
            vec![
                OscType::String("dolby".into()),
                OscType::String("room".into()),
            ],
        );
        let epoch = ctx.renderer.options_epoch();
        let effects = apply_live_control(&set_room, &ctx).expect("handled");
        assert!(effects.mark_dirty);
        assert_eq!(effects.notify, Notify::CoalescedSnapshot);
        assert_eq!(ctx.renderer.options_epoch(), epoch + 1);
        assert_eq!(
            ctx.renderer
                .live
                .read()
                .placement
                .family(SourceFamily::Dolby)
                .mode,
            Some(PlacementMode::Room)
        );

        // The same value again: still acknowledged, but no re-plan.
        apply_live_control(&set_room, &ctx).expect("handled");
        assert_eq!(ctx.renderer.options_epoch(), epoch + 1);
    }

    #[test]
    fn placement_layout_never_bumps_the_options_epoch() {
        let ctx = ctx();
        let clear = msg(
            osc_contract::CONTROL_VIRTUAL_BED,
            vec![OscType::String(String::new())],
        );
        let epoch = ctx.renderer.options_epoch();
        // The channel planners compare the placement by value.
        let effects = apply_live_control(&clear, &ctx).expect("handled");
        assert!(effects.mark_dirty);
        assert_eq!(ctx.renderer.options_epoch(), epoch);
    }

    #[test]
    fn param_writes_are_coalesced_and_non_finite_values_dropped() {
        let ctx = ctx();
        let write = |addr: &str, value: f32| {
            apply_live_control(
                &msg(
                    addr,
                    vec![OscType::String(" Strength ".into()), OscType::Float(value)],
                ),
                &ctx,
            )
            .expect("handled")
        };
        let effects = write(osc_contract::CONTROL_OBJECT_GENERATOR_PARAM, 0.25);
        assert!(effects.mark_dirty);
        assert_eq!(effects.notify, Notify::CoalescedSnapshot);
        let effects = write(osc_contract::CONTROL_PHANTOM_EXTRACT_PARAM, f32::NAN);
        assert!(!effects.mark_dirty);
        let live = ctx.renderer.live.read();
        assert_eq!(live.object_generator_params.get("strength"), Some(&0.25));
        assert!(live.phantom_params.is_empty());
    }

    #[test]
    fn monitoring_rates_reject_non_finite_values() {
        let ctx = ctx();
        let before = ctx.renderer.meter_rate_hz();
        let effects = apply_live_control(
            &msg(
                osc_contract::CONTROL_METERING_RATE_HZ,
                vec![OscType::Float(f32::NAN)],
            ),
            &ctx,
        )
        .expect("handled");
        assert!(!effects.mark_dirty);
        assert_eq!(ctx.renderer.meter_rate_hz(), before);

        let effects = apply_live_control(
            &msg(
                osc_contract::CONTROL_DIAG_RATE_HZ,
                vec![OscType::Float(12.0)],
            ),
            &ctx,
        )
        .expect("handled");
        assert!(effects.mark_dirty);
        assert_eq!(effects.notify, Notify::Snapshot);
        assert_eq!(ctx.renderer.diag_rate_hz(), 12.0);
    }
}
