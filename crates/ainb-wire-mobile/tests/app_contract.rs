//! The app's five latch strings (`apps/ainb-mobile/src/wire/types.ts`,
//! `RepairLatch` and `HostNotice`) must be the crate's `LATCH_VALUES`,
//! verbatim. The app keeps no copy of the meaning, so a drift here is a
//! phone showing the wrong state. Skips when the app is not in this tree
//! (it lands in its own lane); runs on the merged tree.

use ainb_wire_mobile::pairing::LATCH_VALUES;

#[test]
fn the_apps_five_latch_strings_are_the_crates_constants() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/ainb-mobile/src/wire/types.ts");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("skipped: {} is not in this tree", path.display());
        return;
    };
    let line = |name: &str| {
        text.lines()
            .find(|l| l.contains(&format!("export type {name} =")))
            .unwrap_or_else(|| panic!("{name} is not declared in types.ts"))
            .to_owned()
    };
    let quoted =
        |l: &str| -> Vec<String> { l.split('"').skip(1).step_by(2).map(str::to_owned).collect() };
    let mut app: Vec<String> = quoted(&line("RepairLatch"));
    app.extend(quoted(&line("HostNotice")));
    let mut app_sorted = app.clone();
    app_sorted.sort();
    let mut ours: Vec<String> = LATCH_VALUES.iter().map(|s| (*s).to_owned()).collect();
    ours.sort();
    assert_eq!(
        app_sorted, ours,
        "the app's latch strings drifted from LATCH_VALUES"
    );
    assert_eq!(app.len(), 5);
}
