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

use crate::HostControlHandler;
use crate::command_table::{self, Command};
use crate::context::RuntimeControlContext;
use crate::osc::{ControlEffects, Notify, parse_f32_arg};
use crate::osc_contract;
use crate::persist::PersistOp;

/// Handle the live-state writes this module owns. `None` when `msg` is not one
/// of them, so the dispatcher moves on. `host` is the registered host control
/// handler, if any: the options it declares are set through the same generic
/// setters as the core's, and its presence scopes out the core options only
/// the embedded engine offers.
pub fn apply_live_control(
    msg: &OscMessage,
    ctx: &RuntimeControlContext,
    host: Option<&dyn HostControlHandler>,
) -> Option<ControlEffects> {
    let addr = msg.addr.as_str();
    if let Some(run) = command_table::find(LIVE_CONTROL_COMMANDS, addr) {
        return Some(run(msg, ctx, host));
    }
    // The dedicated per-option addresses: aliases of the generic setter.
    let spec = renderer::options::find_by_legacy_addr(addr)?;
    let Some(value) = msg.args.get(..spec.kind.arity()) else {
        return Some(ControlEffects::rejected(format!(
            "option {}: missing value",
            spec.key
        )));
    };
    let target = core_target(spec, host);
    Some(apply_options(
        ctx,
        host,
        &[OptionPair {
            key: spec.key,
            kind: spec.kind,
            target,
            args: value,
        }],
    ))
}

/// A handler of [`LIVE_CONTROL_COMMANDS`].
pub type LiveHandler =
    fn(&OscMessage, &RuntimeControlContext, Option<&dyn HostControlHandler>) -> ControlEffects;

/// The live-state writes that are not one option's alias (see
/// `command_table`): the generic setters themselves, the group apply, the
/// monitoring cadences, the plugin parameters and the placement.
pub static LIVE_CONTROL_COMMANDS: &[Command<LiveHandler>] = &[
    Command::any(
        &[osc_contract::CONTROL_OPTION, osc_contract::CONTROL_OPTIONS],
        options,
    ),
    Command::exact(osc_contract::CONTROL_OPTIONS_APPLY, |msg, _, host| {
        apply_option_group(msg, host)
    }),
    Command::any(
        &[
            osc_contract::CONTROL_METERING_RATE_HZ,
            osc_contract::CONTROL_DIAG_RATE_HZ,
        ],
        monitoring_rate,
    ),
    Command::any(
        &[
            osc_contract::CONTROL_OBJECT_GENERATOR_PARAM,
            osc_contract::CONTROL_PHANTOM_EXTRACT_PARAM,
        ],
        plugin_param,
    ),
    // Per-family placement of fixed channels (`renderer::placement`): the
    // mode a family is placed with, and the family's own entries. The legacy
    // `virtual_bed` address is the generic family's entries.
    Command::exact(osc_contract::CONTROL_PLACEMENT_MODE, |msg, ctx, _| {
        apply_placement_mode(msg, ctx)
    }),
    Command::any(
        &[
            osc_contract::CONTROL_PLACEMENT_LAYOUT,
            osc_contract::CONTROL_VIRTUAL_BED,
        ],
        |msg, ctx, _| apply_placement_layout(msg, ctx),
    ),
];

/// Declared live options (renderer::options registry): the generic setter
/// `/control/option [key, value]` and the legacy per-option addresses both
/// land on the same registry-driven path — validate, apply, and on a real
/// change mark dirty and bump the replan epoch. Options change what is
/// heard, so they reach config.yaml through the Save button only
/// (docs/persistence-policy.md); a handoff to another renderer instance
/// carries them unsaved in the live-handoff sidecar.
///
/// `/control/options [key, value, key, value, …]` writes several at once:
/// one lock, one rebuild, one notification (`renderer::options` groups).
/// Both take the host's options too (its audio output and live input).
fn options(
    msg: &OscMessage,
    ctx: &RuntimeControlContext,
    host: Option<&dyn HostControlHandler>,
) -> ControlEffects {
    // `/control/option` takes one pair; anything after it is ignored.
    let single = msg.addr == osc_contract::CONTROL_OPTION;
    match parse_option_pairs(&msg.args, single, host) {
        Ok(pairs) => apply_options(ctx, host, &pairs),
        Err(reason) => ControlEffects::rejected(reason),
    }
}

/// Monitoring cadences live on RendererControl (the source of truth): both
/// CLI and embedded engine read them, and they are broadcast in the
/// live-state bundle. They shape what clients display, not what anyone
/// hears, so they are view state: written to config at once, never behind
/// the Save button. Studio re-sends the diag rate every second while its
/// plot is open, so an unchanged value must cost nothing.
fn monitoring_rate(
    msg: &OscMessage,
    ctx: &RuntimeControlContext,
    _host: Option<&dyn HostControlHandler>,
) -> ControlEffects {
    let addr = msg.addr.as_str();
    let control = &ctx.renderer;
    let Some(hz) = parse_f32_arg(msg.args.first()).filter(|hz| hz.is_finite()) else {
        return ControlEffects::rejected("expected a rate in Hz");
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
        return ControlEffects::default();
    }
    let mut effects = ControlEffects::view(Notify::Snapshot, persist);
    effects.log_message = Some(format!("OSC {what} rate set to {applied:.1} Hz"));
    effects
}

/// Object-generator and phantom-extraction parameters: aliases of the
/// plugin store, as `/backend/param` is for backends. `[key, value]`
/// addresses the selected generator (the phantom stage);
/// `[generator_id, key, value]` a named generator, selected or not. The
/// value is read in the type the parameter declares, so a float-only
/// client still drives a switch (on at >= 0.5).
fn plugin_param(
    msg: &OscMessage,
    ctx: &RuntimeControlContext,
    _host: Option<&dyn HostControlHandler>,
) -> ControlEffects {
    let control = &ctx.renderer;
    let generator = msg.addr == osc_contract::CONTROL_OBJECT_GENERATOR_PARAM;
    use renderer::plugin::{PHANTOM_EXTRACT_ID, PluginKind};
    let (target, key, value) = if generator && msg.args.len() >= 3 {
        (
            crate::osc::parse_string_arg(msg.args.first()),
            msg.args.get(1),
            msg.args.get(2),
        )
    } else {
        (None, msg.args.first(), msg.args.get(1))
    };
    let key = match key {
        Some(OscType::String(s)) => s.trim().to_ascii_lowercase(),
        _ => return ControlEffects::default(),
    };
    let Some(value) = value.and_then(crate::osc::parse_param_value) else {
        return ControlEffects::default();
    };
    let (kind, id) = if generator {
        let id = target
            .unwrap_or_else(|| control.live.read().options.object_generator_id.clone())
            .trim()
            .to_string();
        // "No generator" is a selection, not a plugin with values.
        if id.is_empty() || id.eq_ignore_ascii_case("none") {
            return ControlEffects::default();
        }
        (PluginKind::ObjectGenerator, id)
    } else {
        (PluginKind::PhantomExtract, PHANTOM_EXTRACT_ID.to_string())
    };
    if key.is_empty() || !control.set_plugin_param(kind, &id, &key, value) {
        return ControlEffects::default();
    }
    // Params are NOT persisted immediately (a slider drag is a burst of
    // updates — no config write per tick), so the Save button is the only
    // way to keep them and the dirty state must reach it. The bundle is
    // coalesced for the same reason.
    ControlEffects::dirty(Notify::CoalescedSnapshot)
}

/// Where a key of `/control/option(s)` goes.
#[derive(Clone, Copy)]
enum OptionTarget {
    /// A core option (`renderer::options::LIVE_OPTIONS`).
    Core(&'static renderer::options::OptionSpec),
    /// One the host declares.
    Host,
    /// A core option this host does not offer (`EMBEDDED_ONLY` on a host
    /// with audio I/O): its value is skipped and refused.
    NotOffered,
}

/// One key of `/control/option(s)` with its value's arguments.
struct OptionPair<'a> {
    key: &'a str,
    kind: renderer::options::OptionKind,
    target: OptionTarget,
    args: &'a [OscType],
}

fn core_target(
    spec: &'static renderer::options::OptionSpec,
    host: Option<&dyn HostControlHandler>,
) -> OptionTarget {
    let env = renderer::options::OptionEnv::detached().with_host_io(host.is_some());
    if env.offers(spec) {
        OptionTarget::Core(spec)
    } else {
        OptionTarget::NotOffered
    }
}

/// Split `[key, value, key, value, …]` into (option, value arguments) pairs,
/// each value as many arguments as its option's kind takes; only the first
/// pair when `single`. A key is a core option or one the host declares.
/// `None` — the whole message dropped — on an unknown key or a truncated
/// value: past either, where the next key starts is unknowable.
/// The `[key, value…]` pairs of an option write, or why the whole message is
/// refused.
fn parse_option_pairs<'a>(
    args: &'a [OscType],
    single: bool,
    host: Option<&dyn HostControlHandler>,
) -> Result<Vec<OptionPair<'a>>, String> {
    let mut pairs = Vec::new();
    let mut rest = args;
    while let Some((key, tail)) = rest.split_first() {
        let OscType::String(key) = key else {
            return Err(format!("options: expected a key, got {key:?}"));
        };
        let (kind, target) = if let Some(spec) = renderer::options::find(key) {
            (spec.kind, core_target(spec, host))
        } else if let Some(kind) = host.and_then(|host| host.option_kind(key)) {
            (kind, OptionTarget::Host)
        } else {
            return Err(format!("option: unknown key '{key}'"));
        };
        let arity = kind.arity();
        if tail.len() < arity {
            return Err(format!("option {key}: missing value"));
        }
        let (value, next) = tail.split_at(arity);
        pairs.push(OptionPair {
            key,
            kind,
            target,
            args: value,
        });
        if single {
            break;
        }
        rest = next;
    }
    if pairs.is_empty() {
        return Err("options: no key".to_string());
    }
    Ok(pairs)
}

/// A client value in the owned shape [`RawOptionValue`] borrows from: the
/// numbers of an array option are collected here first. Public so a host
/// maps the arguments of its own legacy addresses the same way.
///
/// [`RawOptionValue`]: renderer::options::RawOptionValue
pub enum WireValue<'a> {
    Scalar(renderer::options::RawOptionValue<'a>),
    Numbers(Vec<f64>),
    Invalid,
}

impl<'a> WireValue<'a> {
    /// Map the OSC arguments of one value onto the registry's
    /// transport-agnostic raw value. A shape no option accepts (blobs, arrays,
    /// a non-number inside an array value, …) is `Invalid`; an OSC nil is the
    /// explicit "unset" of an optional value.
    pub fn from_args(kind: renderer::options::OptionKind, args: &'a [OscType]) -> Self {
        use renderer::options::{OptionKind, RawOptionValue};
        if let OptionKind::FloatArray { .. } = kind {
            let numbers: Option<Vec<f64>> = args.iter().map(number).collect();
            return numbers.map_or(Self::Invalid, Self::Numbers);
        }
        let raw = match args.first() {
            Some(OscType::String(s)) => RawOptionValue::Str(s),
            Some(OscType::Bool(b)) => RawOptionValue::Bool(*b),
            Some(OscType::Nil) => RawOptionValue::Null,
            Some(other) => match number(other) {
                Some(n) => RawOptionValue::Number(n),
                None => return Self::Invalid,
            },
            None => return Self::Invalid,
        };
        Self::Scalar(raw)
    }

    pub fn raw(&self) -> Option<renderer::options::RawOptionValue<'_>> {
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

/// Registry-driven application of declared options: the core ones validated
/// and applied together via `options::apply_batch` (which marks dirty and
/// bumps the replan epoch on a real change) and asking for the one rebuild
/// their groups need, the host's handed to the host in one batch; then one
/// live-state bundle. Invalid values are dropped with a warning, per the OSC
/// contract; the rest of the message still applies.
///
/// The bundle goes out with the acknowledgement: without it a client that did
/// not send the message never learns the value moved, and the one that did
/// never learns what the setter made of it — an option clamped on arrival
/// would keep displaying the number the user typed.
fn apply_options(
    ctx: &RuntimeControlContext,
    host: Option<&dyn HostControlHandler>,
    pairs: &[OptionPair],
) -> ControlEffects {
    use renderer::options::{Applied, Rebuild};
    let values: Vec<WireValue> = pairs
        .iter()
        .map(|pair| WireValue::from_args(pair.kind, pair.args))
        .collect();
    let mut core_items = Vec::new();
    let mut host_items = Vec::new();
    // What is refused, pair by pair: the others still apply.
    let mut refused = Vec::new();
    for (pair, value) in pairs.iter().zip(&values) {
        let Some(raw) = value.raw() else {
            refused.push(format!("{}: rejected value", pair.key));
            continue;
        };
        match pair.target {
            OptionTarget::Core(spec) => core_items.push((spec, raw)),
            OptionTarget::Host => host_items.push((pair.key, raw)),
            OptionTarget::NotOffered => {
                refused.push(format!("{}: not offered by this host", pair.key))
            }
        }
    }
    let refused_reason = |refused: Vec<String>| {
        (!refused.is_empty()).then(|| format!("option {}", refused.join(", ")))
    };
    if core_items.is_empty() && host_items.is_empty() {
        return ControlEffects {
            rejected: refused_reason(refused),
            ..ControlEffects::default()
        };
    }
    let core = (!core_items.is_empty())
        .then(|| renderer::options::apply_batch(&ctx.renderer, &core_items));
    let hosted = match host {
        Some(host) if !host_items.is_empty() => Some(host.apply_options(&host_items)),
        _ => None,
    };

    let mut applied = Vec::new();
    let mut accepted = false;
    let mut report = |key: &str, result: &Option<Applied>| match result {
        Some(result) => {
            accepted = true;
            if result.changed {
                applied.push(format!("{} set to '{}'", key, result.canonical));
            }
        }
        None => refused.push(format!("{key}: rejected value")),
    };
    if let Some(core) = &core {
        for ((spec, _), result) in core_items.iter().zip(&core.results) {
            report(spec.key, result);
        }
    }
    if let Some(hosted) = &hosted {
        for ((key, _), result) in host_items.iter().zip(&hosted.results) {
            report(key, result);
        }
    }
    let host_changed = hosted.as_ref().is_some_and(|hosted| hosted.changed);
    if host_changed {
        // The core batch marks the config dirty on its own changes; a host
        // change is a config edit too.
        ctx.renderer.mark_dirty();
    }
    let changed = core.as_ref().is_some_and(|core| core.changed) || host_changed;
    let rejected = refused_reason(refused);
    if !changed {
        if !accepted {
            return ControlEffects {
                rejected,
                ..ControlEffects::default()
            };
        }
        // Still published: a value clamped back onto the current one must
        // reach the client that typed it.
        return ControlEffects {
            rejected,
            ..ControlEffects::transient(Notify::Snapshot)
        };
    }
    let mut effects = ControlEffects::dirty(Notify::Snapshot);
    effects.rejected = rejected;
    effects.log_message = Some(format!("OSC option {}", applied.join(", ")));
    match core.map(|core| core.rebuild).unwrap_or(Rebuild::None) {
        Rebuild::None => {}
        Rebuild::Evaluation => {
            effects.trigger_layout_recompute = true;
            effects.evaluation_only = true;
        }
        Rebuild::Topology => effects.trigger_layout_recompute = true,
    }
    effects
}

/// `/control/options/apply [group]`: apply a group of declared options. A
/// host's `Staged` group applies what it staged; a `Live` group, the core's
/// or the host's, has nothing waiting and is only acknowledged with a
/// bundle.
fn apply_option_group(msg: &OscMessage, host: Option<&dyn HostControlHandler>) -> ControlEffects {
    let Some(OscType::String(group)) = msg.args.first() else {
        return ControlEffects::rejected("options/apply: expected a group");
    };
    if let Some(effects) = host.and_then(|host| host.apply_option_group(group)) {
        return effects;
    }
    if renderer::options::OPTION_GROUPS
        .iter()
        .any(|declared| declared.key == group)
    {
        return ControlEffects::transient(Notify::Snapshot);
    }
    ControlEffects::rejected(format!("options/apply: unknown group '{group}'"))
}

fn apply_placement_mode(msg: &OscMessage, ctx: &RuntimeControlContext) -> ControlEffects {
    use renderer::placement::PlacementMode;
    let (Some(OscType::String(name)), Some(OscType::String(mode))) =
        (msg.args.first(), msg.args.get(1))
    else {
        return ControlEffects::rejected("placement mode: expected [family, mode]");
    };
    let mode = if mode.trim().eq_ignore_ascii_case("inherit") {
        None
    } else {
        match PlacementMode::parse(mode) {
            Some(mode) => Some(mode),
            None => {
                return ControlEffects::rejected(format!("placement mode: unknown mode '{mode}'"));
            }
        }
    };
    let changed = {
        let mut live = ctx.renderer.live.write();
        // The family table is the loaded bridge's: a name it does not hold
        // (a client built for another bridge) is refused, not added.
        let Some(family) = live.placement.find(name) else {
            return ControlEffects::rejected(format!("placement mode: unknown family '{name}'"));
        };
        live.placement.family_mut(family).set_mode(mode)
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
        name.trim(),
        mode.map_or("inherit", |m| m.as_str())
    ))
}

fn apply_placement_layout(msg: &OscMessage, ctx: &RuntimeControlContext) -> ControlEffects {
    let (name, arg) = if msg.addr == osc_contract::CONTROL_PLACEMENT_LAYOUT {
        match msg.args.first() {
            Some(OscType::String(name)) => (name.trim(), msg.args.get(1)),
            _ => return ControlEffects::default(),
        }
    } else {
        ("generic", msg.args.first())
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
    {
        let mut live = ctx.renderer.live.write();
        let Some(family) = live.placement.find(name) else {
            log::warn!("OSC placement layout: unknown family '{}'", name);
            return ControlEffects::default();
        };
        live.placement.family_mut(family).layout = layout;
    }
    placement_effects(format!(
        "OSC placement layout: {} {}",
        name,
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
    use renderer::placement::PlacementMode;

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
        let effects = apply_live_control(&write, &ctx, None).expect("handled");
        assert!(effects.mark_dirty);
        assert_eq!(effects.notify, Notify::Snapshot);
        assert!(effects.trigger_layout_recompute);
        assert!(!effects.evaluation_only, "the room moves the geometry");
        let (ratio, rear, _, blend) = room(&ctx);
        assert_eq!(ratio, [1.0, 3.0, 2.0]);
        assert_eq!(rear, 2.5);
        assert_eq!(blend, 0.25);

        // The same message again changes nothing: no rebuild, no Save.
        let again = apply_live_control(&write, &ctx, None).expect("handled");
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
        let effects = apply_live_control(&fff, &ctx, None).expect("handled");
        assert!(!effects.trigger_layout_recompute, "unchanged: no rebuild");

        let rear = msg(
            osc_contract::CONTROL_ROOM_RATIO_REAR,
            vec![OscType::Float(before.1 + 1.0)],
        );
        let effects = apply_live_control(&rear, &ctx, None).expect("handled");
        assert!(effects.mark_dirty && effects.trigger_layout_recompute);
        assert_eq!(room(&ctx).1, before.1 + 1.0);

        // The old handlers' bounds: rear/lower floored, the blend clamped.
        let lower = msg(
            osc_contract::CONTROL_ROOM_RATIO_LOWER,
            vec![OscType::Float(-1.0)],
        );
        apply_live_control(&lower, &ctx, None).expect("handled");
        assert_eq!(room(&ctx).2, 0.01);
        let blend = msg(
            osc_contract::CONTROL_ROOM_RATIO_CENTER_BLEND,
            vec![OscType::Float(3.0)],
        );
        apply_live_control(&blend, &ctx, None).expect("handled");
        assert_eq!(room(&ctx).3, 1.0);

        // A short ratio is dropped, as before.
        let short = msg(osc_contract::CONTROL_ROOM_RATIO, vec![OscType::Float(1.0)]);
        let effects = apply_live_control(&short, &ctx, None).expect("handled");
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
            let effects = apply_live_control(
                &msg(osc_contract::CONTROL_OPTIONS, args.clone()),
                &ctx,
                None,
            )
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
            None,
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
            None,
        )
        .expect("handled");
        assert!(effects.mark_dirty);
        assert!(!effects.trigger_layout_recompute);
        assert!(ctx.renderer.live.read().options.auto_gain);

        let effects = apply_live_control(
            &msg(
                osc_contract::CONTROL_OPTION,
                vec![s("use_loudness"), OscType::Int(1), s("ignored")],
            ),
            &ctx,
            None,
        )
        .expect("handled");
        assert!(effects.mark_dirty);
    }

    /// A grid edit re-samples the tables and keeps the gain models; a batch
    /// that also moves the geometry asks for the full rebuild, once.
    #[test]
    fn a_batch_asks_for_the_widest_rebuild_of_its_groups() {
        let ctx = ctx();
        let grid = apply_live_control(
            &msg(
                osc_contract::CONTROL_OPTIONS,
                vec![
                    s("render_evaluation_mode"),
                    s("precomputed_cartesian"),
                    s("evaluation_cartesian_x_size"),
                    OscType::Int(11),
                    s("evaluation_cartesian_z_neg_size"),
                    OscType::Float(2.4),
                ],
            ),
            &ctx,
            None,
        )
        .expect("handled");
        assert!(grid.trigger_layout_recompute && grid.evaluation_only);
        {
            let live = ctx.renderer.live.read();
            assert_eq!(live.evaluation.cartesian.x_size, 11);
            assert_eq!(live.evaluation.cartesian.z_neg_size, 2, "rounded");
        }

        let mixed = apply_live_control(
            &msg(
                osc_contract::CONTROL_OPTIONS,
                vec![
                    s("evaluation_cartesian_y_size"),
                    OscType::Int(12),
                    s("distance_diffuse_mirror_axes"),
                    s("z+y"),
                ],
            ),
            &ctx,
            None,
        )
        .expect("handled");
        assert!(mixed.trigger_layout_recompute && !mixed.evaluation_only);
        assert_eq!(
            ctx.renderer
                .live
                .read()
                .distance_diffuse_mirror_axes
                .to_string(),
            "yz"
        );
    }

    /// The prefix-family addresses are exact aliases of their rows.
    #[test]
    fn the_prefixed_legacy_addresses_are_aliases() {
        let ctx = ctx();
        let send = |addr: String, arg: OscType| {
            apply_live_control(&msg(&addr, vec![arg]), &ctx, None).expect("handled")
        };
        let effects = send(
            format!("{}threshold", osc_contract::CONTROL_DISTANCE_DIFFUSE_PREFIX),
            OscType::Float(0.0),
        );
        assert!(effects.trigger_layout_recompute && !effects.evaluation_only);
        assert_eq!(ctx.renderer.live.read().distance_diffuse_threshold, 1e-6);

        let effects = send(
            format!(
                "{}distance_max",
                osc_contract::CONTROL_RENDER_EVALUATION_POLAR_PREFIX
            ),
            OscType::Int(3),
        );
        assert!(effects.evaluation_only);
        assert_eq!(ctx.renderer.live.read().evaluation.polar.distance_max, 3.0);

        let effects = send(
            format!("{}external_backend", osc_contract::CONTROL_HYBRID_PREFIX),
            s(" Barycenter "),
        );
        assert!(effects.mark_dirty && effects.trigger_layout_recompute);
        assert_eq!(
            ctx.renderer.live.read().hybrid.external_backend_id,
            "barycenter"
        );
        // A nested hybrid would recurse: refused, as before.
        let effects = send(
            format!("{}internal_backend", osc_contract::CONTROL_HYBRID_PREFIX),
            s("hybrid"),
        );
        assert!(!effects.mark_dirty);
        // The curve is not a registry row: left to the hand-wired handler.
        assert!(
            apply_live_control(
                &msg(
                    &format!("{}curve", osc_contract::CONTROL_HYBRID_PREFIX),
                    vec![OscType::Float(0.0); 4]
                ),
                &ctx,
                None
            )
            .is_none()
        );
    }

    /// The backend resolves built-in aliases and refuses an unknown id.
    #[test]
    fn the_backend_accepts_aliases_and_refuses_unknown_ids() {
        let ctx = ctx();
        let set = |id: &str| {
            apply_live_control(
                &msg(osc_contract::CONTROL_RENDER_BACKEND, vec![s(id)]),
                &ctx,
                None,
            )
            .expect("handled")
        };
        let effects = set("distance");
        assert!(effects.trigger_layout_recompute && !effects.evaluation_only);
        assert_eq!(
            ctx.renderer.live.read().backend_id(),
            "experimental_distance"
        );
        assert!(!set("no_such_backend").mark_dirty);
        assert!(!set("").mark_dirty);
        assert_eq!(
            ctx.renderer.live.read().backend_id(),
            "experimental_distance"
        );
    }

    /// The binaural addresses keep their old validation: a value the old
    /// handler rejected is still rejected, not clamped.
    #[test]
    fn the_binaural_aliases_keep_their_rejections() {
        let ctx = ctx();
        let send = |addr: &str, arg: OscType| {
            apply_live_control(&msg(addr, vec![arg]), &ctx, None).expect("handled")
        };
        let before = ctx.renderer.live.read().binaural.unit_scale_m;
        assert!(
            !send(
                osc_contract::CONTROL_BINAURAL_UNIT_SCALE,
                OscType::Float(0.0)
            )
            .mark_dirty
        );
        assert!(
            !send(
                osc_contract::CONTROL_BINAURAL_REVERB_PREDELAY,
                OscType::Float(-1.0)
            )
            .mark_dirty
        );
        assert!(!send(osc_contract::CONTROL_GAIN, OscType::Float(-0.5)).mark_dirty);
        assert_eq!(ctx.renderer.live.read().binaural.unit_scale_m, before);

        // In range after the check: clamped, as before.
        let effects = send(
            osc_contract::CONTROL_BINAURAL_HEAD_RADIUS,
            OscType::Float(0.3),
        );
        assert!(effects.mark_dirty && !effects.trigger_layout_recompute);
        assert_eq!(ctx.renderer.live.read().binaural.head_radius_m, 0.15);

        // Tri-state: `auto`, or a bool.
        send(
            osc_contract::CONTROL_BINAURAL_BRIR_HEAD_TRACKING,
            OscType::Int(1),
        );
        assert_eq!(
            ctx.renderer.live.read().binaural.brir.head_tracking,
            Some(true)
        );
        send(osc_contract::CONTROL_BINAURAL_BRIR_HEAD_TRACKING, s("auto"));
        assert_eq!(ctx.renderer.live.read().binaural.brir.head_tracking, None);

        // An empty tracking address disables tracking.
        send(
            osc_contract::CONTROL_HEAD_TRACKING_ADDRESS,
            s(" /rotation "),
        );
        assert_eq!(
            ctx.renderer
                .live
                .read()
                .binaural
                .tracking
                .address
                .as_deref(),
            Some("/rotation")
        );
        send(osc_contract::CONTROL_HEAD_TRACKING_ADDRESS, s(""));
        assert_eq!(ctx.renderer.live.read().binaural.tracking.address, None);
    }

    /// The HRIR source reports its selector, the SOFA file included, and
    /// none of the binaural groups asks the engine for a rebuild: their
    /// stages reload by themselves.
    #[test]
    fn a_binaural_batch_asks_for_no_rebuild() {
        let ctx = ctx();
        let effects = apply_live_control(
            &msg(
                osc_contract::CONTROL_OPTIONS,
                vec![
                    s("hrir_source"),
                    s("sofa:/data/hrtf/test.sofa"),
                    s("brir_max_length_s"),
                    OscType::Float(1.0),
                    s("crossover_type"),
                    s("fir"),
                    s("binaural_ear_gains"),
                    OscType::Float(0.5),
                    OscType::Float(0.75),
                ],
            ),
            &ctx,
            None,
        )
        .expect("handled");
        assert!(effects.mark_dirty && !effects.trigger_layout_recompute);
        let live = ctx.renderer.live.read();
        assert_eq!(
            renderer::options::options_json(&live)["hrir_source"],
            "sofa:/data/hrtf/test.sofa"
        );
        assert_eq!(live.binaural.ears[0].gain, 0.5);
        assert_eq!(live.binaural.ears[1].gain, 0.75);
    }

    /// A host declaring one staged option, `stub_rate`, in group `stub`.
    struct StubHost {
        rate: std::sync::Mutex<Option<i64>>,
        applied: std::sync::Mutex<Option<i64>>,
    }

    impl StubHost {
        fn new() -> Self {
            Self {
                rate: std::sync::Mutex::new(None),
                applied: std::sync::Mutex::new(None),
            }
        }
    }

    impl crate::HostControlHandler for StubHost {
        fn handle(&self, _addr: &str, _msg: &OscMessage) -> Option<ControlEffects> {
            None
        }
        fn extend_snapshot(&self) -> Vec<rosc::OscPacket> {
            Vec::new()
        }
        fn amend_saved_config(&self, _render: &mut renderer::config::RenderConfig) {}
        fn option_kind(&self, key: &str) -> Option<renderer::options::OptionKind> {
            (key == "stub_rate")
                .then_some(renderer::options::OptionKind::OptionalInt { min: 1, max: 1000 })
        }
        fn apply_options(
            &self,
            items: &[(&str, renderer::options::RawOptionValue)],
        ) -> renderer::options::HostBatchApplied {
            let mut batch = renderer::options::HostBatchApplied::default();
            for (_, raw) in items {
                let kind = self.option_kind("stub_rate").unwrap();
                let result = renderer::options::raw_optional_int(raw, kind).map(|value| {
                    let changed =
                        std::mem::replace(&mut *self.rate.lock().unwrap(), value) != value;
                    batch.changed |= changed;
                    renderer::options::Applied {
                        canonical: format!("{value:?}"),
                        changed,
                    }
                });
                batch.results.push(result);
            }
            batch
        }
        fn apply_option_group(&self, group: &str) -> Option<ControlEffects> {
            (group == "stub").then(|| {
                *self.applied.lock().unwrap() = *self.rate.lock().unwrap();
                ControlEffects::transient(Notify::Snapshot)
            })
        }
    }

    /// `/control/options` spans the core and the host: one message, both
    /// applied, one notification.
    #[test]
    fn a_batch_spans_core_and_host_options() {
        let ctx = ctx();
        let host = StubHost::new();
        let effects = apply_live_control(
            &msg(
                osc_contract::CONTROL_OPTIONS,
                vec![
                    s("stub_rate"),
                    OscType::Int(48),
                    s("room_ratio_rear"),
                    OscType::Float(3.0),
                ],
            ),
            &ctx,
            Some(&host),
        )
        .expect("handled");
        assert!(effects.mark_dirty && effects.trigger_layout_recompute);
        assert_eq!(*host.rate.lock().unwrap(), Some(48));
        assert_eq!(ctx.renderer.live.read().room_ratio_rear, 3.0);

        // Staged: nothing applied until the group is.
        assert_eq!(*host.applied.lock().unwrap(), None);
        let effects = apply_live_control(
            &msg(osc_contract::CONTROL_OPTIONS_APPLY, vec![s("stub")]),
            &ctx,
            Some(&host),
        )
        .expect("handled");
        assert!(!effects.mark_dirty, "an apply is an action");
        assert_eq!(*host.applied.lock().unwrap(), Some(48));

        // A host change alone is a config edit too; an OSC nil unsets.
        ctx.renderer.mark_clean();
        let effects = apply_live_control(
            &msg(
                osc_contract::CONTROL_OPTION,
                vec![s("stub_rate"), OscType::Nil],
            ),
            &ctx,
            Some(&host),
        )
        .expect("handled");
        assert!(effects.mark_dirty);
        assert!(
            ctx.renderer
                .config_dirty
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        assert_eq!(*host.rate.lock().unwrap(), None);

        // Without that host the key is unknown.
        let effects = apply_live_control(
            &msg(
                osc_contract::CONTROL_OPTION,
                vec![s("stub_rate"), OscType::Int(1)],
            ),
            &ctx,
            None,
        )
        .expect("handled");
        assert!(!effects.mark_dirty);
        // A core live group is only acknowledged; an unknown one ignored.
        let ack = apply_live_control(
            &msg(osc_contract::CONTROL_OPTIONS_APPLY, vec![s("room")]),
            &ctx,
            None,
        )
        .expect("handled");
        assert!(ack.publish_only && !ack.mark_dirty);
        let unknown = apply_live_control(
            &msg(osc_contract::CONTROL_OPTIONS_APPLY, vec![s("nope")]),
            &ctx,
            None,
        )
        .expect("handled");
        assert!(!unknown.publish_only && !unknown.mark_dirty);
    }

    /// `decode_thread` belongs to the embedded engine: a host with audio I/O
    /// refuses it, by key and by its dedicated address, and the rest of a
    /// batch still applies.
    #[test]
    fn a_host_with_audio_refuses_the_embedded_engines_options() {
        let ctx = ctx();
        let host = StubHost::new();
        let before = ctx.renderer.live.read().options.decode_thread;
        for message in [
            msg(osc_contract::CONTROL_DECODE_THREAD, vec![OscType::Int(1)]),
            msg(
                osc_contract::CONTROL_OPTION,
                vec![s("decode_thread"), OscType::Int(1)],
            ),
        ] {
            let effects = apply_live_control(&message, &ctx, Some(&host)).expect("handled");
            assert!(!effects.mark_dirty, "{}", message.addr);
        }
        assert_eq!(ctx.renderer.live.read().options.decode_thread, before);
        let effects = apply_live_control(
            &msg(
                osc_contract::CONTROL_OPTIONS,
                vec![
                    s("decode_thread"),
                    OscType::Int(1),
                    s("auto_gain"),
                    OscType::Int(1),
                ],
            ),
            &ctx,
            Some(&host),
        )
        .expect("handled");
        assert!(effects.mark_dirty);
        assert_eq!(ctx.renderer.live.read().options.decode_thread, before);
        assert!(ctx.renderer.live.read().options.auto_gain);

        // The embedded engine (no host) still takes it.
        let effects = apply_live_control(
            &msg(osc_contract::CONTROL_DECODE_THREAD, vec![OscType::Int(1)]),
            &ctx,
            None,
        )
        .expect("handled");
        assert!(effects.mark_dirty);
    }

    #[test]
    fn placement_mode_bumps_the_options_epoch_only_on_a_real_change() {
        let ctx = ctx();
        let dolby = {
            let mut live = ctx.renderer.live.write();
            live.placement
                .declare("dolby", "Dolby", PlacementMode::Room);
            live.placement.find("dolby").expect("declared")
        };
        let set_room = msg(
            osc_contract::CONTROL_PLACEMENT_MODE,
            vec![
                OscType::String("dolby".into()),
                OscType::String("room".into()),
            ],
        );
        let epoch = ctx.renderer.options_epoch();
        let effects = apply_live_control(&set_room, &ctx, None).expect("handled");
        assert!(effects.mark_dirty);
        assert_eq!(effects.notify, Notify::CoalescedSnapshot);
        assert_eq!(ctx.renderer.options_epoch(), epoch + 1);
        assert_eq!(
            ctx.renderer.live.read().placement.family(dolby).mode,
            Some(PlacementMode::Room)
        );

        // The same value again: still acknowledged, but no re-plan.
        apply_live_control(&set_room, &ctx, None).expect("handled");
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
        let effects = apply_live_control(&clear, &ctx, None).expect("handled");
        assert!(effects.mark_dirty);
        assert_eq!(ctx.renderer.options_epoch(), epoch);
    }

    #[test]
    fn param_writes_are_coalesced_and_non_finite_values_dropped() {
        use renderer::backend_params::ParamValue;
        use renderer::plugin::{PHANTOM_EXTRACT_ID, PluginKind};
        let ctx = ctx();
        ctx.renderer.live.write().options.object_generator_id = "pad".to_string();
        let write = |addr: &str, args: Vec<OscType>| {
            apply_live_control(&msg(addr, args), &ctx, None).expect("handled")
        };
        let effects = write(
            osc_contract::CONTROL_OBJECT_GENERATOR_PARAM,
            vec![OscType::String(" Strength ".into()), OscType::Float(0.25)],
        );
        assert!(effects.mark_dirty);
        assert_eq!(effects.notify, Notify::CoalescedSnapshot);
        let effects = write(
            osc_contract::CONTROL_PHANTOM_EXTRACT_PARAM,
            vec![OscType::String("strength".into()), OscType::Float(f32::NAN)],
        );
        assert!(!effects.mark_dirty);
        // The explicit form addresses a generator that is not selected.
        write(
            osc_contract::CONTROL_OBJECT_GENERATOR_PARAM,
            vec![
                OscType::String("dirac".into()),
                OscType::String("amount".into()),
                OscType::Double(0.5),
            ],
        );
        let params = ctx.renderer.plugin_params();
        assert_eq!(
            params.get(PluginKind::ObjectGenerator, "pad", "strength"),
            Some(&ParamValue::Float(0.25))
        );
        assert_eq!(
            params.get(PluginKind::ObjectGenerator, "dirac", "amount"),
            Some(&ParamValue::Float(0.5))
        );
        assert!(
            params
                .plugin(PluginKind::PhantomExtract, PHANTOM_EXTRACT_ID)
                .is_none()
        );

        // Without a generator selected there is nothing to address.
        ctx.renderer.live.write().options.object_generator_id = "none".to_string();
        let effects = write(
            osc_contract::CONTROL_OBJECT_GENERATOR_PARAM,
            vec![OscType::String("strength".into()), OscType::Float(0.5)],
        );
        assert!(!effects.mark_dirty);
    }

    /// A declared switch or integer is stored in its type whatever the
    /// client sends — a float from an older client, a bool from a new one —
    /// and a value it cannot read is refused.
    #[test]
    fn param_values_are_stored_in_their_declared_type() {
        use renderer::backend_params::{ParamSpec, ParamValue};
        use renderer::plugin::{PHANTOM_EXTRACT_ID, PluginKind, PluginListing};
        let ctx = ctx();
        ctx.renderer.set_phantom_listing(PluginListing {
            id: PHANTOM_EXTRACT_ID,
            label: "Phantom",
            i18n_key: None,
            params: vec![
                ParamSpec::bool("center", "Center", false),
                ParamSpec::int("passes", "Passes", 1, 3, 1),
            ],
        });
        let write = |key: &str, value: OscType| {
            apply_live_control(
                &msg(
                    osc_contract::CONTROL_PHANTOM_EXTRACT_PARAM,
                    vec![OscType::String(key.into()), value],
                ),
                &ctx,
                None,
            )
            .expect("handled")
        };
        let get = |key: &str| {
            ctx.renderer
                .plugin_params()
                .get(PluginKind::PhantomExtract, PHANTOM_EXTRACT_ID, key)
                .cloned()
        };
        write("center", OscType::Float(1.0));
        assert_eq!(get("center"), Some(ParamValue::Bool(true)));
        write("center", OscType::Bool(false));
        assert_eq!(get("center"), Some(ParamValue::Bool(false)));
        write("passes", OscType::Float(2.0));
        assert_eq!(get("passes"), Some(ParamValue::Int(2)));
        let effects = write("passes", OscType::String("many".into()));
        assert!(!effects.mark_dirty);
        assert_eq!(get("passes"), Some(ParamValue::Int(2)));
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
            None,
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
            None,
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
                apply_live_control(&msg(addr, vec![OscType::Float(25.0)]), &ctx, None)
                    .expect("handled")
            };
            assert!(set().publish_only, "{addr}: the first write is a change");
            let again = set();
            assert!(!again.mark_dirty, "{addr}");
            assert!(!again.publish_only, "{addr}");
            assert!(again.persist.is_empty(), "{addr}");
        }
    }
}
