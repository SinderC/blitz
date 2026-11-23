use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;

/// Regression test for https://github.com/DioxusLabs/blitz/issues/407
///
/// `resolve_stylist` used to panic with "invalid key" when a CSS-animated node
/// was removed between two `resolve` calls. Verify the second resolve is safe.
#[test]
fn resolve_does_not_panic_after_removing_animated_node() {
    let html = r#"
        <style>
          @keyframes pulse { 0% { opacity: 1; } 100% { opacity: 0.5; } }
          .pulse { animation: pulse 2s infinite; }
        </style>
        <div id="pulse-node" class="pulse">animated</div>
    "#;

    let mut doc = HtmlDocument::from_html(html, DocumentConfig::default());
    doc.resolve(0.0);

    let pulse_id = doc.get_element_by_id("pulse-node");
    if let Some(id) = pulse_id {
        doc.mutate().remove_and_drop_node(id);
    }

    // Must not panic: stale animation entry should be skipped safely.
    doc.resolve(0.1);
}

/// An animation taken away mid-run lets go of the value it gave: Stylo cancels it
/// after the restyle has applied its current frame, so the element must be styled
/// once more without it, as a loading pulse ending must not leave the icon faded.
#[test]
fn removed_animation_lets_go_of_its_value() {
    use blitz_dom::{CopyStyleGetters, LocalName, QualName, ns};

    let html = r#"
        <style>
          @keyframes pulse { to { opacity: 0; } }
          .pulse { animation: pulse 1s linear infinite; }
        </style>
        <div id="pulse-node" class="pulse">animated</div>
    "#;
    let mut doc = HtmlDocument::from_html(html, DocumentConfig::default());
    let id = doc.get_element_by_id("pulse-node").unwrap();
    let opacity = |doc: &HtmlDocument| {
        doc.get_node(id)
            .unwrap()
            .primary_styles()
            .unwrap()
            .clone_opacity()
    };
    for i in 0..=5 {
        doc.resolve(i as f64 * 0.1);
    }
    assert!(opacity(&doc) < 0.9, "the pulse runs");

    doc.mutate()
        .set_attribute(id, QualName::new(None, ns!(), LocalName::from("class")), "");
    let mut time = 0.6;
    doc.resolve(time);
    while doc.is_animating() {
        time += 0.1;
        doc.resolve(time);
        assert!(time < 2.0, "the cancelled animation keeps ticking");
    }
    assert_eq!(opacity(&doc), 1.0);
}
