use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};

const HTML: &str = r#"<!doctype html><html><head><style>
.root { width: 100px; height: 10px; }
@media (prefers-color-scheme: dark) { .root[data-theme='system'] { width: 200px; } }
</style></head><body><div class="root" data-theme="system"></div></body></html>"#;

#[test]
fn a_color_scheme_change_reapplies_media_queries() {
    let mut doc = HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let id = doc.query_selector(".root").unwrap().unwrap();
    assert_eq!(doc.get_node(id).unwrap().final_layout().size.width, 100.0);

    doc.viewport_mut().color_scheme = ColorScheme::Dark;
    doc.resolve(0.0);
    assert_eq!(doc.get_node(id).unwrap().final_layout().size.width, 200.0);

    doc.viewport_mut().color_scheme = ColorScheme::Light;
    doc.resolve(0.0);
    assert_eq!(doc.get_node(id).unwrap().final_layout().size.width, 100.0);
}
