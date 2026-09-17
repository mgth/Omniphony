//! Minimal native window for separating toolkit/driver failures from Studio.
//! It starts no renderer, listener, service manager, or preferences writer.
struct WindowSmoke;
impl eframe::App for WindowSmoke {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _: &mut eframe::Frame) {
        ui.label("Isolated close smoke");
    }
    fn on_exit(&mut self) {
        eprintln!("smoke on_exit reached");
    }
}
fn main() -> eframe::Result {
    eframe::run_native(
        "Studio close smoke",
        eframe::NativeOptions::default(),
        Box::new(|_| Ok(Box::new(WindowSmoke))),
    )
}
