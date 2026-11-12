use blitz_traits::node_id::NodeId;
use style::values::computed::Contain;
use taffy::{
    BoxSizing, CoreStyle as _, LayoutInput, LayoutOutput, MaybeResolve as _, ResolveOrZero as _,
    RunMode, Size,
};

use super::replaced::{IntrinsicSizes, ReplacedContext, compute_replaced_layout};
use super::{LayoutPassState, resolve_calc_value};

impl LayoutPassState<'_> {
    pub(crate) fn compute_select_layout(
        &mut self,
        id: NodeId,
        mut inputs: LayoutInput,
    ) -> LayoutOutput {
        let node = &self.nodes[id];
        let style = node.layout_style();
        let containment = node.primary_styles().unwrap().get_box().contain;
        let layout = node
            .element_data()
            .unwrap()
            .inline_layout_data
            .as_ref()
            .unwrap();
        let scale = layout.layout.scale();
        let width = if containment.contains(Contain::INLINE_SIZE) || layout.text.is_empty() {
            0.0
        } else {
            layout.layout.width() / scale
        };
        let height = layout.layout.height() / scale;
        let pb = style
            .padding()
            .resolve_or_zero(inputs.parent_size.width, resolve_calc_value)
            + style
                .border()
                .resolve_or_zero(inputs.parent_size.width, resolve_calc_value);
        let context = ReplacedContext {
            intrinsic_sizes: IntrinsicSizes {
                width: Some(width),
                height: Some(height),
                ratio: None,
            },
            default_object_size: Size::ZERO,
        };
        if style.box_sizing() == BoxSizing::BorderBox {
            if let Some(ratio) = style
                .aspect_ratio()
                .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
            {
                let preferred = style
                    .size()
                    .maybe_resolve(inputs.parent_size, resolve_calc_value);
                match (
                    inputs.known_dimensions.width.or(preferred.width),
                    inputs.known_dimensions.height.or(preferred.height),
                ) {
                    (None, Some(height)) => {
                        inputs.known_dimensions = Size {
                            width: Some(height * ratio),
                            height: Some(height),
                        }
                    }
                    (Some(width), None) => {
                        inputs.known_dimensions = Size {
                            width: Some(width),
                            height: Some(width / ratio),
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut output = compute_replaced_layout(inputs, &style, resolve_calc_value, &context);
        if style.aspect_ratio().is_some()
            && style.size().width.is_auto()
            && style.min_size().width.is_auto()
        {
            let max_width = style
                .max_size()
                .width
                .maybe_resolve(inputs.parent_size.width, resolve_calc_value);
            let min_width =
                (width + pb.horizontal_axis_sum()).min(max_width.unwrap_or(f32::INFINITY));
            output.size.width = output.size.width.max(min_width);
        }
        // Browsers center a select's label in the block axis of its content box, so a
        // `height` taller (or shorter) than the line leaves equal space above and below.
        let block_offset = (output.size.height - pb.vertical_axis_sum() - height) / 2.0;
        if let Some(line) = layout
            .layout
            .get(0)
            .filter(|_| !style.contain().suppresses_baseline())
        {
            let baseline = pb.top + block_offset + line.metrics().baseline / scale;
            output.baselines = taffy::Baselines {
                first: Some(baseline),
                last: Some(baseline),
            };
        }
        // The style borrows the node, which the offset below is written to.
        drop(style);
        if inputs.run_mode == RunMode::PerformLayout {
            if let Some(layout) = self.nodes[id]
                .element_data_mut()
                .and_then(|el| el.inline_layout_data.as_mut())
            {
                layout.block_offset = block_offset;
            }
        }
        output
    }
}
