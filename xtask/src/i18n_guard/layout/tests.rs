use super::*;

fn texts(findings: &[Finding]) -> Vec<&str> {
    findings.iter().map(|f| f.text.as_str()).collect()
}

#[test]
fn web_physical_utilities_are_found_with_their_variants() {
    let src =
        r#"<div className="absolute top-2 left-2 mr-2 hover:pl-8 sm:text-left text-right -ml-1">"#;
    assert_eq!(
        texts(&web_findings(src)),
        [
            "left-2",
            "mr-2",
            "hover:pl-8",
            "sm:text-left",
            "text-right",
            "-ml-1"
        ]
    );
}

#[test]
fn web_logical_utilities_pass() {
    let src = r#"<div className="absolute top-2 start-2 me-2 ps-8 text-start text-end ms-auto px-4 mx-2 inset-x-0">"#;
    assert!(web_findings(src).is_empty());
}

#[test]
fn a_web_utility_behind_a_direction_variant_is_deliberate() {
    // `rtl:`/`ltr:` exist precisely to say "physically, in this direction".
    let src = r#"<span className="rtl:mr-2 ltr:text-left">"#;
    assert!(web_findings(src).is_empty());
}

#[test]
fn web_words_that_merely_contain_left_or_right_pass() {
    let src = r#"<p className="copyright-notice">left-handed mode</p> const remainder = left - 2;"#;
    assert!(web_findings(src).is_empty());
}

#[test]
fn a_web_utility_inside_an_arbitrary_variant_is_found() {
    let src = r"'p-4 [&:has([role=checkbox])]:pr-0'";
    assert_eq!(texts(&web_findings(src)), ["[&:has([role=checkbox])]:pr-0"]);
}

#[test]
fn an_unmirrored_web_directional_icon_is_found_and_a_mirrored_one_passes() {
    let src = r#"<ChevronRight className="ms-auto h-4 w-4" />
<ArrowLeft className="h-4 w-4 rtl:rotate-180" />"#;
    let found = web_findings(src);
    assert_eq!(texts(&found), ["ChevronRight"]);
    assert_eq!(found[0].line, 1);
    assert_eq!(found[0].kind, "web-unmirrored-icon");
}

#[test]
fn swift_physical_symbols_and_alignment_are_found() {
    let src = r#"Image(systemName: "chevron.right")
Label("x", systemImage: "arrow.left.circle")
label.textAlignment = .left
.environment(\.layoutDirection, .leftToRight)"#;
    assert_eq!(
        texts(&swift_findings(src)),
        [
            "chevron.right",
            "arrow.left.circle",
            ".textAlignment = .left",
            ".leftToRight"
        ]
    );
}

#[test]
fn swift_mirroring_symbols_and_leading_trailing_pass() {
    let src = r#"Image(systemName: "chevron.forward")
Image(systemName: "arrow.backward")
Image(systemName: "arrow.left.arrow.right")
.padding(.leading, 8).frame(alignment: .trailing)"#;
    assert!(swift_findings(src).is_empty());
}

#[test]
fn compose_physical_layout_and_unmirrored_icons_are_found() {
    let src = r"Text(x, textAlign = TextAlign.Left)
Modifier.absolutePadding(left = 4.dp)
Icon(Icons.Filled.ArrowBack, null)
Icon(Icons.Default.KeyboardArrowRight, null)
Box(contentAlignment = AbsoluteAlignment.TopLeft)";
    assert_eq!(
        texts(&kotlin_findings(src)),
        [
            "TextAlign.Left",
            "absolutePadding",
            "Icons.Filled.ArrowBack",
            "Icons.Default.KeyboardArrowRight",
            "AbsoluteAlignment",
        ]
    );
}

#[test]
fn compose_logical_layout_and_auto_mirrored_icons_pass() {
    let src = r"Text(x, textAlign = TextAlign.Start)
Modifier.padding(start = 4.dp)
Icon(Icons.AutoMirrored.Filled.ArrowBack, null)
Icon(Icons.Filled.Add, null)";
    assert!(kotlin_findings(src).is_empty());
}

#[test]
fn bare_physical_borders_and_corners_are_found() {
    let src = r#"<aside className="w-64 border-r bg-background rounded-l rounded-tl-md border-s">"#;
    assert_eq!(
        texts(&web_findings(src)),
        ["border-r", "rounded-l", "rounded-tl-md"]
    );
}

#[test]
fn named_and_starred_variants_arbitrary_values_and_important_are_found() {
    let src = r"'data-[state=open]:pl-2 group-hover/item:ml-2 *:mr-1 left-[calc(50%+1px)] ml-(--gap) mr-2!'";
    assert_eq!(
        texts(&web_findings(src)),
        [
            "data-[state=open]:pl-2",
            "group-hover/item:ml-2",
            "*:mr-1",
            "left-[calc(50%+1px)]",
            "ml-(--gap)",
            "mr-2!"
        ]
    );
}

#[test]
fn a_direction_variant_later_in_the_chain_is_deliberate() {
    let src = r#"<span className="hover:rtl:mr-2 sm:ltr:text-left">"#;
    assert!(web_findings(src).is_empty());
}

#[test]
fn a_lucide_icon_alias_is_found() {
    let src = r"<ChevronRightIcon className='size-4' />";
    assert_eq!(texts(&web_findings(src)), ["ChevronRightIcon"]);
}

#[test]
fn every_compose_icon_style_and_more_mirrored_icons_are_found() {
    let src = r"Icons.Outlined.Help Icons.Rounded.Label Icons.Sharp.ViewList Icons.TwoTone.Send
TextAlign.Right Modifier.absoluteOffset(x = 1.dp) Arrangement.Absolute.Left";
    assert_eq!(
        texts(&kotlin_findings(src)),
        [
            "Icons.Outlined.Help",
            "Icons.Rounded.Label",
            "Icons.Sharp.ViewList",
            "Icons.TwoTone.Send",
            "TextAlign.Right",
            "absoluteOffset",
            "Arrangement.Absolute",
        ]
    );
}

#[test]
fn swift_ui_kit_alignment_and_other_symbol_families_are_found() {
    let src = r#"label.textAlignment = NSTextAlignment.right
Image(systemName: "arrowtriangle.left.fill")
Image(systemName: "chevron.compact.right")"#;
    assert_eq!(
        texts(&swift_findings(src)),
        [
            "NSTextAlignment.right",
            "arrowtriangle.left.fill",
            "chevron.compact.right"
        ]
    );
}
