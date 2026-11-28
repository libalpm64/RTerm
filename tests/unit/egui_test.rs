//! Headless egui passes inspect UI geometry and behavior, not GPU textures.
use eframe::egui;

pub(crate) trait TestContextExt {
    fn run_test_pass(
        &self,
        input: egui::RawInput,
        render: impl FnMut(&egui::Context),
    ) -> egui::FullOutput;
}

impl TestContextExt for egui::Context {
    fn run_test_pass(
        &self,
        input: egui::RawInput,
        render: impl FnMut(&egui::Context),
    ) -> egui::FullOutput {
        let mut output = self.run_pass(input, render);
        // There is deliberately no renderer in these fixtures. Retain all
        // geometry, widget output and viewport commands for assertions.
        output.textures_delta.clear();
        output
    }
}
