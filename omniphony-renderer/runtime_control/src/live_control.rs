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
use crate::persist::PersistOp;

/// Handle the live-state writes this module owns. `None` when `msg` is not one
/// of them, so the dispatcher moves on.
pub fn apply_live_control(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let addr = msg.addr.as_str();
    let control = &ctx.renderer;

    // Declared live options (renderer::options registry): the generic setter
    // `/control/option [key, value]` and the legacy per-option addresses both
    // land on the same registry-driven path — validate, apply, and on a real
    // change mark dirty and bump the replan epoch. Options change what is
    // heard, so they reach config.yaml through the Save button only
    // (docs/persistence-policy.md); a handoff to another renderer instance
    // carries them unsaved in the live-handoff sidecar.
    //
    // `/control/options [key, value, key, value, …]` writes several at once:
    // one lock, one rebuild, one notification (`renderer::options` groups).
    if addr == osc_contract::CONTROL_OPTION || addr == osc_contract::CONTROL_OPTIONS {
        // `/control/option` takes one pair; anything after it is ignored.
        let single = addr == osc_contract::CONTROL_OPTION;
        let Some(pairs) = parse_option_pairs(&msg.args, single) else {
            return Some(ControlEffects::default());
        };
        return Some(apply_options(ctx, &pairs));
    }
    if let Some(spec) = renderer::options::find_by_legacy_addr(addr) {
        let Some(value) = msg.args.get(..spec.kind.arity()) else {
            log::warn!("OSC option {}: missing value", spec.key);
            return Some(ControlEffects::default());
        };
        return Some(apply_options(ctx, &[(spec, value)]));
    }

    // Monitoring cadences live on RendererControl (the source of truth): both
    // CLI and embedded engine read them, and they are broadcast in the
    // live-state bundle. They shape what clients display, not what anyone
    // hears, so they are view state: written to config at once, never behind
    // the Save button. Studio re-sends the diag rate every second while its
    // plot is open, so an unchanged value must cost nothing.
    if addr == osc_contract::CONTROL_METERING_RATE_HZ || addr == osc_contract::CONTROL_DIAG_RATE_HZ
    {
        let Some(hz) = parse_f32_arg(msg.args.first()).filter(|hz| hz.is_finite()) else {
            return Some(ControlEffects::default());
        };
        let (what, before, applied, persist) = if addr == osc_contract::CONTROL_METERING_RATE_HZ {
            let before = control.meter_rate_hz();
            control.set_meter_rate_hz(hz);
            (
                "metering",
                before,
                control.meter_rate_hz(),
                PersistOp::METER_RATE,
            )
        } else {
            let before = control.diag_rate_hz();
            control.set_diag_rate_hz(hz);
            ("diag", before, control.diag_rate_hz(), PersistOp::DIAG_RATE)
        };
        if applied == before {
            return Some(ControlEffects::default());
        }
        let mut effects = ControlEffects::view(Notify::Snapshot, persist);
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

/// Split `[key, value, key, value, …]` into (option, value arguments) pairs,
/// each value as many arguments as its option's kind takes; only the first
/// pair when `single`. `None` — the whole message dropped — on an unknown
/// key or a truncated value: past either, where the next key starts is
/// unknowable.
fn parse_option_pairs(
    args: &[OscType],
    single: bool,
) -> Option<Vec<(&'static renderer::options::OptionSpec, &[OscType])>> {
    let mut pairs = Vec::new();
    let mut rest = args;
    while let Some((key, tail)) = rest.split_first() {
        let OscType::String(key) = key else {
            log::warn!("OSC options: expected a key, got {key:?}");
            return None;
        };
        let Some(spec) = renderer::options::find(key) else {
            log::warn!("OSC option: unknown key '{}'", key);
            return None;
        };
        let arity = spec.kind.arity();
        if tail.len() < arity {
            log::warn!("OSC option {}: missing value", spec.key);
            return None;
        }
        let (value, next) = tail.split_at(arity);
        pairs.push((spec, value));
        if single {
            break;
        }
        rest = next;
    }
    if pairs.is_empty() {
        log::warn!("OSC options: no key");
        return None;
    }
    Some(pairs)
}

/// A client value in the owned shape [`RawOptionValue`] borrows from: the
/// numbers of an array option are collected here first.
///
/// [`RawOptionValue`]: renderer::options::RawOptionValue
enum WireValue<'a> {
    Scalar(renderer::options::RawOptionValue<'a>),
    Numbers(Vec<f64>),
    Invalid,
}

impl<'a> WireValue<'a> {
    /// Map the OSC arguments of one value onto the registry's
    /// transport-agnostic raw value. A shape no option accepts (blobs, arrays,
    /// a non-number inside an array value, …) is `Invalid`.
    fn from_args(kind: renderer::options::OptionKind, args: &'a [OscType]) -> Self {
        use renderer::options::{OptionKind, RawOptionValue};
        if let OptionKind::FloatArray { .. } = kind {
            let numbers: Option<Vec<f64>> = args.iter().map(number).collect();
            return numbers.map_or(Self::Invalid, Self::Numbers);
        }
        let raw = match args.first() {
            Some(OscType::String(s)) => RawOptionValue::Str(s),
            Some(OscType::Bool(b)) => RawOptionValue::Bool(*b),
            Some(other) => match number(other) {
                Some(n) => RawOptionValue::Number(n),
                None => return Self::Invalid,
            },
            None => return Self::Invalid,
        };
        Self::Scalar(raw)
    }

    fn raw(&self) -> Option<renderer::options::RawOptionValue<'_>> {
        match self {
            Self::Scalar(raw) => Some(*raw),
            Self::Numbers(values) => Some(renderer::options::RawOptionValue::Numbers(values)),
            Self::Invalid => None,
        }
    }
}

fn number(arg: &OscType) -> Option<f64> {
    match arg {
        OscType::Int(i) => Some(*i as f64),
        OscType::Long(l) => Some(*l as f64),
        OscType::Float(f) => Some(*f as f64),
        OscType::Double(d) => Some(*d),
        _ => None,
    }
}

/// Registry-driven application of declared live options: validate + apply
/// them together via `options::apply_batch` (which marks dirty and bumps the
/// replan epoch on a real change), ask for the one rebuild their groups
/// need, then for a live-state bundle. Invalid values are dropped with a
/// warning, per the OSC contract; the rest of the message still applies.
///
/// The bundle goes out with the acknowledgement: without it a client that did
/// not send the message never learns the value moved, and the one that did
/// never learns what the setter made of it — an option clamped on arrival
/// would keep displaying the number the user typed.
fn apply_options(
    ctx: &RuntimeControlContext,
    pairs: &[(&'static renderer::options::OptionSpec, &[OscType])],
) -> ControlEffects {
    use renderer::options::Rebuild;
    let values: Vec<WireValue> = pairs
        .iter()
        .map(|(spec, args)| WireValue::from_args(spec.kind, args))
        .collect();
    let mut items = Vec::with_capacity(pairs.len());
    for ((spec, _), value) in pairs.iter().zip(&values) {
        match value.raw() {
            Some(raw) => items.push((*spec, raw)),
            None => log::warn!("OSC option {}: rejected value", spec.key),
        }
    }
    if items.is_empty() {
        return ControlEffects::default();
    }
    let batch = renderer::options::apply_batch(&ctx.renderer, &items);
    let mut applied = Vec::new();
    for ((spec, _), result) in items.iter().zip(&batch.results) {
        match result {
            Some(result) if result.changed => {
                applied.push(format!("{} set to '{}'", spec.key, result.canonical));
            }
            Some(_) => {}
            None => log::warn!("OSC option {}: rejected value", spec.key),
        }
    }
    if !batch.changed {
        if batch.results.iter().all(Option::is_none) {
            return ControlEffects::default();
        }
        // Still published: a value clamped back onto the current one must
        // reach the client that typed it.
        return ControlEffects::transient(Notify::Snapshot);
    }
    let mut effects = ControlEffects::dirty(Notify::Snapshot);
    effects.log_message = Some(format!("OSC option {}", applied.join(", ")));
    match batch.rebuild {
        Rebuild::None => {}
        Rebuild::Evaluation => {
            effects.trigger_layout_recompute = true;
            effects.evaluation_only = true;
        }
        Rebuild::Topology => effects.trigger_layout_recompute = true,
    }
    effects
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

    fn room(ctx: &RuntimeControlContext) -> ([f32; 3], f32, f32, f32) {
        let live = ctx.renderer.live.read();
        (
            live.room_ratio,
            live.room_ratio_rear,
            live.room_ratio_lower,
            live.room_ratio_center_blend,
        )
    }

    fn s(v: &str) -> OscType {
        OscType::String(v.into())
    }

    /// The whole room in one message: every key applied before anything is
    /// rebuilt, and one topology rebuild for all of them.
    #[test]
    fn a_grouped_room_write_is_one_topology_rebuild() {
        let ctx = ctx();
        let write = msg(
            osc_contract::CONTROL_OPTIONS,
            vec![
                s("room_ratio"),
                OscType::Float(1.0),
                OscType::Float(3.0),
                OscType::Int(2),
                s("room_ratio_rear"),
                OscType::Double(2.5),
                s("room_ratio_center_blend"),
                OscType::Float(0.25),
            ],
        );
        let effects = apply_live_control(&write, &ctx).expect("handled");
        assert!(effects.mark_dirty);
        assert_eq!(effects.notify, Notify::Snapshot);
        assert!(effects.trigger_layout_recompute);
        assert!(!effects.evaluation_only, "the room moves the geometry");
        let (ratio, rear, _, blend) = room(&ctx);
        assert_eq!(ratio, [1.0, 3.0, 2.0]);
        assert_eq!(rear, 2.5);
        assert_eq!(blend, 0.25);

        // The same message again changes nothing: no rebuild, no Save.
        let again = apply_live_control(&write, &ctx).expect("handled");
        assert!(!again.mark_dirty);
        assert!(!again.trigger_layout_recompute);
        assert!(again.publish_only, "still acknowledged");
    }

    /// The pre-registry room addresses are exact aliases, and an unchanged
    /// value no longer rebuilds the topology (Studio re-sends all four on
    /// every edit).
    #[test]
    fn the_legacy_room_addresses_are_aliases() {
        let ctx = ctx();
        let before = room(&ctx);
        let fff = msg(
            osc_contract::CONTROL_ROOM_RATIO,
            vec![
                OscType::Float(before.0[0]),
                OscType::Float(before.0[1]),
                OscType::Float(before.0[2]),
            ],
        );
        let effects = apply_live_control(&fff, &ctx).expect("handled");
        assert!(!effects.trigger_layout_recompute, "unchanged: no rebuild");

        let rear = msg(
            osc_contract::CONTROL_ROOM_RATIO_REAR,
            vec![OscType::Float(before.1 + 1.0)],
        );
        let effects = apply_live_control(&rear, &ctx).expect("handled");
        assert!(effects.mark_dirty && effects.trigger_layout_recompute);
        assert_eq!(room(&ctx).1, before.1 + 1.0);

        // The old handlers' bounds: rear/lower floored, the blend clamped.
        let lower = msg(
            osc_contract::CONTROL_ROOM_RATIO_LOWER,
            vec![OscType::Float(-1.0)],
        );
        apply_live_control(&lower, &ctx).expect("handled");
        assert_eq!(room(&ctx).2, 0.01);
        let blend = msg(
            osc_contract::CONTROL_ROOM_RATIO_CENTER_BLEND,
            vec![OscType::Float(3.0)],
        );
        apply_live_control(&blend, &ctx).expect("handled");
        assert_eq!(room(&ctx).3, 1.0);

        // A short ratio is dropped, as before.
        let short = msg(osc_contract::CONTROL_ROOM_RATIO, vec![OscType::Float(1.0)]);
        let effects = apply_live_control(&short, &ctx).expect("handled");
        assert!(!effects.mark_dirty && !effects.publish_only);
    }

    /// Past an unknown key or a truncated value, where the next key starts is
    /// unknowable: the whole message is dropped. An invalid value only drops
    /// its own pair.
    #[test]
    fn a_grouped_write_is_parsed_whole_before_anything_applies() {
        let ctx = ctx();
        let before = room(&ctx);
        for args in [
            vec![
                s("room_ratio_rear"),
                OscType::Float(3.0),
                s("no_such_option"),
                OscType::Int(1),
            ],
            vec![
                s("room_ratio_rear"),
                OscType::Float(3.0),
                s("room_ratio"),
                OscType::Float(1.0),
            ],
            vec![OscType::Float(3.0)],
            vec![],
        ] {
            let effects =
                apply_live_control(&msg(osc_contract::CONTROL_OPTIONS, args.clone()), &ctx)
                    .expect("handled");
            assert!(!effects.mark_dirty, "{args:?}");
            assert_eq!(room(&ctx), before, "{args:?}: nothing may apply");
        }

        let effects = apply_live_control(
            &msg(
                osc_contract::CONTROL_OPTIONS,
                vec![
                    s("room_ratio"),
                    s("wide"),
                    OscType::Float(1.0),
                    OscType::Float(1.0),
                    s("room_ratio_lower"),
                    OscType::Float(0.75),
                ],
            ),
            &ctx,
        )
        .expect("handled");
        assert!(effects.mark_dirty);
        assert_eq!(room(&ctx).0, before.0, "the invalid ratio is dropped");
        assert_eq!(room(&ctx).2, 0.75, "its neighbour still applies");
    }

    /// Options with no group effect ride the same batch without a rebuild,
    /// and the single-pair form keeps ignoring trailing arguments.
    #[test]
    fn ungrouped_options_batch_without_a_rebuild() {
        let ctx = ctx();
        let effects = apply_live_control(
            &msg(
                osc_contract::CONTROL_OPTIONS,
                vec![
                    s("ramp_mode"),
                    s("interp"),
                    s("auto_gain"),
                    OscType::Bool(true),
                ],
            ),
            &ctx,
        )
        .expect("handled");
        assert!(effects.mark_dirty);
        assert!(!effects.trigger_layout_recompute);
        assert!(ctx.renderer.live.read().auto_gain);

        let effects = apply_live_control(
            &msg(
                osc_contract::CONTROL_OPTION,
                vec![s("use_loudness"), OscType::Int(1), s("ignored")],
            ),
            &ctx,
        )
        .expect("handled");
        assert!(effects.mark_dirty);
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
        assert!(!effects.mark_dirty, "a cadence is view state, not a Save");
        assert!(effects.publish_only);
        assert_eq!(effects.notify, Notify::Snapshot);
        assert_eq!(effects.persist.len(), 1);
        assert_eq!(ctx.renderer.diag_rate_hz(), 12.0);
    }

    /// Studio re-sends the diag rate every second while its plot is open: the
    /// same value again must neither publish, nor write, nor dirty anything.
    #[test]
    fn an_unchanged_monitoring_rate_is_a_no_op() {
        let ctx = ctx();
        for addr in [
            osc_contract::CONTROL_METERING_RATE_HZ,
            osc_contract::CONTROL_DIAG_RATE_HZ,
        ] {
            let set = || {
                apply_live_control(&msg(addr, vec![OscType::Float(25.0)]), &ctx).expect("handled")
            };
            assert!(set().publish_only, "{addr}: the first write is a change");
            let again = set();
            assert!(!again.mark_dirty, "{addr}");
            assert!(!again.publish_only, "{addr}");
            assert!(again.persist.is_empty(), "{addr}");
        }
    }
}
