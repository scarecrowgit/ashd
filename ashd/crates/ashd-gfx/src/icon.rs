//! Icons from `ashd/assets/icons/*.svg`: 24×24 grid, stroke 1.6, round caps/joins,
//! colour = `currentColor` (stroke `#000` in the files), fills only on indicators
//! (battery level). A tiny parser for exactly that subset — `path`, `circle`, `ellipse`,
//! `rect` (+`rx`), `line`, `polyline`, `polygon`; `fill`/`stroke`/`stroke-width` on the
//! root and per element. Anything after the first `</svg>` is ignored.

use std::f64::consts::PI;

use anyhow::{Context as _, bail};
use vello::kurbo::{BezPath, Circle, Ellipse, Point, Rect, RoundedRect, Shape as _};

/// One drawable part of an icon.
#[derive(Clone, Debug)]
pub struct IconPart {
    pub path: BezPath,
    pub fill: bool,
    /// Stroke width in grid units; `None` = no stroke.
    pub stroke: Option<f64>,
}

/// A parsed icon in its own viewBox units (normally 24).
#[derive(Clone, Debug)]
pub struct Icon {
    pub view_box: f64,
    pub parts: Vec<IconPart>,
}

#[derive(Clone, Copy)]
struct Paint {
    fill: bool,
    stroke: Option<f64>,
}

fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = attrs;
    while let Some(i) = rest.find(name) {
        let before_ok = i == 0 || rest.as_bytes()[i - 1].is_ascii_whitespace();
        let after = &rest[i + name.len()..];
        let after_trim = after.trim_start();
        if before_ok && after_trim.starts_with('=') {
            let v = after_trim[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let end = v[1..].find(q)?;
                return Some(&v[1..1 + end]);
            }
        }
        rest = &rest[i + name.len()..];
    }
    None
}

fn num(attrs: &str, name: &str) -> f64 {
    attr(attrs, name).and_then(|v| v.trim().trim_end_matches("px").parse().ok()).unwrap_or(0.0)
}

fn points(attrs: &str, close: bool) -> anyhow::Result<BezPath> {
    let raw = attr(attrs, "points").context("polyline/polygon without points")?;
    let vals: Vec<f64> =
        raw.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).map(str::parse).collect::<Result<_, _>>()?;
    let mut p = BezPath::new();
    for (i, xy) in vals.chunks_exact(2).enumerate() {
        let pt = Point::new(xy[0], xy[1]);
        if i == 0 { p.move_to(pt) } else { p.line_to(pt) }
    }
    if close {
        p.close_path();
    }
    Ok(p)
}

fn paint(attrs: &str, inherited: Paint) -> Paint {
    let mut p = inherited;
    if let Some(f) = attr(attrs, "fill") {
        p.fill = f != "none";
    }
    if let Some(s) = attr(attrs, "stroke") {
        if s == "none" {
            p.stroke = None;
        } else if p.stroke.is_none() {
            p.stroke = Some(1.0);
        }
    }
    if let Some(w) = attr(attrs, "stroke-width").and_then(|w| w.parse::<f64>().ok())
        && p.stroke.is_some()
    {
        p.stroke = Some(w);
    }
    p
}

impl Icon {
    /// Parse one of the ashd icon files.
    pub fn parse(svg: &str) -> anyhow::Result<Icon> {
        let start = svg.find("<svg").context("no <svg> element")?;
        let svg = &svg[start..];
        let end = svg.find("</svg>").unwrap_or(svg.len());
        let svg = &svg[..end];
        let root_end = svg.find('>').context("unterminated <svg>")?;
        let root = &svg[4..root_end];
        let view_box = attr(root, "viewBox")
            .and_then(|v| v.split_whitespace().nth(2).and_then(|w| w.parse().ok()))
            .unwrap_or(24.0);
        // SVG defaults: fill black, no stroke; the root of our icons sets fill none + stroke.
        let root_paint = paint(root, Paint { fill: true, stroke: None });
        let mut parts = Vec::new();
        let mut rest = &svg[root_end + 1..];
        while let Some(lt) = rest.find('<') {
            rest = &rest[lt + 1..];
            let gt = rest.find('>').context("unterminated element")?;
            let tag = &rest[..gt];
            rest = &rest[gt + 1..];
            if tag.starts_with('/') || tag.starts_with('!') || tag.starts_with('?') {
                continue;
            }
            let tag = tag.trim_end_matches('/');
            let (name, attrs) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
            let path = match name {
                "path" => BezPath::from_svg(attr(attrs, "d").context("path without d")?)
                    .map_err(|e| anyhow::anyhow!("bad path data: {e:?}"))?,
                "circle" => {
                    Circle::new((num(attrs, "cx"), num(attrs, "cy")), num(attrs, "r")).to_path(0.01)
                }
                "ellipse" => Ellipse::new((num(attrs, "cx"), num(attrs, "cy")), (num(attrs, "rx"), num(attrs, "ry")), 0.0)
                    .to_path(0.01),
                "rect" => {
                    let (x, y) = (num(attrs, "x"), num(attrs, "y"));
                    let r = Rect::new(x, y, x + num(attrs, "width"), y + num(attrs, "height"));
                    let rx = attr(attrs, "rx").or(attr(attrs, "ry")).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
                    if rx > 0.0 { RoundedRect::from_rect(r, rx).to_path(0.01) } else { r.to_path(0.01) }
                }
                "line" => {
                    let mut p = BezPath::new();
                    p.move_to((num(attrs, "x1"), num(attrs, "y1")));
                    p.line_to((num(attrs, "x2"), num(attrs, "y2")));
                    p
                }
                "polyline" => points(attrs, false)?,
                "polygon" => points(attrs, true)?,
                "g" | "title" | "desc" | "defs" => continue,
                other => bail!("unsupported icon element <{other}>"),
            };
            let p = paint(attrs, root_paint);
            if p.fill || p.stroke.is_some() {
                parts.push(IconPart { path, fill: p.fill, stroke: p.stroke });
            }
        }
        if parts.is_empty() {
            bail!("icon has no drawable elements");
        }
        Ok(Icon { view_box, parts })
    }

    /// Parse an embedded icon; panics on a broken asset (a build-time bug).
    pub fn embedded(name: &str, svg: &str) -> Icon {
        Icon::parse(svg).unwrap_or_else(|e| panic!("icon {name}: {e:#}"))
    }
}

/// An arc path (for spinners): centre, radius, start angle and sweep in radians.
pub fn arc_path(center: Point, radius: f64, start: f64, sweep: f64) -> BezPath {
    vello::kurbo::Arc::new(center, (radius, radius), start, sweep.clamp(-2.0 * PI, 2.0 * PI), 0.0).to_path(0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="7" width="18" height="10" rx="2"></rect><line x1="22" y1="11" x2="22" y2="13"></line><rect x="4.5" y="9.5" width="10" height="5" fill="#000" stroke="none"></rect></svg>"##;

    #[test]
    fn parses_battery_with_fill_override() {
        let icon = Icon::parse(BAT).unwrap();
        assert_eq!(icon.view_box, 24.0);
        assert_eq!(icon.parts.len(), 3);
        assert!(!icon.parts[0].fill && icon.parts[0].stroke == Some(1.6));
        assert!(icon.parts[2].fill && icon.parts[2].stroke.is_none());
    }

    #[test]
    fn parses_every_asset_icon() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icons");
        let mut n = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "svg") {
                let src = std::fs::read_to_string(&path).unwrap();
                Icon::parse(&src).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
                n += 1;
            }
        }
        assert!(n > 10);
    }

    #[test]
    fn attr_does_not_match_suffixes() {
        assert_eq!(attr(r#"stroke-width="1.6" width="24""#, "width"), Some("24"));
        assert_eq!(attr(r#"stroke="none" fill="x""#, "stroke"), Some("none"));
    }
}
