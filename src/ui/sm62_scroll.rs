// Keep the original 900-point settings column centered, including the same
// 18-point right gutter (12 gap + 6 track) as the other settings list viewports.
// Do not give ScrollArea the full window width: its minimum content width wins
// over the page's set_width(900) and stretches every shared settings row.
fn sm62_display_scroll<R>(ui: &mut egui::Ui, body: egui::Rect, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::scroll_area::ScrollAreaOutput<R> {
    let metrics = crate::ui_style::ResponsiveMetrics::from_ctx(ui.ctx());
    let gutter = metrics.settings_content_width() - metrics.settings_row_content_width();
    let width = body.width().min(900.0 + gutter);
    let column = egui::Rect::from_center_size(body.center(),egui::vec2(width,body.height().max(1.0)));
    let track = egui::Rect::from_min_max(egui::pos2(column.right()-metrics.value(6.0),column.top()),column.max);
    let viewport = egui::Rect::from_min_max(column.min,egui::pos2((column.right()-gutter).max(column.left()+1.0),column.bottom()));
    let mut output = crate::ui_style::allocate_ui_at_rect(ui,viewport,|ui| {
        ui.with_layout(egui::Layout::top_down(egui::Align::Center),|ui| {
            egui::ScrollArea::vertical().id_salt("sm62_display_settings")
                .auto_shrink([false,false]).max_height(viewport.height())
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui,|ui| {ui.set_min_width(viewport.width());add(ui)})
        }).inner
    }).inner;
    let max_offset = (output.content_size.y-output.inner_rect.height()).max(0.0);
    if max_offset > 0.5 {
        let height = (track.height()*output.inner_rect.height()/output.content_size.y)
            .clamp(24.0_f32.min(track.height()),track.height());
        let response = ui.interact(track,output.id.with("floating_track"),egui::Sense::click_and_drag());
        if response.clicked() || response.dragged() {
            if let Some(pos)=response.interact_pointer_pos() {
                output.state.offset.y=((pos.y-track.top()-height/2.0)/(track.height()-height).max(1.0)).clamp(0.0,1.0)*max_offset;
                output.state.store(ui.ctx(),output.id);
                ui.ctx().request_repaint();
            }
        } else if response.hovered() {
            let delta=ui.input(|input|input.smooth_scroll_delta.y);
            if delta!=0.0 {
                output.state.offset.y=(output.state.offset.y-delta).clamp(0.0,max_offset);
                output.state.store(ui.ctx(),output.id);
                ui.ctx().request_repaint();
            }
        }
        crate::ui_style::paint_floating_scrollbar_handle(ui,track,height,output.state.offset.y/max_offset,response.hovered()||response.dragged());
    }
    #[cfg(test)]
    ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new("sm62_scroll_fixture"),(output.id,max_offset)));
    output
}
