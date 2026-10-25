pub const MAX_ICON_BYTES: usize = 65_536;
const SVG: &str = "http://www.w3.org/2000/svg";

fn white(paint: &str) -> bool {
    paint.eq_ignore_ascii_case("white")
        || paint.eq_ignore_ascii_case("#fff")
        || paint.eq_ignore_ascii_case("#ffffff")
}
fn number(value: &str) -> bool {
    value
        .parse::<f64>()
        .is_ok_and(|n| n.is_finite() && n.abs() <= 1_000_000.0)
}
fn numbers(value: &str) -> Option<Vec<&str>> {
    let values: Vec<_> = value
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|v| !v.is_empty())
        .collect();
    (!values.is_empty() && values.len() <= 4096 && values.iter().all(|v| number(v)))
        .then_some(values)
}
fn transform(mut value: &str) -> bool {
    for _ in 0..16 {
        value = value.trim();
        if value.is_empty() {
            return true;
        }
        let Some((name, rest)) = value.split_once('(') else {
            return false;
        };
        let Some((args, rest)) = rest.split_once(')') else {
            return false;
        };
        let Some(args) = numbers(args) else {
            return false;
        };
        if !match name.trim() {
            "matrix" => args.len() == 6,
            "translate" | "scale" => (1..=2).contains(&args.len()),
            "rotate" => matches!(args.len(), 1 | 3),
            "skewX" | "skewY" => args.len() == 1,
            _ => false,
        } {
            return false;
        }
        value = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
    }
    value.is_empty()
}
fn attribute(tag: &str, name: &str, value: &str) -> bool {
    match name {
        "fill" | "stroke" => white(value) || value == "none",
        "fill-rule" | "clip-rule" => matches!(value, "nonzero" | "evenodd"),
        "stroke-linecap" => matches!(value, "butt" | "round" | "square"),
        "stroke-linejoin" => matches!(value, "miter" | "round" | "bevel"),
        "stroke-width" | "stroke-miterlimit" => {
            number(value) && value.parse::<f64>().unwrap() >= 0.0
        }
        "opacity" | "fill-opacity" | "stroke-opacity" => {
            number(value) && (0.0..=1.0).contains(&value.parse::<f64>().unwrap())
        }
        "transform" => transform(value),
        "viewBox" if tag == "svg" => numbers(value).is_some_and(|v| {
            v.len() == 4 && v[2].parse::<f64>().unwrap() > 0.0 && v[3].parse::<f64>().unwrap() > 0.0
        }),
        "width" | "height" if matches!(tag, "svg" | "rect") => {
            number(value) && value.parse::<f64>().unwrap() > 0.0
        }
        "version" if tag == "svg" => matches!(value, "1.0" | "1.1" | "2.0"),
        "preserveAspectRatio" if tag == "svg" => matches!(
            value,
            "none" | "xMidYMid" | "xMidYMid meet" | "xMidYMid slice"
        ),
        "d" if tag == "path" => {
            !value.is_empty()
                && value.len() <= 32_768
                && value.bytes().all(|b| {
                    b.is_ascii_whitespace() || b"0123456789+-.eEMmZzLlHhVvCcSsQqTtAa,".contains(&b)
                })
        }
        "points" if matches!(tag, "polyline" | "polygon") => {
            numbers(value).is_some_and(|v| v.len() >= 4 && v.len() % 2 == 0)
        }
        "x" | "y" if tag == "rect" => number(value),
        "cx" | "cy" if matches!(tag, "circle" | "ellipse") => number(value),
        "r" if tag == "circle" => number(value) && value.parse::<f64>().unwrap() >= 0.0,
        "rx" | "ry" if matches!(tag, "rect" | "ellipse") => {
            number(value) && value.parse::<f64>().unwrap() >= 0.0
        }
        "x1" | "x2" | "y1" | "y2" if tag == "line" => number(value),
        _ => false,
    }
}

pub fn validate_icon(bytes: &[u8]) -> Result<(), String> {
    let invalid = || "Icon must be a simple white SVG with safe geometry only".to_owned();
    if bytes.is_empty() || bytes.len() > MAX_ICON_BYTES {
        return Err(invalid());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    if text.contains('&') || text.contains("<?") {
        return Err(invalid());
    }
    let doc = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 2048,
            ..Default::default()
        },
    )
    .map_err(|_| invalid())?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some(SVG) {
        return Err(invalid());
    }
    let (mut elements, mut painted) = (0, false);
    for node in doc.descendants() {
        if node.is_pi() || node.is_text() && !node.text().unwrap_or("").trim().is_empty() {
            return Err(invalid());
        }
        if !node.is_element() {
            continue;
        }
        elements += 1;
        let tag = node.tag_name().name();
        if elements > 512
            || node.ancestors().filter(|n| n.is_element()).count() > 16
            || node.attributes().len() > 24
            || node.tag_name().namespace() != Some(SVG)
            || !matches!(
                tag,
                "svg"
                    | "g"
                    | "path"
                    | "rect"
                    | "circle"
                    | "ellipse"
                    | "line"
                    | "polyline"
                    | "polygon"
            )
            || tag == "svg" && node != root
            || node
                .parent_element()
                .is_some_and(|p| !matches!(p.tag_name().name(), "svg" | "g"))
            || node.namespaces().any(|ns| ns.uri() != SVG)
        {
            return Err(invalid());
        }
        for attr in node.attributes() {
            if attr.namespace().is_some() || !attribute(tag, attr.name(), attr.value()) {
                return Err(invalid());
            }
        }
        if !matches!(tag, "svg" | "g") {
            let fill = node
                .ancestors()
                .find_map(|n| n.attribute("fill"))
                .unwrap_or("black");
            let stroke = node
                .ancestors()
                .find_map(|n| n.attribute("stroke"))
                .unwrap_or("none");
            if tag != "line" && !white(fill) && fill != "none" || !white(stroke) && stroke != "none"
            {
                return Err(invalid());
            }
            painted |= tag != "line" && white(fill) || white(stroke);
        }
    }
    if !painted {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn svg(body: &str) -> Vec<u8> {
        format!(r#"<svg xmlns="{SVG}" viewBox="0 0 24 24">{body}</svg>"#).into_bytes()
    }
    #[test]
    fn white_icons_accept_geometry_and_inherited_paints_without_changing_bytes() {
        for body in [
            r##"<path fill="#fff" d="M1 2h20v20H1z"/>"##,
            r##"<g fill="none" stroke="#FFFFFF" transform="translate(1 2) rotate(45)"><circle cx="10" cy="10" r="5"/><path d="M1 1L20 20" stroke-linecap="round"/></g>"##,
            r#"<line stroke="white" x1="0" y1="0" x2="24" y2="24"/>"#,
        ] {
            let bytes = svg(body);
            let original = bytes.clone();
            assert!(validate_icon(&bytes).is_ok());
            assert_eq!(bytes, original);
        }
    }
    #[test]
    fn icons_reject_active_content_external_references_nonwhite_paints_and_bounds() {
        for body in [
            r#"<script>alert(1)</script>"#,
            r#"<style>*{fill:white}</style>"#,
            r#"<path fill="white" onclick="alert(1)" d="M1 1"/>"#,
            r#"<image href="https://example.com/a.svg"/>"#,
            r##"<use href="#x"/>"##,
            r#"<foreignObject/>"#,
            r#"<path style="fill:white" d="M1 1"/>"#,
            r#"<path fill="red" d="M1 1"/>"#,
            r#"<path d="M1 1"/>"#,
            r#"<path fill="url(https://example.com)" d="M1 1"/>"#,
            r#"<path fill="white" transform="translate(NaN)" d="M1 1"/>"#,
            r#"<path fill="white" stroke-width="-1" d="M1 1"/>"#,
            r#"<path fill="white" opacity="2" d="M1 1"/>"#,
            r#"<path fill="white" d="&amp;"/>"#,
            r#"<?xml-stylesheet href="a"?><path fill="white" d="M1 1"/>"#,
            r#"<svg><path fill="white" d="M1 1"/></svg>"#,
        ] {
            assert!(validate_icon(&svg(body)).is_err(), "{body}");
        }
        assert!(
            validate_icon(
                br#"<!DOCTYPE svg [<!ENTITY x "x">]><svg xmlns="http://www.w3.org/2000/svg"/>"#
            )
            .is_err()
        );
        assert!(validate_icon(&vec![b' '; MAX_ICON_BYTES + 1]).is_err());
        assert!(
            validate_icon(&svg(&format!(
                "{}<path fill=\"white\" d=\"M1 1\"/>{}",
                "<g>".repeat(16),
                "</g>".repeat(16)
            )))
            .is_err()
        );
        assert!(validate_icon(&svg(&r#"<path fill="white" d="M1 1"/>"#.repeat(512))).is_err());
    }
}
