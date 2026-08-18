use anyhow::{Result, anyhow};
use pipewire as pw;
use pw::spa;
use pw::spa::pod::{object, property};

pub(crate) const IEC958_CODECS_PROP: &str = "[ \"TRUEHD\", \"EAC3\" ]";
const IEC958_AUDIO_POSITION_PROP_8CH: &str = "[ FL FR C LFE SL SR RL RR ]";
const IEC958_AUDIO_POSITION_PROP_2CH: &str = "[ FL FR ]";
const SPA_PARAM_BUFFERS_META_TYPE_RAW: u32 = 7;
/// PipeWire's default `clock.quantum-limit`: the largest number of frames a
/// graph cycle can carry.
const PW_QUANTUM_LIMIT_FRAMES: u32 = 8192;

fn iec958_audio_position(channels: u16) -> &'static str {
    match channels {
        2 => IEC958_AUDIO_POSITION_PROP_2CH,
        _ => IEC958_AUDIO_POSITION_PROP_8CH,
    }
}

fn iec958_codec_for_channels(channels: u16) -> u32 {
    match channels {
        2 => spa::sys::SPA_AUDIO_IEC958_CODEC_EAC3,
        _ => spa::sys::SPA_AUDIO_IEC958_CODEC_TRUEHD,
    }
}

/// Copy a borrowed SPA pod into an owned byte buffer.
///
/// Callbacks receive pods that only live for the duration of the call, so a pod
/// that must outlive the callback has to be cloned out first.
pub fn clone_spa_pod_bytes(param: *const spa::sys::spa_pod) -> Option<Vec<u8>> {
    if param.is_null() {
        return None;
    }
    let pod = unsafe { &*param };
    let total_size = std::mem::size_of::<spa::sys::spa_pod>() + pod.size as usize;
    Some(unsafe { std::slice::from_raw_parts(param.cast::<u8>(), total_size) }.to_vec())
}

#[derive(Copy, Clone)]
struct RawSpaPodKey(u32);

impl RawSpaPodKey {
    fn as_raw(&self) -> u32 {
        self.0
    }
}

pub fn build_pipewire_bridge_stream_properties(
    node_name: &str,
    node_description: &str,
    channels: u16,
    sample_rate_hz: u32,
    requested_latency: &str,
) -> pw::properties::PropertiesBox {
    let mut props = pw::properties::PropertiesBox::new();
    let requested_rate = format!("1/{}", sample_rate_hz);
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Playback");
    props.insert(*pw::keys::MEDIA_ROLE, "Movie");
    props.insert("media.class", "Audio/Sink");
    props.insert("node.virtual", "true");
    props.insert("node.name", node_name.to_owned());
    props.insert("node.description", node_description.to_owned());
    props.insert("media.name", node_description.to_owned());
    props.insert("audio.channels", channels.to_string());
    props.insert("audio.position", iec958_audio_position(channels));
    props.insert("iec958.codecs", IEC958_CODECS_PROP);
    props.insert("resample.disable", "true");
    props.insert("node.latency", requested_latency);
    props.insert("node.rate", requested_rate);
    props.insert("node.lock-rate", "true");
    props.insert("node.force-rate", sample_rate_hz.to_string());
    props
}

pub fn build_pipewire_bridge_adapter_properties(
    node_name: &str,
    node_description: &str,
    channels: u16,
    requested_latency: &str,
) -> pw::properties::PropertiesBox {
    let mut props = pw::properties::PropertiesBox::new();
    props.insert("factory.name", "support.null-audio-sink");
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Playback");
    props.insert(*pw::keys::MEDIA_ROLE, "Movie");
    props.insert("media.class", "Audio/Sink");
    props.insert("object.linger", "false");
    props.insert("node.virtual", "true");
    props.insert("node.name", node_name.to_owned());
    props.insert("node.description", node_description.to_owned());
    props.insert("media.name", node_description.to_owned());
    props.insert("audio.channels", channels.to_string());
    props.insert("audio.position", iec958_audio_position(channels));
    props.insert("iec958.codecs", IEC958_CODECS_PROP);
    props.insert("resample.disable", "true");
    props.insert("node.latency", requested_latency);
    props
}

pub fn build_pipewire_bridge_capture_stream_properties(
    node_name: &str,
    node_description: &str,
    channels: u16,
    target_object: &str,
) -> pw::properties::PropertiesBox {
    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Movie");
    props.insert("target.object", target_object);
    props.insert("node.target", target_object);
    props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
    props.insert(*pw::keys::STREAM_MONITOR, "true");
    props.insert("node.name", format!("{node_name}.monitor.capture"));
    props.insert(
        "node.description",
        format!("{node_description} Monitor Capture"),
    );
    props.insert("media.name", format!("{node_description} Monitor Capture"));
    props.insert("audio.channels", channels.to_string());
    props.insert("audio.position", iec958_audio_position(channels));
    props.insert("iec958.codecs", IEC958_CODECS_PROP);
    props.insert("resample.disable", "true");
    props
}

pub fn build_pipewire_bridge_buffers_pod(channels: u16, sample_rate_hz: u32) -> Result<Vec<u8>> {
    build_buffers_pod(channels, sample_rate_hz, std::mem::size_of::<u16>(), 0)
}

/// Buffer pod matching the linear-PCM alternative, whose samples are four bytes
/// wide instead of the two-byte IEC 61937 transport container.
///
/// Unlike an encoded burst, a PCM buffer carries a whole graph cycle, so it has
/// to be sized for the largest quantum PipeWire may pick — not for the 10 ms
/// window that suits the deframer. A buffer smaller than the negotiated quantum
/// makes every cycle arrive oversized and get dropped.
pub fn build_pipewire_bridge_raw_buffers_pod(
    channels: u16,
    sample_rate_hz: u32,
) -> Result<Vec<u8>> {
    build_buffers_pod(
        channels,
        sample_rate_hz,
        std::mem::size_of::<f32>(),
        PW_QUANTUM_LIMIT_FRAMES,
    )
}

/// Byte size a buffer must have to hold one graph cycle.
fn buffers_nominal_size(
    channels: u16,
    sample_rate_hz: u32,
    bytes_per_sample: usize,
    min_frames: u32,
) -> usize {
    let port_bytes_per_frame = (channels as usize) * bytes_per_sample;
    let nominal_frames = sample_rate_hz.div_ceil(100).max(min_frames);
    (port_bytes_per_frame * nominal_frames as usize).max(1024)
}

fn build_buffers_pod(
    channels: u16,
    sample_rate_hz: u32,
    bytes_per_sample: usize,
    min_frames: u32,
) -> Result<Vec<u8>> {
    let port_bytes_per_frame = (channels as usize) * bytes_per_sample;
    let nominal_size = buffers_nominal_size(channels, sample_rate_hz, bytes_per_sample, min_frames);
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamBuffers,
        spa::param::ParamType::Buffers,
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_buffers), Int, 8i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_blocks), Int, 1i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_size), Int, nominal_size as i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_stride), Int, port_bytes_per_frame as i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_align), Int, 16i32),
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_dataType),
            pw::spa::pod::Value::Int(spa::sys::SPA_DATA_MemPtr as i32)
        ),
        property!(
            RawSpaPodKey(SPA_PARAM_BUFFERS_META_TYPE_RAW),
            pw::spa::pod::Value::Int(1i32 << (spa::sys::SPA_META_Header as i32))
        ),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge buffer pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

pub fn build_pipewire_bridge_format_pod(
    sample_rate_hz: u32,
    channels: u16,
    param_type: spa::param::ParamType,
) -> Result<Vec<u8>> {
    build_format_pod(
        sample_rate_hz,
        channels,
        iec958_codec_for_channels(channels),
        param_type,
    )
}

/// Advertise a linear-PCM alternative next to the IEC 61937 formats.
///
/// A sink that only exposes an encoded format is skipped by every client that
/// builds its device list from `SPA_PARAM_EnumFormat`: Kodi's PipeWire sink
/// parses formats/rates/channels first and treats `iec958Codec` as an extra
/// capability, and the PulseAudio compatibility layer never publishes the node
/// at all. Hardware S/PDIF sinks advertise PCM *and* their codecs, so we match
/// that shape — otherwise the node stays invisible to anything but a client
/// that targets it by name.
///
/// Values are wrapped in `Choice` pods because that is what those clients
/// expect to parse; a fixed scalar reads back as an empty capability set.
pub fn build_pipewire_bridge_raw_format_pod(
    sample_rate_hz: u32,
    channels: u16,
    param_type: spa::param::ParamType,
) -> Result<Vec<u8>> {
    let positions = raw_audio_positions(channels);
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamFormat,
        param_type,
        property!(spa::param::format::FormatProperties::MediaType, Id, spa::param::format::MediaType::Audio),
        property!(spa::param::format::FormatProperties::MediaSubtype, Id, spa::param::format::MediaSubtype::Raw),
        property!(
            spa::param::format::FormatProperties::AudioFormat,
            Choice,
            Enum,
            Id,
            spa::param::audio::AudioFormat::F32LE,
            spa::param::audio::AudioFormat::F32LE
        ),
        property!(
            spa::param::format::FormatProperties::AudioRate,
            pw::spa::pod::Value::Choice(pw::spa::pod::ChoiceValue::Int(pw::spa::utils::Choice(
                pw::spa::utils::ChoiceFlags::empty(),
                pw::spa::utils::ChoiceEnum::Enum {
                    default: sample_rate_hz as i32,
                    alternatives: vec![sample_rate_hz as i32],
                }
            )))
        ),
        property!(spa::param::format::FormatProperties::AudioChannels, Int, channels as i32),
        property!(
            spa::param::format::FormatProperties::AudioPosition,
            pw::spa::pod::Value::ValueArray(pw::spa::pod::ValueArray::Id(positions))
        ),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge raw format pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

/// Channel positions matching [`iec958_audio_position`], in the order the
/// renderer's fixed 7.1 input map expects them.
fn raw_audio_positions(channels: u16) -> Vec<pw::spa::utils::Id> {
    let raw: &[u32] = match channels {
        2 => &[
            spa::sys::SPA_AUDIO_CHANNEL_FL,
            spa::sys::SPA_AUDIO_CHANNEL_FR,
        ],
        _ => &[
            spa::sys::SPA_AUDIO_CHANNEL_FL,
            spa::sys::SPA_AUDIO_CHANNEL_FR,
            spa::sys::SPA_AUDIO_CHANNEL_FC,
            spa::sys::SPA_AUDIO_CHANNEL_LFE,
            spa::sys::SPA_AUDIO_CHANNEL_SL,
            spa::sys::SPA_AUDIO_CHANNEL_SR,
            spa::sys::SPA_AUDIO_CHANNEL_RL,
            spa::sys::SPA_AUDIO_CHANNEL_RR,
        ],
    };
    raw.iter().map(|id| pw::spa::utils::Id(*id)).collect()
}

fn build_format_pod(
    sample_rate_hz: u32,
    channels: u16,
    codec: u32,
    param_type: spa::param::ParamType,
) -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamFormat,
        param_type,
        property!(spa::param::format::FormatProperties::MediaType, Id, spa::param::format::MediaType::Audio),
        property!(spa::param::format::FormatProperties::MediaSubtype, Id, spa::param::format::MediaSubtype::Iec958),
        property!(spa::param::format::FormatProperties::AudioFormat, Id, spa::param::audio::AudioFormat::Encoded),
        property!(spa::param::format::FormatProperties::AudioRate, Int, sample_rate_hz as i32),
        property!(spa::param::format::FormatProperties::AudioChannels, Int, channels as i32),
        property!(
            spa::param::format::FormatProperties::AudioIec958Codec,
            pw::spa::pod::Value::Id(pw::spa::utils::Id(codec))
        ),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge input format pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

pub fn build_pipewire_bridge_io_buffers_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamIO,
        spa::param::ParamType::IO,
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_IO_id),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(spa::sys::SPA_IO_Buffers))
        ),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_IO_size), Int, std::mem::size_of::<spa::sys::spa_io_buffers>() as i32),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge IO pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

pub fn build_pipewire_bridge_props_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamProps,
        spa::param::ParamType::Props,
        property!(RawSpaPodKey(spa::sys::SPA_PROP_mute), Bool, false),
        property!(RawSpaPodKey(spa::sys::SPA_PROP_volume), Float, 1.0f32),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge props pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

pub fn build_pipewire_bridge_meta_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamMeta,
        spa::param::ParamType::Meta,
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_META_type),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(spa::sys::SPA_META_Header))
        ),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_META_size), Int, 32i32),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge meta pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

pub fn build_pipewire_bridge_process_latency_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamProcessLatency,
        spa::param::ParamType::ProcessLatency,
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PROCESS_LATENCY_quantum), Float, 0.0f32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PROCESS_LATENCY_rate), Int, 0i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PROCESS_LATENCY_ns), Long, 0i64),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge process latency pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

pub fn build_pipewire_bridge_tag_pod(direction: spa::sys::spa_direction) -> Result<Vec<u8>> {
    let obj = object! {
        pw::spa::utils::SpaTypes::from_raw(spa::sys::SPA_TYPE_OBJECT_ParamTag),
        RawSpaPodKey(spa::sys::SPA_PARAM_Tag),
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_TAG_direction),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(direction))
        ),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge tag pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

pub fn build_pipewire_bridge_latency_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamLatency,
        spa::param::ParamType::Latency,
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_direction),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(spa::sys::SPA_DIRECTION_INPUT))
        ),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_minQuantum), Float, 0.0f32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_maxQuantum), Float, 0.0f32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_minRate), Int, 0i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_maxRate), Int, 0i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_minNs), Long, 0i64),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_maxNs), Long, 0i64),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge latency pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

pub fn build_pipewire_bridge_enum_port_config_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamPortConfig,
        spa::param::ParamType::EnumPortConfig,
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_direction),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(spa::sys::SPA_DIRECTION_INPUT))
        ),
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_mode),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(
                spa::sys::SPA_PARAM_PORT_CONFIG_MODE_none
            ))
        ),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_monitor), Bool, false),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_control), Bool, false),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge enum port config pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

pub fn build_pipewire_bridge_port_config_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamPortConfig,
        spa::param::ParamType::PortConfig,
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_direction),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(spa::sys::SPA_DIRECTION_INPUT))
        ),
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_mode),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(
                spa::sys::SPA_PARAM_PORT_CONFIG_MODE_none
            ))
        ),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_monitor), Bool, false),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_control), Bool, false),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge port config pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

pub fn spa_param_info(id: u32, flags: u32) -> spa::sys::spa_param_info {
    spa::sys::spa_param_info {
        id,
        flags,
        user: 0,
        seq: 0,
        padding: [0; 4],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PCM buffer carries a whole graph cycle, so sizing it from the 10 ms
    /// window that suits the deframer makes every cycle arrive oversized and
    /// get dropped before it reaches the PCM consumer.
    #[test]
    fn raw_buffers_hold_a_full_quantum() {
        let channels = 8u16;
        let stride = channels as usize * std::mem::size_of::<f32>();
        for rate in [48_000u32, 96_000, 192_000] {
            let size = buffers_nominal_size(
                channels,
                rate,
                std::mem::size_of::<f32>(),
                PW_QUANTUM_LIMIT_FRAMES,
            );
            assert!(
                size >= PW_QUANTUM_LIMIT_FRAMES as usize * stride,
                "rate {rate}: buffer of {size} B cannot hold a {PW_QUANTUM_LIMIT_FRAMES}-frame quantum",
            );
        }
    }

    /// The encoded path keeps its 10 ms sizing: bursts are far smaller than a
    /// quantum, and oversizing them would add latency to the deframer.
    #[test]
    fn encoded_buffers_track_the_transport_window() {
        let size = buffers_nominal_size(2, 192_000, std::mem::size_of::<u16>(), 0);
        assert_eq!(size, 2 * std::mem::size_of::<u16>() * 1_920);
    }

    #[test]
    fn raw_positions_match_the_fixed_input_map() {
        assert_eq!(raw_audio_positions(8).len(), 8);
        assert_eq!(raw_audio_positions(2).len(), 2);
    }
}
