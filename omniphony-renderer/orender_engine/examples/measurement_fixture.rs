//! Hardware-free measurement protocol bench: renders into discarded memory.
//! Run with a free OSC port and a lifetime in seconds (defaults: 19003, 30).
use orender_engine::osc::OscSender;
use renderer::{spatial_renderer::SpatialRenderer, speaker_layout::SpeakerLayout, test_support};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::{Duration, Instant},
};

fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "19003".into())
        .parse()?;
    let seconds: u64 = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "30".into())
        .parse()?;
    let mut renderer = SpatialRenderer::new(test_support::small_grid_spec(
        SpeakerLayout::preset_stereo()?,
    ))?;
    let control = renderer.renderer_control();
    let mut sender = OscSender::new(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1))?;
    sender.attach_renderer_control(control);
    sender.start_listener(port, false)?;
    anyhow::ensure!(sender.is_listening(), "OSC port is unavailable");
    println!("READY 127.0.0.1:{port}; audio is discarded, no hardware is opened");
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let input = [0.0; 480];
    let mut buffer = Vec::new();
    while Instant::now() < deadline {
        let frame = renderer.render_frame(&input, 1, &[], buffer, false)?;
        buffer = renderer.recycle_frame(frame);
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
