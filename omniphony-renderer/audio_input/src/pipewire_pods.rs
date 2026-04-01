use anyhow::{Result, anyhow};
use pipewire as pw;
use pw::spa;
use pw::spa::pod::{object, property};

const TRUEHD_ONLY_IEC958_CODECS_PROP: &str = "[ \"TRUEHD\" ]";
const IEC958_AUDIO_POSITION_PROP: &str = "[ FL FR C LFE SL SR RL RR ]";

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
    props.insert("audio.position", IEC958_AUDIO_POSITION_PROP);
    props.insert("iec958.codecs", TRUEHD_ONLY_IEC958_CODECS_PROP);
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
    props.insert("audio.position", IEC958_AUDIO_POSITION_PROP);
    props.insert("iec958.codecs", TRUEHD_ONLY_IEC958_CODECS_PROP);
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
    props.insert("audio.position", IEC958_AUDIO_POSITION_PROP);
    props.insert("iec958.codecs", TRUEHD_ONLY_IEC958_CODECS_PROP);
    props.insert("resample.disable", "true");
    props
}

pub fn build_pipewire_bridge_buffers_pod(channels: u16, sample_rate_hz: u32) -> Result<Vec<u8>> {
    let port_bytes_per_frame = (channels as usize) * std::mem::size_of::<u16>();
    let nominal_frames = sample_rate_hz.div_ceil(100);
    let nominal_size = (port_bytes_per_frame * nominal_frames as usize).max(1024);
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
            RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_metaType),
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

pub fn build_pipewire_bridge_format_pod(
    sample_rate_hz: u32,
    channels: u16,
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
            pw::spa::pod::Value::Id(pw::spa::utils::Id(
                spa::sys::SPA_AUDIO_IEC958_CODEC_TRUEHD
            ))
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

pub fn spa_param_info(id: u32, flags: u32) -> spa::sys::spa_param_info {
    spa::sys::spa_param_info {
        id,
        flags,
        user: 0,
        seq: 0,
        padding: [0; 4],
    }
}
