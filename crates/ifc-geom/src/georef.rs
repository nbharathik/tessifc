// SPDX-License-Identifier: Apache-2.0
//! Georeferencing: `IfcMapConversion` and its target CRS, read as metadata.
//!
//! Nothing here moves geometry; the pack carries it for a consumer to apply.

use crate::units::Units;
use tessifc_model::{Entity, Model};

/// The file's map conversion as a JSON object, or `None` without one.
///
/// Lengths are in metres; the axis vector and the scale are ratios. The
/// `projected_crs` member repeats the target CRS's own attributes.
pub fn georef_json(model: &Model, units: &Units) -> Option<String> {
    let conversion = model.entities_of_type("IfcMapConversion").next()?;
    let length = |name: &str| conversion.attr(name).as_f64().map(|v| units.length(v));
    let ratio = |name: &str| conversion.attr(name).as_f64();
    let mut members = vec![
        format!("\"eastings\":{}", number(length("Eastings")?)),
        format!("\"northings\":{}", number(length("Northings")?)),
        format!(
            "\"orthogonal_height\":{}",
            number(length("OrthogonalHeight")?)
        ),
    ];
    if let (Some(x), Some(y)) = (ratio("XAxisAbscissa"), ratio("XAxisOrdinate")) {
        members.push(format!("\"x_axis\":[{},{}]", number(x), number(y)));
    }
    if let Some(scale) = ratio("Scale") {
        members.push(format!("\"scale\":{}", number(scale)));
    }
    let mut json = format!("{{\"map_conversion\":{{{}}}", members.join(","));
    if let Some(crs) = conversion.attr("TargetCRS").as_entity() {
        json.push_str(&format!(",\"projected_crs\":{}", crs_json(crs)));
    }
    json.push('}');
    Some(json)
}

fn crs_json(crs: Entity<'_>) -> String {
    let text = |name: &str| {
        crs.attr(name)
            .as_text()
            .map(|t| escape(&String::from_utf8_lossy(t.raw())))
    };
    let mut members = Vec::new();
    for name in [
        "Name",
        "Description",
        "GeodeticDatum",
        "VerticalDatum",
        "MapProjection",
        "MapZone",
    ] {
        if let Some(value) = text(name) {
            members.push(format!("\"{}\":\"{}\"", snake(name), value));
        }
    }
    if let Some(unit) = crs.attr("MapUnit").as_entity()
        && let Some(name) = unit.attr("Name").as_text()
    {
        members.push(format!(
            "\"map_unit\":\"{}\"",
            escape(&String::from_utf8_lossy(name.raw()))
        ));
    }
    format!("{{{}}}", members.join(","))
}

fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn number(value: f64) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "null".to_string()
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_map_conversion_is_carried_as_metadata() {
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCPROJECTEDCRS('EPSG:25832','ETRS89 / UTM 32N','ETRS89','DHHN2016','TM','32',$);\n",
            "#2=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-5,$,$);\n",
            "#3=IFCMAPCONVERSION(#2,#1,393000.5,5730000.25,42.,0.6,0.8,1.);\n",
        ));
        let units = Units::from_model(&model);
        let json = georef_json(&model, &units).expect("a conversion is present");
        assert!(json.contains("\"eastings\":393000.5"), "{json}");
        assert!(json.contains("\"x_axis\":[0.6,0.8]"), "{json}");
        assert!(json.contains("\"name\":\"EPSG:25832\""), "{json}");
        assert!(json.contains("\"map_zone\":\"32\""), "{json}");
        assert!(
            serde_json_like_balanced(&json),
            "the object closes what it opens: {json}"
        );
    }

    #[test]
    fn a_model_without_a_conversion_has_no_georef() {
        let model = crate::eval::tests::model_of("#1=IFCCARTESIANPOINT((0.,0.,0.));\n");
        let units = Units::from_model(&model);
        assert!(georef_json(&model, &units).is_none());
    }

    fn serde_json_like_balanced(json: &str) -> bool {
        let mut depth = 0i32;
        let mut in_string = false;
        let mut escaped = false;
        for ch in json.chars() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ => {}
            }
            if depth < 0 {
                return false;
            }
        }
        depth == 0 && !in_string
    }
}
