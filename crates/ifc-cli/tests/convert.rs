// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "schema-ifc4")]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "tessifc-cli-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(
            directory.join("input.ifc"),
            concat!(
                "ISO-10303-21;HEADER;FILE_SCHEMA(('IFC4'));ENDSEC;DATA;",
                "#1=IFCCIRCLEPROFILEDEF(.AREA.,$,$,1.);",
                "#2=IFCDIRECTION((0.,0.,1.));",
                "#3=IFCEXTRUDEDAREASOLID(#1,$,#2,2.);",
                "#4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));",
                "#5=IFCPRODUCTDEFINITIONSHAPE($,$,(#4));",
                "#6=IFCWALL('wall',$,$,$,$,$,#5,$,$);",
                "ENDSEC;END-ISO-10303-21;"
            ),
        )
        .unwrap();
        Self(directory)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tessifc"))
            .arg("convert")
            .arg(self.0.join("input.ifc"))
            .args(["--json", "--jobs", "1", "--output"])
            .arg(self.0.join("output.igp"))
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for file in ["input.ifc", "output.igp"] {
            let _ = std::fs::remove_file(self.0.join(file));
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}

#[test]
fn strict_rejection_preserves_existing_output() {
    let fixture = Fixture::new();
    let output = fixture.run(&["--circle-segments", "8"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["output_written"], true);
    assert!(
        report["diagnostics"]["by_code"]["W_TESSELLATION_TOLERANCE_UNMET"]
            .as_u64()
            .unwrap()
            > 0
    );

    std::fs::write(fixture.0.join("output.igp"), b"previous output").unwrap();
    let output = fixture.run(&["--circle-segments", "8", "--strict"]);
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["output_written"], false);
    assert_eq!(
        std::fs::read(fixture.0.join("output.igp")).unwrap(),
        b"previous output"
    );
}

#[test]
fn flags_override_json_and_effective_settings_are_reported() {
    let fixture = Fixture::new();
    let output = fixture.run(&[
        "--settings",
        r#"{"chordToleranceM":0.02,"weld":false}"#,
        "--chord-tolerance-m",
        "0.005",
        "--strict",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["effective_settings"]["chordToleranceM"], 0.005);
    assert_eq!(report["effective_settings"]["weld"], false);
    assert_eq!(report["products_with_geometry"], 1);
    assert_eq!(report["product_outcomes"][0]["state"], "emitted");
}

#[test]
fn malformed_settings_fail_before_writing_a_pack() {
    let fixture = Fixture::new();
    for settings in [
        "{",
        "[]",
        r#"{"chordToleranceM":0}"#,
        r#"{"circleSegments":4294967297}"#,
        r#"{"maxSurfaceVertices":0}"#,
        r#"{"maxSurfaceVertices":1048577}"#,
        r#"{"repairSurfaceCurves":1}"#,
    ] {
        let output = fixture.run(&["--settings", settings]);
        assert_eq!(output.status.code(), Some(2), "accepted {settings}");
        assert!(!fixture.0.join("output.igp").exists());
    }
}

#[test]
fn surface_curve_recovery_is_explicit_and_strict_refuses_the_repaired_pack() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.join("input.ifc"),
        concat!(
            "ISO-10303-21;HEADER;FILE_SCHEMA(('IFC4'));ENDSEC;DATA;",
            "#1=IFCCARTESIANPOINT((0.,0.,0.));#2=IFCAXIS2PLACEMENT3D(#1,$,$);",
            "#3=IFCPLANE(#2);#4=IFCCARTESIANPOINT((1.,0.,0.));",
            "#5=IFCCARTESIANPOINT((0.,1.,0.));#6=IFCCARTESIANPOINT((0.,0.));",
            "#7=IFCPOLYLINE((#6,#6));#8=IFCPCURVE(#3,#7);",
            "#9=IFCPOLYLINE((#1,#4));#10=IFCSURFACECURVE(#9,(#8),.PCURVE_S1.);",
            "#11=IFCVERTEXPOINT(#1);#12=IFCVERTEXPOINT(#4);#13=IFCVERTEXPOINT(#5);",
            "#14=IFCEDGECURVE(#11,#12,#10,.T.);#15=IFCPOLYLINE((#4,#5));",
            "#16=IFCPOLYLINE((#5,#1));#17=IFCEDGECURVE(#12,#13,#15,.T.);",
            "#18=IFCEDGECURVE(#13,#11,#16,.T.);#19=IFCORIENTEDEDGE(*,*,#14,.T.);",
            "#20=IFCORIENTEDEDGE(*,*,#17,.T.);#21=IFCORIENTEDEDGE(*,*,#18,.T.);",
            "#22=IFCEDGELOOP((#19,#20,#21));#23=IFCFACEOUTERBOUND(#22,.T.);",
            "#24=IFCADVANCEDFACE((#23),#3,.T.);#25=IFCOPENSHELL((#24));",
            "#26=IFCSHELLBASEDSURFACEMODEL((#25));",
            "#27=IFCSHAPEREPRESENTATION($,'Body','SurfaceModel',(#26));",
            "#28=IFCPRODUCTDEFINITIONSHAPE($,$,(#27));",
            "#29=IFCBUILDINGELEMENTPROXY('repair',$,$,$,$,$,#28,$,$);",
            "ENDSEC;END-ISO-10303-21;"
        ),
    )
    .unwrap();
    let default = fixture.run(&[]);
    let report: serde_json::Value = serde_json::from_slice(&default.stdout).unwrap();
    assert_eq!(report["products_with_geometry"], 0);

    let args = ["--settings", r#"{"repairSurfaceCurves":true}"#];
    let recovered = fixture.run(&args);
    assert!(recovered.status.success());
    let report: serde_json::Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(report["products_with_geometry"], 1);
    assert_eq!(report["diagnostics"]["by_code"]["W_PCURVE_3D_FALLBACK"], 1);

    let previous = std::fs::read(fixture.0.join("output.igp")).unwrap();
    let strict = fixture.run(&[args[0], args[1], "--strict"]);
    assert_eq!(strict.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&strict.stdout).unwrap();
    assert_eq!(report["output_written"], false);
    assert_eq!(
        std::fs::read(fixture.0.join("output.igp")).unwrap(),
        previous
    );
}
