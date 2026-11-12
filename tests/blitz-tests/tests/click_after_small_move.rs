//! A press that moves a few pixels before its release begins a text selection drag,
//! but it is still a click, as in browsers: a trackpad click often moves that far.
//! The click goes to the nearest element holding both the press and the release,
//! except in a link, where the drag selects the link's text instead.

use blitz_test_harness::{Harness, HarnessOptions, mouse_pointer_event};
use blitz_traits::events::UiEvent;

fn harness(html: &str) -> Harness {
    Harness::from_html_with(
        html,
        HarnessOptions {
            width: 300,
            height: 200,
            ..Default::default()
        },
    )
}

/// The events dispatched for a press at `from`, a move to `to` and a release there.
fn press_move_release(harness: &mut Harness, from: (f32, f32), to: (f32, f32)) -> Vec<String> {
    harness.dispatch_recorded([
        UiEvent::PointerDown(mouse_pointer_event(from.0, from.1)),
        UiEvent::PointerMove(mouse_pointer_event(to.0, to.1)),
        UiEvent::PointerUp(mouse_pointer_event(to.0, to.1)),
    ])
}

fn is_checked(harness: &Harness, selector: &str) -> bool {
    let id = harness.node(selector);
    harness
        .base()
        .get_node(id)
        .and_then(|node| node.element_data())
        .and_then(|el| el.checkbox_input_checked())
        .unwrap()
}

const TEXT: &str = r#"<html><body style="margin:0">
    <p id="text" style="margin:0; width:300px; font-size:20px">Some text to press on</p>
    <p style="margin:0; width:300px; height:100px">Another paragraph</p>
</body></html>"#;

#[test]
fn a_small_move_over_text_still_clicks() {
    let mut harness = harness(TEXT);
    let names = press_move_release(&mut harness, (20.0, 10.0), (25.0, 13.0));
    assert!(
        names.contains(&"click".to_string()),
        "expected a click, got {names:?}"
    );
}

#[test]
fn a_drag_across_elements_clicks_their_common_ancestor() {
    let mut harness = harness(
        r#"<html><body style="margin:0">
        <label style="display:block; width:300px; font-size:20px">
            <span>First span</span><br><span>Second span</span>
            <input id="box" type="checkbox">
        </label>
        <p style="margin:0; height:100px">Outside</p>
    </body></html>"#,
    );
    // From the first line's span to the second's (inline spans have no center).
    harness.drag((20.0, 12.0), (30.0, 36.0), 4);
    assert!(
        is_checked(&harness, "#box"),
        "a drag from one span to another in a label should click the label"
    );
}

#[test]
fn a_drag_out_of_an_element_does_not_click_it() {
    let mut harness = harness(
        r#"<html><body style="margin:0">
        <label style="display:block; width:300px; height:40px; font-size:20px">
            <span>In the label</span>
            <input id="box" type="checkbox">
        </label>
        <p id="out" style="margin:0; height:100px; font-size:20px">Outside</p>
    </body></html>"#,
    );
    harness.drag((20.0, 12.0), harness.center_of("#out"), 4);
    assert!(
        !is_checked(&harness, "#box"),
        "a drag that ends outside the label must not click it"
    );
}

#[test]
fn a_drag_inside_a_link_selects_instead_of_clicking() {
    let mut harness = harness(
        r#"<html><body style="margin:0">
        <p style="margin:0; width:300px; font-size:20px"><a href="https://example.com/">A long link to select from</a></p>
    </body></html>"#,
    );
    let names = press_move_release(&mut harness, (20.0, 10.0), (120.0, 12.0));
    assert!(
        !names.contains(&"click".to_string()),
        "a selection drag inside a link must not click it, got {names:?}"
    );
}

#[test]
fn a_press_without_a_move_still_clicks() {
    let mut harness = harness(TEXT);
    let names = press_move_release(&mut harness, (20.0, 10.0), (20.0, 10.0));
    assert!(names.contains(&"click".to_string()), "got {names:?}");
}
