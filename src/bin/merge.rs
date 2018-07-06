extern crate crater_report_merge;
extern crate serde_json;

use crater_report_merge::{Comparison, TestResult, TestResults};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::process;

fn main() -> Result<(), Box<std::error::Error>> {
    let mut args = env::args_os();
    let result_a = args.next();
    let result_b = args.next();

    let (result_a, result_b) = if let (Some(a), Some(b)) = (result_a, result_b) {
        (a, b)
    } else {
        eprintln!("Please provide the two result (JSON) files as arguments");
        process::exit(1);
    };

    let mut result_a: TestResults = serde_json::from_slice(&fs::read(result_a)?)?;
    let result_b: TestResults = serde_json::from_slice(&fs::read(result_b)?)?;

    let mut master_map = HashMap::new();
    for (idx, res) in result_b.crates.iter().enumerate() {
        assert!(master_map.insert(res.name.clone(), idx).is_none());
    }

    for res in &mut result_a.crates {
        if !master_map.contains_key(&res.name) {
            // skip
            continue;
        }
        let master_res = &result_b.crates[master_map[&res.name]];
        if let Some(result) = &master_res.runs[0] {
            res.runs[0] = Some(result.clone());
            let cmp = match (result.res, res.runs[1].clone().unwrap().res) {
                (TestResult::BuildFail, TestResult::BuildFail) => Comparison::SameBuildFail,
                (TestResult::TestFail, TestResult::TestFail) => Comparison::SameTestFail,
                (TestResult::TestSkipped, TestResult::TestSkipped) => Comparison::SameTestSkipped,
                (TestResult::TestPass, TestResult::TestPass) => Comparison::SameTestPass,
                (TestResult::BuildFail, TestResult::TestFail)
                | (TestResult::BuildFail, TestResult::TestSkipped)
                | (TestResult::BuildFail, TestResult::TestPass)
                | (TestResult::TestFail, TestResult::TestPass) => Comparison::Fixed,
                (TestResult::TestPass, TestResult::TestFail)
                | (TestResult::TestPass, TestResult::BuildFail)
                | (TestResult::TestSkipped, TestResult::BuildFail)
                | (TestResult::TestFail, TestResult::BuildFail) => Comparison::Regressed,
                (TestResult::TestFail, TestResult::TestSkipped)
                | (TestResult::TestPass, TestResult::TestSkipped)
                | (TestResult::TestSkipped, TestResult::TestFail)
                | (TestResult::TestSkipped, TestResult::TestPass) => {
                    panic!("can't compare");
                }
            };
            res.res = cmp;
        }

        if let Some(run) = &mut res.runs[0] {
            run.log = format!(
                "https://cargobomb-reports.s3.amazonaws.com/pr-51762/{}",
                run.log
            );
        }
        if let Some(run) = &mut res.runs[1] {
            run.log = format!(
                "https://cargobomb-reports.s3.amazonaws.com/result_a-1/{}",
                run.log
            );
        }
    }

    println!("result_a crates: {}", result_a.crates.len());
    println!("result_b crates: {}", result_b.crates.len());

    std::io::stdout().write(&serde_json::to_vec(&result_a)?)?;

    Ok(())
}
