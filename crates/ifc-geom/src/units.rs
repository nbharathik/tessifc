// SPDX-License-Identifier: Apache-2.0
//! What the numbers in the file mean: everything downstream is metres and
//! radians. A project declaring DEGREE writes `90.` where RADIAN writes `1.5708`.

use tessifc_model::{Entity, Model};

/// The scale factors that turn file numbers into metres and radians.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Units {
    /// Multiply a length in file units by this to get metres.
    pub length_to_m: f64,
    /// Multiply an angle in file units by this to get radians.
    pub angle_to_rad: f64,
    /// True when the file said nothing and these are assumptions.
    pub assumed: bool,
}

impl Default for Units {
    /// Metres and radians, which is what the standard says to assume.
    fn default() -> Self {
        Units {
            length_to_m: 1.0,
            angle_to_rad: 1.0,
            assumed: true,
        }
    }
}

impl Units {
    /// Read the unit assignment from the project.
    ///
    /// No project or no units yields the default with [`Units::assumed`] set.
    pub fn from_model(model: &Model) -> Units {
        let mut units = Units::default();
        let Some(project) = model.entities_of_type("IfcProject").next() else {
            return units;
        };
        let Some(assignment) = project.attr("UnitsInContext").as_entity() else {
            return units;
        };
        let Some(list) = assignment.attr("Units").as_list() else {
            return units;
        };

        for value in list {
            let Some(unit) = value.as_entity() else {
                continue;
            };
            let Some(kind) = unit_type_of(unit) else {
                continue;
            };
            let Some(factor) = scale_of(unit).and_then(usable_scale) else {
                continue;
            };
            match kind.as_str() {
                "LENGTHUNIT" => {
                    units.length_to_m = factor;
                    units.assumed = false;
                }
                "PLANEANGLEUNIT" => units.angle_to_rad = factor,
                _ => {}
            }
        }
        units
    }

    /// A length from the file, in metres.
    #[inline]
    pub fn length(&self, value: f64) -> f64 {
        value * self.length_to_m
    }

    /// An angle from the file, in radians.
    #[inline]
    pub fn angle(&self, value: f64) -> f64 {
        value * self.angle_to_rad
    }

    /// True when the project measures angles in degrees.
    pub fn angles_are_degrees(&self) -> bool {
        (self.angle_to_rad - std::f64::consts::PI / 180.0).abs() < 1e-9
    }
}

/// The `UnitType` enumeration of a named unit, if it has one.
fn unit_type_of(unit: Entity<'_>) -> Option<String> {
    let value = unit.attr("UnitType");
    value
        .as_text()
        .map(|text| String::from_utf8_lossy(text.raw()).into_owned())
}

/// A cyclic `IfcConversionBasedUnit` chain must not recurse without end.
const MAX_UNIT_DEPTH: usize = 16;

/// The factor that converts this unit to its SI base.
fn scale_of(unit: Entity<'_>) -> Option<f64> {
    scale_of_at(unit, 0)
}

fn scale_of_at(unit: Entity<'_>, depth: usize) -> Option<f64> {
    if depth >= MAX_UNIT_DEPTH {
        return None;
    }
    if unit.is_a("IfcSIUnit") {
        let prefix = unit
            .attr("Prefix")
            .as_text()
            .map(|text| si_prefix(&String::from_utf8_lossy(text.raw())))
            .unwrap_or(1.0);
        return Some(prefix);
    }
    if unit.is_a("IfcConversionBasedUnit") {
        // ConversionFactor is a number plus the unit it is in, which may itself convert.
        let measure = unit.attr("ConversionFactor").as_entity()?;
        let value = measure.attr("ValueComponent").as_f64()?;
        let base = measure
            .attr("UnitComponent")
            .as_entity()
            .and_then(|component| scale_of_at(component, depth + 1))
            .unwrap_or(1.0);
        return Some(value * base);
    }
    None
}

/// A scale factor the rest of the pipeline can divide and multiply by.
///
/// File numbers can be zero, negative, infinite or NaN; those are dropped.
fn usable_scale(factor: f64) -> Option<f64> {
    (factor.is_finite() && factor > 0.0 && factor < 1e12).then_some(factor)
}

/// The multiplier for an SI prefix name.
fn si_prefix(name: &str) -> f64 {
    match name.to_ascii_uppercase().as_str() {
        "EXA" => 1e18,
        "PETA" => 1e15,
        "TERA" => 1e12,
        "GIGA" => 1e9,
        "MEGA" => 1e6,
        "KILO" => 1e3,
        "HECTO" => 1e2,
        "DECA" => 1e1,
        "DECI" => 1e-1,
        "CENTI" => 1e-2,
        "MILLI" => 1e-3,
        "MICRO" => 1e-6,
        "NANO" => 1e-9,
        "PICO" => 1e-12,
        "FEMTO" => 1e-15,
        "ATTO" => 1e-18,
        _ => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessifc_step::{ParseOptions, parse};

    fn model_of(data: &str) -> Model {
        let source = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{data}ENDSEC;\n"
        );
        Model::new(parse(source.as_bytes(), &ParseOptions::default()))
    }

    #[test]
    fn a_cyclic_conversion_unit_terminates() {
        // The conversion factor points back at its own unit.
        let model = model_of(
            "#1=IFCCONVERSIONBASEDUNIT(#4,.LENGTHUNIT.,'loop',#2);
             #2=IFCMEASUREWITHUNIT(IFCRATIOMEASURE(2.0),#1);
             #3=IFCUNITASSIGNMENT((#1));
             #4=IFCDIMENSIONALEXPONENTS(1,0,0,0,0,0,0);
             #5=IFCPROJECT('g',$,'P',$,$,$,$,$,#3);
",
        );
        let units = Units::from_model(&model);
        assert!(units.length_to_m.is_finite());
    }

    #[test]
    fn a_nonsense_scale_factor_is_ignored() {
        // A zero length unit would divide every coordinate by zero.
        let model = model_of(
            "#1=IFCCONVERSIONBASEDUNIT(#4,.LENGTHUNIT.,'zero',#2);
             #2=IFCMEASUREWITHUNIT(IFCRATIOMEASURE(0.0),#5);
             #3=IFCUNITASSIGNMENT((#1));
             #4=IFCDIMENSIONALEXPONENTS(1,0,0,0,0,0,0);
             #5=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
             #6=IFCPROJECT('g',$,'P',$,$,$,$,$,#3);
",
        );
        let units = Units::from_model(&model);
        assert!(units.length_to_m > 0.0);
    }

    #[test]
    fn millimetres_and_radians() {
        let model = model_of(
            "#1=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);\n\
             #2=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);\n\
             #3=IFCUNITASSIGNMENT((#1,#2));\n\
             #4=IFCPROJECT('g',$,'P',$,$,$,$,$,#3);\n",
        );
        let units = Units::from_model(&model);
        assert_eq!(units.length_to_m, 1e-3);
        assert_eq!(units.angle_to_rad, 1.0);
        assert!(!units.assumed);
        assert!((units.length(3500.0) - 3.5).abs() < 1e-12);
    }

    #[test]
    fn metres_when_there_is_no_prefix() {
        let model = model_of(
            "#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n\
             #3=IFCUNITASSIGNMENT((#1));\n\
             #4=IFCPROJECT('g',$,'P',$,$,$,$,$,#3);\n",
        );
        assert_eq!(Units::from_model(&model).length_to_m, 1.0);
    }

    #[test]
    fn degrees_are_detected() {
        // A project in degrees puts 90 where one in radians puts 1.5708.
        let model = model_of(
            "#1=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);\n\
             #2=IFCMEASUREWITHUNIT(IFCPLANEANGLEMEASURE(0.0174532925199433),#1);\n\
             #3=IFCCONVERSIONBASEDUNIT(#9,.PLANEANGLEUNIT.,'DEGREE',#2);\n\
             #4=IFCUNITASSIGNMENT((#3));\n\
             #5=IFCPROJECT('g',$,'P',$,$,$,$,$,#4);\n",
        );
        let units = Units::from_model(&model);
        assert!(units.angles_are_degrees(), "got {}", units.angle_to_rad);
        assert!((units.angle(90.0) - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
    }

    #[test]
    fn feet_convert_through_their_base_unit() {
        let model = model_of(
            "#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n\
             #2=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(0.3048),#1);\n\
             #3=IFCCONVERSIONBASEDUNIT(#9,.LENGTHUNIT.,'FOOT',#2);\n\
             #4=IFCUNITASSIGNMENT((#3));\n\
             #5=IFCPROJECT('g',$,'P',$,$,$,$,$,#4);\n",
        );
        let units = Units::from_model(&model);
        assert!((units.length_to_m - 0.3048).abs() < 1e-12);
        assert!((units.length(10.0) - 3.048).abs() < 1e-12);
    }

    #[test]
    fn a_file_with_no_project_says_it_assumed() {
        let model = model_of("#1=IFCWALL('w',$,$,$,$,$,$,$,$);\n");
        let units = Units::from_model(&model);
        assert!(units.assumed);
        assert_eq!(units.length_to_m, 1.0);
    }

    #[test]
    fn a_project_with_no_units_says_it_assumed() {
        let model = model_of("#4=IFCPROJECT('g',$,'P',$,$,$,$,$,$);\n");
        assert!(Units::from_model(&model).assumed);
    }

    #[test]
    fn every_si_prefix_is_known() {
        assert_eq!(si_prefix("MILLI"), 1e-3);
        assert_eq!(si_prefix("CENTI"), 1e-2);
        assert_eq!(si_prefix("KILO"), 1e3);
        assert_eq!(si_prefix("micro"), 1e-6);
        // An unknown prefix must not silently scale anything.
        assert_eq!(si_prefix("WHATEVER"), 1.0);
    }
}
