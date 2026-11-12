//! Physical-direction layout detectors: the gate behind the i18n contract's rule that app
//! layout is logical (leading/trailing, start/end) and directional icons mirror, so a
//! right-to-left language lays out without per-language code.
//!
//! Like the literal detectors, each is an exact-pattern `&str -> Vec<Finding>` scan, not a
//! parser. A finding's `text` is the matched token, which is what an allowlist entry names
//! for a control that is physically directional on purpose (a media scrubber's time axis).

use std::sync::OnceLock;

use regex::Regex;

use super::{Finding, line_of};

/// Tailwind's physical utilities, each with its logical replacement:
/// `ml/mr → ms/me`, `pl/pr → ps/pe`, `left/right → start/end`, `border-l/r → border-s/e`,
/// `rounded-l/r/tl/… → rounded-s/e/ss/…`, `text-left/right → text-start/end`,
/// `float-left/right → float-start/end`. Borders and corners also come bare (`border-r`); a
/// value may be arbitrary (`left-[calc(50%+1px)]`, `ml-(--gap)`), and v4's `!` may trail it.
const WEB_PHYSICAL: &str = r"-?(?:(?:m[lr]|p[lr]|left|right|scroll-[mp][lr])-[\w./\[\]%()+,-]+|(?:border-[lr]|rounded-[lr]|rounded-[tb][lr])(?:-[\w./\[\]%()+,-]+)?|(?:text|float|clear)-(?:left|right))!?";

/// Lucide icons that point a direction and so must flip under `rtl:`.
const WEB_DIRECTIONAL_ICONS: &str = "ChevronLeft|ChevronRight|ChevronsLeft|ChevronsRight|ArrowLeft|ArrowRight|ArrowBigLeft|ArrowBigRight|CircleArrowLeft|CircleArrowRight|MoveLeft|MoveRight|Undo2|Redo2|LogIn|LogOut";

/// Physical Tailwind utilities and unmirrored directional icons in a `.tsx` source.
pub(crate) fn web_findings(content: &str) -> Vec<Finding> {
    static UTILITY: OnceLock<Regex> = OnceLock::new();
    static ICON: OnceLock<Regex> = OnceLock::new();
    // A class token: optional variants (`hover:`, `sm:`, `*:`, `[&:has(…)]:`,
    // `data-[state=open]:`, `group-hover/item:`) then the utility,
    // bounded by whitespace, a quote, a backtick or a brace so `left - 2` in code and
    // `copyright-notice` do not match.
    let utility = UTILITY.get_or_init(|| {
        Regex::new(&format!(
            r#"(?:^|[\s"'`{{])((?:(?:\[\S*?\]|[\w@*-]+(?:\[\S*?\])?(?:/[\w-]+)?):)*(?:{WEB_PHYSICAL}))(?:$|[\s"'`}}])"#
        ))
        .expect("static regex is valid")
    });
    let icon = ICON.get_or_init(|| {
        // Lucide exports each icon under an `…Icon` alias too (`ChevronRightIcon`).
        Regex::new(&format!(r"<((?:{WEB_DIRECTIONAL_ICONS})(?:Icon)?)\b[^>]*>"))
            .expect("static regex is valid")
    });

    let mut findings = Vec::new();
    // Tokens are separated by a single boundary character, so a manual cursor lets two
    // adjacent tokens (`left-2 mr-2`) share the space between them.
    let mut at = 0;
    while let Some(caps) = utility.captures_at(content, at) {
        let m = caps.get(1).expect("group 1 exists");
        at = m.end();
        let token = m.as_str();
        // `rtl:`/`ltr:` anywhere in the variant chain (`hover:rtl:mr-2`) says "physically,
        // in this direction" on purpose.
        let directed = token
            .rsplit_once(':')
            .is_some_and(|(variants, _)| variants.split(':').any(|v| v == "rtl" || v == "ltr"));
        if directed {
            continue;
        }
        findings.push(Finding {
            line: line_of(content, m.start()),
            text: token.to_string(),
            kind: "web-physical-layout",
        });
    }
    for caps in icon.captures_iter(content) {
        let tag = caps.get(0).expect("group 0 exists");
        if tag.as_str().contains("rtl:") {
            continue;
        }
        let name = caps.get(1).expect("group 1 exists");
        findings.push(Finding {
            line: line_of(content, name.start()),
            text: name.as_str().to_string(),
            kind: "web-unmirrored-icon",
        });
    }
    findings.sort_by_key(|f| f.line);
    findings
}

/// Physical alignment, a forced layout direction and non-mirroring SF Symbols in Swift.
///
/// SwiftUI's own alignment and edge types are already logical; the physical forms that
/// survive are UIKit's `NSTextAlignment`, a forced `.leftToRight`, and the `left`/`right`
/// symbol names, which never flip (their `backward`/`forward` twins do). A symbol naming
/// both directions (`arrow.left.arrow.right`) is symmetric and passes.
pub(crate) fn swift_findings(content: &str) -> Vec<Finding> {
    static SYMBOL: OnceLock<Regex> = OnceLock::new();
    static PHYSICAL: OnceLock<Regex> = OnceLock::new();
    let symbol = SYMBOL.get_or_init(|| {
        Regex::new(r#""((?:chevron|arrow|arrowshape|arrowtriangle|chevron\.compact)\.(?:left|right)(?:\.[\w.]+)?)""#)
            .expect("static regex is valid")
    });
    let physical = PHYSICAL.get_or_init(|| {
        Regex::new(r"(\.textAlignment\s*=\s*\.(?:left|right)\b|NSTextAlignment\.(?:left|right)\b|\.leftToRight\b)")
            .expect("static regex is valid")
    });
    let mut findings: Vec<Finding> = symbol
        .captures_iter(content)
        .filter_map(|caps| caps.get(1))
        .filter(|m| {
            let name = m.as_str();
            !(name.contains("left") && name.contains("right"))
        })
        .map(|m| Finding {
            line: line_of(content, m.start()),
            text: m.as_str().to_string(),
            kind: "swift-unmirrored-symbol",
        })
        .collect();
    findings.extend(
        physical
            .captures_iter(content)
            .filter_map(|caps| caps.get(1))
            .map(|m| Finding {
                line: line_of(content, m.start()),
                text: m.as_str().to_string(),
                kind: "swift-physical-layout",
            }),
    );
    findings.sort_by_key(|f| f.line);
    findings
}

/// Material icons that point a direction; each has an `Icons.AutoMirrored` twin.
const COMPOSE_DIRECTIONAL_ICONS: &str = "ArrowBack|ArrowBackIos|ArrowForward|ArrowForwardIos|ArrowLeft|ArrowRight|KeyboardArrowLeft|KeyboardArrowRight|NavigateBefore|NavigateNext|LastPage|FirstPage|Send|Undo|Redo|Reply|Logout|Login|ExitToApp|OpenInNew|List|Sort|Help|HelpOutline|Article|Comment|Chat|Message|Note|Notes|Label|LabelImportant|TrendingUp|TrendingDown|TrendingFlat|VolumeUp|VolumeDown|VolumeMute|VolumeOff|ViewList|ViewQuilt|ReadMore|MenuOpen|ManageSearch|ShortText|Subject|WrapText|DirectionsWalk|DirectionsRun|DirectionsBike";

/// Physical alignment and padding, and non-auto-mirrored directional icons, in Compose.
pub(crate) fn kotlin_findings(content: &str) -> Vec<Finding> {
    static PHYSICAL: OnceLock<Regex> = OnceLock::new();
    static ICON: OnceLock<Regex> = OnceLock::new();
    let physical = PHYSICAL.get_or_init(|| {
        Regex::new(r"\b(TextAlign\.(?:Left|Right)|absolutePadding|absoluteOffset|AbsoluteAlignment|Arrangement\.Absolute)\b")
            .expect("static regex is valid")
    });
    let icon = ICON.get_or_init(|| {
        Regex::new(&format!(
            r"\b(Icons\.(?:Filled|Outlined|Rounded|Sharp|TwoTone|Default)\.(?:{COMPOSE_DIRECTIONAL_ICONS}))\b"
        ))
        .expect("static regex is valid")
    });
    let mut findings: Vec<Finding> = physical
        .captures_iter(content)
        .filter_map(|caps| caps.get(1))
        .map(|m| Finding {
            line: line_of(content, m.start()),
            text: m.as_str().to_string(),
            kind: "compose-physical-layout",
        })
        .collect();
    findings.extend(
        icon.captures_iter(content)
            .filter_map(|caps| caps.get(1))
            .map(|m| Finding {
                line: line_of(content, m.start()),
                text: m.as_str().to_string(),
                kind: "compose-unmirrored-icon",
            }),
    );
    findings.sort_by_key(|f| f.line);
    findings
}

#[cfg(test)]
mod tests;
